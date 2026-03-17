use crate::types::{NixGenerationConfig, ReleaseManifest};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub command_id: String,
    pub artifact_path: PathBuf,
    pub current_slot: String,
    pub next_slot: String,
    pub manifest: ReleaseManifest,
    pub release_version: String,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum ActivationOutcome {
    Complete,
    AwaitReboot(PendingReboot),
}

#[derive(Debug, Clone)]
pub struct PendingReboot {
    pub expected_system_path: Option<String>,
    pub expected_hostname: Option<String>,
    pub validation_timeout_seconds: u64,
}

pub trait Executor: Send + Sync {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>>;
}

pub fn build(name: &str) -> Box<dyn Executor> {
    match name {
        "scripted" => Box::new(ScriptedExecutor),
        "grub-ab" => Box::new(GrubAbExecutor),
        "nix-generation" => Box::new(NixGenerationExecutor),
        _ => Box::new(NoopExecutor),
    }
}

struct NoopExecutor;
impl Executor for NoopExecutor {
    fn install<'a>(&'a self, _ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }

    fn activate<'a>(&'a self, _ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move { Ok(ActivationOutcome::Complete) })
    }
}

struct ScriptedExecutor;
impl Executor for ScriptedExecutor {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cmd) = &ctx.manifest.install.install_command {
                run_shell(cmd, &shell_env(ctx, &[]))?;
            }
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cmd) = &ctx.manifest.activation.activate_command {
                run_shell(cmd, &shell_env(ctx, &[]))?;
            }
            Ok(ActivationOutcome::Complete)
        })
    }
}

struct GrubAbExecutor;
impl Executor for GrubAbExecutor {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let slots_dir = ctx.state_dir.join("slots");
            fs::create_dir_all(&slots_dir)?;
            let dest = slots_dir.join(format!("slot-{}-{}", ctx.next_slot, ctx.release_version));
            fs::copy(&ctx.artifact_path, &dest).with_context(|| format!("copying artifact into inactive slot {:?}", dest))?;
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cmd) = &ctx.manifest.activation.activate_command {
                run_shell(cmd, &shell_env(ctx, &[]))?;
            } else {
                fs::write(ctx.state_dir.join("next-boot-slot"), &ctx.next_slot)?;
            }
            Ok(ActivationOutcome::Complete)
        })
    }
}

struct NixGenerationExecutor;
impl Executor for NixGenerationExecutor {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let cfg = nix_cfg(ctx)?;
            let flake_ref = flake_ref(ctx, cfg);
            let extra = [
                ("NIX_FLAKE_REF", flake_ref.clone()),
                ("NIX_FLAKE_ATTR", cfg.flake_attr.clone().unwrap_or_default()),
            ];

            let built_system = if let Some(store_path) = &cfg.store_path {
                let copy_from = cfg
                    .copy_from
                    .as_ref()
                    .context("missing install.nix_generation.copy_from for store-path import")?;
                let extra = [
                    ("NIX_COPY_FROM", copy_from.clone()),
                    ("NIX_STORE_PATH", store_path.clone()),
                ];
                let env = shell_env(ctx, &extra);
                if let Some(cmd) = &cfg.copy_command {
                    run_shell(cmd, &env)?;
                } else {
                    let status = Command::new("nix")
                        .args(["copy", "--from", copy_from, store_path])
                        .status()
                        .with_context(|| format!("running nix copy --from {copy_from} {store_path}"))?;
                    if !status.success() {
                        return Err(anyhow!("nix copy failed for {store_path} from {copy_from}"));
                    }
                }
                store_path.clone()
            } else if let Some(cmd) = &cfg.build_command {
                let stdout = run_shell_capture(cmd, &shell_env(ctx, &extra))?;
                parse_system_path(&stdout)?
            } else {
                let target = if let Some(attr) = &cfg.flake_attr {
                    format!("{flake_ref}#{attr}")
                } else {
                    flake_ref
                };
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
            };

            let metadata = NixBuildMetadata {
                system_path: cfg.expected_system_path.clone().unwrap_or(built_system),
            };
            save_nix_build_metadata(ctx, &metadata)?;
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let cfg = nix_cfg(ctx)?;
            let metadata = load_nix_build_metadata(ctx)?;
            let extra = [("EXPECTED_SYSTEM_PATH", metadata.system_path.clone())];
            let env = shell_env(ctx, &extra);

