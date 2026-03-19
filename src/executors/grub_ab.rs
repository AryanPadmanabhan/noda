use super::{artifact_path, run_shell, shell_env, ActivationOutcome, ExecutionContext, Executor};
use anyhow::{Context, Result};
use std::{fs, future::Future, pin::Pin};

pub(super) struct GrubAbExecutor;

impl Executor for GrubAbExecutor {
    fn install<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let slots_dir = ctx.state_dir.join("slots");
            fs::create_dir_all(&slots_dir)?;
            let source_artifact =
                artifact_path(ctx).context("grub-ab requires a downloaded artifact path")?;
            let dest = slots_dir.join(format!("slot-{}-{}", ctx.next_slot, ctx.release_version));
            fs::copy(source_artifact, &dest)
                .with_context(|| format!("copying artifact into inactive slot {:?}", dest))?;
            Ok(())
        })
    }

    fn activate<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::grub_ab_spec(ctx)?;
            if let Some(command) = &spec.activate_command {
                run_shell(command, &shell_env(ctx, &[]))?;
            } else {
                fs::write(ctx.state_dir.join("next-boot-slot"), &ctx.next_slot)?;
            }
            Ok(ActivationOutcome::Complete)
        })
    }
}
