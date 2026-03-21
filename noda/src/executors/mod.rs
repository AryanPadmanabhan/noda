mod grub_ab;
mod nix_generation;
mod noop;
mod scripted;

use crate::types::{
    ArtifactSource, ExecutorSpec, GrubAbExecutorSpec, NixGenerationExecutorSpec, ReleaseManifest,
    ScriptedExecutorSpec,
};
use anyhow::{anyhow, Context, Result};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
};

pub use nix_generation::rollback_nix_generation;
pub use grub_ab::rollback_grub_ab;

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
    pub expected_active_slot: Option<String>,
    pub expected_root_device: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RollbackAction {
    NixGeneration { previous_system_path: String },
    GrubAb {
        authority_device: String,
        mountpoint: String,
        grubenv_relpath: String,
        previous_grub_menu_entry: String,
    },
}

pub trait Executor: Send + Sync {
    fn install<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
    fn activate<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>>;
}

pub fn build(spec: &ExecutorSpec) -> Box<dyn Executor> {
    match spec {
        ExecutorSpec::Scripted(_) => Box::new(scripted::ScriptedExecutor),
        ExecutorSpec::GrubAb(_) => Box::new(grub_ab::GrubAbExecutor),
        ExecutorSpec::NixGeneration(_) => Box::new(nix_generation::NixGenerationExecutor),
        ExecutorSpec::Noop => Box::new(noop::NoopExecutor),
    }
}

pub fn detect_current_slot(spec: &ExecutorSpec) -> Result<Option<String>> {
    match spec {
        ExecutorSpec::GrubAb(grub) if grub.slots.is_some() => {
            Ok(Some(grub_ab::detect_active_slot(grub)?))
        }
        ExecutorSpec::GrubAb(_) | ExecutorSpec::Noop | ExecutorSpec::Scripted(_) | ExecutorSpec::NixGeneration(_) => {
            Ok(None)
        }
    }
}

pub fn rollback_action(
    spec: &ExecutorSpec,
    current_slot: &str,
    previous_system_path: Option<String>,
) -> Result<Option<RollbackAction>> {
    match spec {
        ExecutorSpec::NixGeneration(_) => Ok(previous_system_path.map(|path| {
            RollbackAction::NixGeneration {
                previous_system_path: path,
            }
        })),
        ExecutorSpec::GrubAb(grub) if grub.slots.is_some() => {
            grub_ab::rollback_action(grub, current_slot).map(Some)
        }
        ExecutorSpec::GrubAb(_) => Ok(None),
        ExecutorSpec::Noop | ExecutorSpec::Scripted(_) => Ok(None),
    }
}

pub(crate) fn scripted_spec(ctx: &ExecutionContext) -> Result<&ScriptedExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::Scripted(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected scripted")),
    }
}

pub(crate) fn grub_ab_spec(ctx: &ExecutionContext) -> Result<&GrubAbExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::GrubAb(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected grub-ab")),
    }
}

pub(crate) fn nix_spec(ctx: &ExecutionContext) -> Result<&NixGenerationExecutorSpec> {
    match &ctx.manifest.executor {
        ExecutorSpec::NixGeneration(spec) => Ok(spec),
        _ => Err(anyhow!("executor mismatch: expected nix-generation")),
    }
}

pub(crate) fn shell_env(
    ctx: &ExecutionContext,
    extra: &[(&str, String)],
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        (
            "ARTIFACT_PATH".into(),
            ctx.artifact_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        ),
        (
            "ARTIFACT_URL".into(),
            artifact_source(&ctx.manifest.executor)
                .map(|artifact| artifact.url.clone())
                .unwrap_or_default(),
        ),
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

pub(crate) fn artifact_source(spec: &ExecutorSpec) -> Option<&ArtifactSource> {
    match spec {
        ExecutorSpec::Scripted(spec) => Some(&spec.artifact),
        ExecutorSpec::GrubAb(spec) => Some(&spec.artifact),
        ExecutorSpec::Noop | ExecutorSpec::NixGeneration(_) => None,
    }
}

pub(crate) fn artifact_path(ctx: &ExecutionContext) -> Option<&Path> {
    ctx.artifact_path.as_deref()
}

pub(crate) fn run_shell(command: &str, env: &BTreeMap<String, String>) -> Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd.envs(env);
    let status = cmd
        .status()
        .with_context(|| format!("running shell command: {command}"))?;
    if !status.success() {
        return Err(anyhow!("command failed: {command}"));
    }
    Ok(())
}
