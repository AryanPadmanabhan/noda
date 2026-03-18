use crate::types::{
    ArtifactSource, ExecutorSpec, GrubAbExecutorSpec, NixGenerationExecutorSpec, NixGenerationSource,
    ReleaseManifest, ScriptedExecutorSpec,
};
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
    pub artifact_path: Option<PathBuf>,
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
}

pub trait Executor: Send + Sync {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>>;
}

pub fn build(spec: &ExecutorSpec) -> Box<dyn Executor> {
    match spec {
        ExecutorSpec::Scripted(_) => Box::new(ScriptedExecutor),
        ExecutorSpec::GrubAb(_) => Box::new(GrubAbExecutor),
        ExecutorSpec::NixGeneration(_) => Box::new(NixGenerationExecutor),
        ExecutorSpec::Noop => Box::new(NoopExecutor),
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
            let spec = scripted_spec(ctx)?;
            run_shell(&spec.install_command, &shell_env(ctx, &[]))?;
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = scripted_spec(ctx)?;
            if let Some(cmd) = &spec.activate_command {
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
            let artifact_path = ctx
                .artifact_path
                .as_ref()
                .context("grub-ab requires a downloaded artifact path")?;
            let dest = slots_dir.join(format!("slot-{}-{}", ctx.next_slot, ctx.release_version));
            fs::copy(artifact_path, &dest).with_context(|| format!("copying artifact into inactive slot {:?}", dest))?;
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = grub_ab_spec(ctx)?;
            if let Some(cmd) = &spec.activate_command {
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
            let spec = nix_spec(ctx)?;
            let system_path = match &spec.source {
                NixGenerationSource::CopyFromStore { copy_from, store_path } => {
                    let status = Command::new("nix")
                        .args(["copy", "--from", copy_from, store_path])
                        .status()
                        .with_context(|| format!("running nix copy --from {copy_from} {store_path}"))?;
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

            save_nix_build_metadata(ctx, &NixBuildMetadata { system_path })?;
            Ok(())
        })
    }

    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = nix_spec(ctx)?;
            let metadata = load_nix_build_metadata(ctx)?;

            match &spec.source {
                NixGenerationSource::CopyFromStore { .. } => {
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
                }
                NixGenerationSource::BuildFlake { flake, flake_attr } => {
                    if let Some(config_name) = nixos_config_name(flake_attr) {
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
            }))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NixBuildMetadata {
    system_path: String,
}

fn scripted_spec(ctx: &ExecutionContext) -> Result<&ScriptedExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::Scripted(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected scripted")),
    }
}

fn grub_ab_spec(ctx: &ExecutionContext) -> Result<&GrubAbExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::GrubAb(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected grub-ab")),
    }
}

fn nix_spec(ctx: &ExecutionContext) -> Result<&NixGenerationExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::NixGeneration(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected nix-generation")),
    }
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
    let artifact_path = ctx
        .artifact_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let artifact_url = artifact_source(&ctx.manifest.executor)
        .map(|artifact| artifact.url.clone())
        .unwrap_or_default();
    let mut env = BTreeMap::from([
        ("ARTIFACT_PATH".into(), artifact_path),
        ("ARTIFACT_URL".into(), artifact_url),
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

fn artifact_source(spec: &ExecutorSpec) -> Option<&ArtifactSource> {
    match spec {
        ExecutorSpec::Scripted(spec) => Some(&spec.artifact),
        ExecutorSpec::GrubAb(spec) => Some(&spec.artifact),
        ExecutorSpec::Noop | ExecutorSpec::NixGeneration(_) => None,
    }
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
