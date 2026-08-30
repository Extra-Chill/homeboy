//! Core-owned transport-neutral runner execution service.
//!
//! Callers submit one canonical execution envelope here. Optional runner
//! implementations register the machine/transport behavior at composition
//! startup; core and its callers do not depend on those implementations.

use crate::error::{Error, Result};
use homeboy_runner_contract::{RunnerExecutionEnvelope, RunnerExecutionRecord};

pub trait RunnerExecutionProvider: Send + Sync {
    fn submit(&self, request: RunnerExecutionEnvelope) -> Result<RunnerExecutionRecord>;
}

struct NoopRunnerExecutionProvider;

impl RunnerExecutionProvider for NoopRunnerExecutionProvider {
    fn submit(&self, _request: RunnerExecutionEnvelope) -> Result<RunnerExecutionRecord> {
        Err(Error::validation_invalid_argument(
            "runner",
            "no runner execution provider is registered",
            None,
            Some(vec![
                "Install and register a runner implementation before submitting work.".to_string(),
            ]),
        ))
    }
}

homeboy_engine_primitives::provider_registry_arc! {
    provider: dyn RunnerExecutionProvider,
    noop: NoopRunnerExecutionProvider,
    /// Register the process runner implementation at composition startup.
    register: pub fn register_runner_execution_provider,
    active: fn active_runner_execution_provider,
}

/// Submit one transport-neutral execution request to the registered runner.
pub fn submit(request: RunnerExecutionEnvelope) -> Result<RunnerExecutionRecord> {
    active_runner_execution_provider().submit(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_fails_closed_without_an_execution_provider() {
        let error = submit(RunnerExecutionEnvelope::planned("exec-1", "test"))
            .expect_err("an uncomposed core must not execute work");

        assert_eq!(error.details["field"], "runner");
        assert!(error.message.contains("no runner execution provider"));
    }
}