            if let Some(cmd) = &cfg.boot_command {
                run_shell(cmd, &env)?;
            } else if cfg.store_path.is_some() {
                let status = Command::new("nix-env")
                    .args(["-p", "/nix/var/nix/profiles/system", "--set", &metadata.system_path])
                    .status()
                    .with_context(|| format!("registering system profile for {}", metadata.system_path))?;
                if !status.success() {
                    return Err(anyhow!("nix-env --set failed for {}", metadata.system_path));
                }
                let switch_to_configuration = Path::new(&metadata.system_path).join("bin/switch-to-configuration");
                let status = Command::new(&switch_to_configuration)
                    .arg("boot")
                    .status()
                    .with_context(|| format!("running {}", switch_to_configuration.display()))?;
                if !status.success() {
                    return Err(anyhow!(
                        "switch-to-configuration boot failed for {}",
                        metadata.system_path
                    ));
                }
            } else if let Some(config_name) = nixos_config_name(cfg) {
                let flake = flake_ref(ctx, cfg);
                let flake_target = format!("{flake}#{config_name}");
                let status = Command::new("nixos-rebuild")
                    .args(["boot", "--flake", &flake_target])
                    .status()
                    .with_context(|| format!("running nixos-rebuild boot for {flake_target}"))?;
                if !status.success() {
                    return Err(anyhow!("nixos-rebuild boot failed for {flake_target}"));
                }
            } else {
                let switch_to_configuration = Path::new(&metadata.system_path).join("bin/switch-to-configuration");
                let status = Command::new(&switch_to_configuration)
                    .arg("boot")
                    .status()
                    .with_context(|| format!("running {}", switch_to_configuration.display()))?;
                if !status.success() {
                    return Err(anyhow!(
                        "switch-to-configuration boot failed for {}",
                        metadata.system_path
                    ));
                }
            }

            if let Some(cmd) = &cfg.reboot_command {
                run_shell(cmd, &env)?;
            } else {
                let status = Command::new("systemctl")
                    .arg("reboot")
                    .status()
                    .context("requesting reboot via systemctl")?;
                if !status.success() {
                    return Err(anyhow!("systemctl reboot failed"));
                }
            }

            Ok(ActivationOutcome::AwaitReboot(PendingReboot {
                expected_system_path: Some(metadata.system_path),
                expected_hostname: cfg.expected_hostname.clone(),
                validation_timeout_seconds: cfg.validation_timeout_seconds.unwrap_or(900),
            }))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NixBuildMetadata {
    system_path: String,
}

fn nix_cfg<'a>(ctx: &'a ExecutionContext) -> Result<&'a NixGenerationConfig> {
    ctx.manifest
        .install
        .nix_generation
        .as_ref()
        .context("missing install.nix_generation for nix-generation executor")
}

fn flake_ref(ctx: &ExecutionContext, cfg: &NixGenerationConfig) -> String {
    cfg.flake
        .clone()
        .or_else(|| cfg.source_path.clone())
        .unwrap_or_else(|| ctx.artifact_path.display().to_string())
}

fn nixos_config_name(cfg: &NixGenerationConfig) -> Option<String> {
    let attr = cfg.flake_attr.as_deref()?;
    let trimmed = attr.strip_prefix("nixosConfigurations.")?;
    let (name, suffix) = trimmed.split_once('.')?;
    if suffix == "config.system.build.toplevel" {
        Some(name.to_string())
    } else {
        None
    }
}

fn nix_build_metadata_path(ctx: &ExecutionContext) -> PathBuf {
    ctx.state_dir.join(format!("nix-generation-{}.json", ctx.command_id))
}

fn save_nix_build_metadata(ctx: &ExecutionContext, metadata: &NixBuildMetadata) -> Result<()> {
    let raw = serde_json::to_string_pretty(metadata)?;
    fs::write(nix_build_metadata_path(ctx), raw)?;
    Ok(())
}

fn load_nix_build_metadata(ctx: &ExecutionContext) -> Result<NixBuildMetadata> {
    let raw = fs::read_to_string(nix_build_metadata_path(ctx))
        .with_context(|| format!("reading nix-generation metadata for command {}", ctx.command_id))?;
    Ok(serde_json::from_str(&raw)?)
}

fn parse_system_path(stdout: &str) -> Result<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .map(|line| line.to_string())
        .context("command did not emit a resulting system path")
}

fn shell_env(ctx: &ExecutionContext, extra: &[(&str, String)]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("ARTIFACT_PATH".into(), ctx.artifact_path.display().to_string()),
        ("ARTIFACT_URL".into(), ctx.manifest.artifact.url.clone()),
        ("CURRENT_SLOT".into(), ctx.current_slot.clone()),
        ("NEXT_SLOT".into(), ctx.next_slot.clone()),
        ("RELEASE_VERSION".into(), ctx.release_version.clone()),
        ("STATE_DIR".into(), ctx.state_dir.display().to_string()),
    ]);
    for (key, value) in extra {
        env.insert((*key).into(), value.clone());
    }
    env
}

fn run_shell(command: &str, env: &BTreeMap<String, String>) -> Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd.envs(env);
    let status = cmd.status()?;
    if !status.success() {
        return Err(anyhow!("command failed: {command}"));
    }
    Ok(())
}

fn run_shell_capture(command: &str, env: &BTreeMap<String, String>) -> Result<String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd.envs(env);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "command failed: {command}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
