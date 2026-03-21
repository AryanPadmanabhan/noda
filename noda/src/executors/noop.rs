use super::{ActivationOutcome, ExecutionContext, Executor};
use anyhow::Result;
use std::{future::Future, pin::Pin};

pub(super) struct NoopExecutor;

impl Executor for NoopExecutor {
    fn install<'a>(
        &'a self,
        _ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { Ok(()) })
    }

    fn activate<'a>(
        &'a self,
        _ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move { Ok(ActivationOutcome::Complete) })
    }
}
