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
