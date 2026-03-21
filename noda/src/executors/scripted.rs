use super::{run_shell, shell_env, ActivationOutcome, ExecutionContext, Executor};
use anyhow::Result;
use std::{future::Future, pin::Pin};

pub(super) struct ScriptedExecutor;

impl Executor for ScriptedExecutor {
    fn install<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::scripted_spec(ctx)?;
            run_shell(&spec.install_command, &shell_env(ctx, &[]))?;
            Ok(())
        })
    }

    fn activate<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::scripted_spec(ctx)?;
            if let Some(command) = &spec.activate_command {
                run_shell(command, &shell_env(ctx, &[]))?;
            }
            Ok(ActivationOutcome::Complete)
        })
    }
}
