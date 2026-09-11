//! Cross-process runner execution-context vocabulary.

use serde::{Deserialize, Serialize};

pub const RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA: &str = "homeboy/runner-job-execution-context/v1";
pub const RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY: &str = "runner-job-execution-context";
pub const RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION: u32 = 2;

/// The worker capability negotiated before a claim can carry an execution
/// context assertion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionProtocol {
    pub capability: String,
    pub version: u32,
}

impl RunnerJobExecutionProtocol {
    pub fn current() -> Self {
        Self {
            capability: RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY.to_string(),
            version: RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION,
        }
    }

    pub fn is_supported(&self) -> bool {
        self.capability == RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY
            && (1..=RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION).contains(&self.version)
    }

    pub fn uses_envelope_only_claim(&self) -> bool {
        self.version >= 2
    }
}

/// A transport assertion of a durable runner-job execution context.
///
/// This value is not live authority. The implementation that owns the durable
/// claim must verify it before provider invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionContextAssertion {
    pub schema: String,
    pub id: String,
    pub controller_run_id: String,
    pub controller_attempt_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub accepted_handoff_id: String,
    pub runtime_id: String,
    pub claim_ref: String,
    pub dispatch_receipt: String,
    pub verification: RunnerJobExecutionVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionVerification {
    pub state: String,
    pub verified_at_ms: u64,
}

/// Set while a hosted exec runs inside a runner rather than the local host.
pub const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

/// Private marker added only when an exec crosses a remote runner boundary.
pub const RUNNER_PLACEMENT_RESOLVED_ENV: &str = "HOMEBOY_RUNNER_PLACEMENT_RESOLVED";

/// Identifies the runner an exec is bound to.
pub const RUNNER_ID_ENV: &str = "HOMEBOY_RUNNER_ID";

/// Whether an environment variable is a private runner control marker.
pub fn is_internal_control_env(name: &str) -> bool {
    name == RUNNER_PLACEMENT_RESOLVED_ENV
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_context_vocabulary_and_classification_stay_stable() {
        assert_eq!(RUNNER_HOSTED_EXEC_ENV, "HOMEBOY_RUNNER_HOSTED_EXEC");
        assert_eq!(
            RUNNER_PLACEMENT_RESOLVED_ENV,
            "HOMEBOY_RUNNER_PLACEMENT_RESOLVED"
        );
        assert_eq!(RUNNER_ID_ENV, "HOMEBOY_RUNNER_ID");

        assert!(is_internal_control_env(RUNNER_PLACEMENT_RESOLVED_ENV));
        assert!(!is_internal_control_env(RUNNER_HOSTED_EXEC_ENV));
        assert!(!is_internal_control_env(RUNNER_ID_ENV));
        assert!(!is_internal_control_env("HOMEBOY_LAB_EXECUTION_RUNNER_ID"));
    }
}
