use crate::types::ReleaseManifest;
use anyhow::{anyhow, Context, Result};
use std::{fs, path::PathBuf, process::Command};

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub artifact_path: PathBuf,
    pub current_slot: String,
    pub next_slot: String,
    pub manifest: ReleaseManifest,
    pub release_version: String,
    pub state_dir: PathBuf,
}

pub trait Executor: Send + Sync {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
}

pub fn build(name: &str) -> Box<dyn Executor> {
    match name {
        "scripted" => Box::new(ScriptedExecutor),
        "grub-ab" => Box::new(GrubAbExecutor),
        _ => Box::new(NoopExecutor),
    }
}

struct NoopExecutor;
impl Executor for NoopExecutor {
    fn install<'a>(&'a self, _ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }
    fn activate<'a>(&'a self, _ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }
}

struct ScriptedExecutor;
impl Executor for ScriptedExecutor {
    fn install<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cmd) = &ctx.manifest.install.install_command {
                run_shell(cmd)?;
            }
            Ok(())
        })
    }
    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cmd) = &ctx.manifest.activation.activate_command {
                run_shell(cmd)?;
            }
            Ok(())
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
    fn activate<'a>(&'a self, ctx: &'a ExecutionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Pragmatic default: if the user supplied an activate command, run it.
            // Otherwise, store a file representing the next boot target. This is enough
            // for demos and can be swapped for real grub-editenv integration.
            if let Some(cmd) = &ctx.manifest.activation.activate_command {
                run_shell(cmd)?;
            } else {
                fs::write(ctx.state_dir.join("next-boot-slot"), &ctx.next_slot)?;
            }
            Ok(())
        })
    }
}

fn run_shell(command: &str) -> Result<()> {
    let status = Command::new("sh").arg("-lc").arg(command).status()?;
    if !status.success() {
        return Err(anyhow!("command failed: {command}"));
    }
    Ok(())
}
