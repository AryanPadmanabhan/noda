use super::{ActivationOutcome, ExecutionContext, Executor, PendingReboot};
use crate::types::NixGenerationSource;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
};

pub(super) struct NixGenerationExecutor;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NixBuildMetadata {
    system_path: String,
}

impl Executor for NixGenerationExecutor {
    fn install<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::nix_spec(ctx)?;
            let system_path = match &spec.source {
                NixGenerationSource::CopyFromStore {
                    copy_from,
                    store_path,
                } => {
                    let status = Command::new("nix")
                        .args(["copy", "--from", copy_from, store_path])
                        .status()
                        .with_context(|| {
                            format!("running nix copy --from {copy_from} {store_path}")
                        })?;
                    if !status.success() {
                        return Err(anyhow!("nix copy failed for {store_path} from {copy_from}"));
                    }
                    store_path.clone()
                }
                NixGenerationSource::BuildFlake { flake, flake_attr } => {
                    let target = format!("{flake}#{flake_attr}");
                    let output = Command::new("nix")
                        .args(["build", "--no-link", "--print-out-paths", &target])
                        .output()
                        .with_context(|| format!("running nix build for {target}"))?;
                    if !output.status.success() {
                        return Err(anyhow!(
                            "nix build failed: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                    parse_system_path(&String::from_utf8_lossy(&output.stdout))?
                }
            };

            save_build_metadata(ctx, &NixBuildMetadata { system_path })?;
            Ok(())
        })
    }

    fn activate<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::nix_spec(ctx)?;
            let metadata = load_build_metadata(ctx)?;

            match &spec.source {
                NixGenerationSource::CopyFromStore { .. } => {
                    let status = Command::new("nix-env")
                        .args([
                            "-p",
                            "/nix/var/nix/profiles/system",
                            "--set",
                            &metadata.system_path,
                        ])
                        .status()
                        .with_context(|| {
                            format!("registering system profile for {}", metadata.system_path)
                        })?;
                    if !status.success() {
                        return Err(anyhow!("nix-env --set failed for {}", metadata.system_path));
                    }
                    let switch_to_configuration =
                        Path::new(&metadata.system_path).join("bin/switch-to-configuration");
                    let status = Command::new(&switch_to_configuration)
                        .arg("boot")
                        .status()
                        .with_context(|| {
                            format!("running {}", switch_to_configuration.display())
                        })?;
                    if !status.success() {
                        return Err(anyhow!(
                            "switch-to-configuration boot failed for {}",
                            metadata.system_path
                        ));
                    }
                }
                NixGenerationSource::BuildFlake { flake, flake_attr } => {
                    if let Some(config_name) = nixos_config_name(flake_attr) {
                        let flake_target = format!("{flake}#{config_name}");
                        let status = Command::new("nixos-rebuild")
                            .args(["boot", "--flake", &flake_target])
                            .status()
                            .with_context(|| {
                                format!("running nixos-rebuild boot for {flake_target}")
                            })?;
                        if !status.success() {
                            return Err(anyhow!("nixos-rebuild boot failed for {flake_target}"));
                        }
                    } else {
                        let switch_to_configuration =
                            Path::new(&metadata.system_path).join("bin/switch-to-configuration");
                        let status = Command::new(&switch_to_configuration)
                            .arg("boot")
                            .status()
                            .with_context(|| {
                                format!("running {}", switch_to_configuration.display())
                            })?;
                        if !status.success() {
                            return Err(anyhow!(
                                "switch-to-configuration boot failed for {}",
                                metadata.system_path
                            ));
                        }
                    }
                }
            }

            let status = Command::new("systemctl")
                .arg("reboot")
                .status()
                .context("requesting reboot via systemctl")?;
            if !status.success() {
                return Err(anyhow!("systemctl reboot failed"));
            }

            Ok(ActivationOutcome::AwaitReboot(PendingReboot {
                expected_system_path: Some(metadata.system_path),
                expected_active_slot: None,
                expected_root_device: None,
            }))
        })
    }
}

pub fn rollback_nix_generation(previous_system_path: &str) -> Result<()> {
    let status = Command::new("nix-env")
        .args([
            "-p",
            "/nix/var/nix/profiles/system",
            "--set",
            previous_system_path,
        ])
        .status()
        .with_context(|| {
            format!("registering rollback system profile for {previous_system_path}")
        })?;
    if !status.success() {
        return Err(anyhow!(
            "nix-env --set failed for rollback target {previous_system_path}"
        ));
    }

    let switch_to_configuration =
        Path::new(previous_system_path).join("bin/switch-to-configuration");
    let status = Command::new(&switch_to_configuration)
        .arg("boot")
        .status()
        .with_context(|| format!("running {}", switch_to_configuration.display()))?;
    if !status.success() {
        return Err(anyhow!(
            "switch-to-configuration boot failed for rollback target {}",
            previous_system_path
        ));
    }

    let status = Command::new("systemctl")
        .arg("reboot")
        .status()
        .context("requesting rollback reboot via systemctl")?;
    if !status.success() {
        return Err(anyhow!("systemctl reboot failed during rollback"));
    }

    Ok(())
}

fn nixos_config_name(attr: &str) -> Option<String> {
    let trimmed = attr.strip_prefix("nixosConfigurations.")?;
    let (name, suffix) = trimmed.split_once('.')?;
    if suffix == "config.system.build.toplevel" {
        Some(name.to_string())
    } else {
        None
    }
}

fn metadata_path(ctx: &ExecutionContext) -> PathBuf {
    ctx.state_dir
        .join(format!("nix-generation-{}.json", ctx.command_id))
}

fn save_build_metadata(ctx: &ExecutionContext, metadata: &NixBuildMetadata) -> Result<()> {
    let raw = serde_json::to_string_pretty(metadata)?;
    fs::write(metadata_path(ctx), raw)?;
    Ok(())
}

fn load_build_metadata(ctx: &ExecutionContext) -> Result<NixBuildMetadata> {
    let raw = fs::read_to_string(metadata_path(ctx)).with_context(|| {
        format!(
            "reading nix-generation metadata for command {}",
            ctx.command_id
        )
    })?;
    Ok(serde_json::from_str(&raw)?)
}

fn parse_system_path(stdout: &str) -> Result<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .map(str::to_owned)
        .context("command did not emit a resulting system path")
}
