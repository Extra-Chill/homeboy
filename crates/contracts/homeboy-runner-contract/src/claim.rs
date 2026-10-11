use serde::{Deserialize, Serialize};

use crate::{
    RunnerApiOperationFailure, RunnerApiVersion, RunnerExecutionEnvelope,
    RunnerJobExecutionContextAssertion, RunnerJobExecutionProtocol, WorkspaceClaimBinding,
    WorkspaceClaimProtocol, WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol,
};

/// Reference to controller-owned credentials held only in broker memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerCredentialDeliveryDescriptor {
    pub delivery_id: String,
    pub env_names: Vec<String>,
    pub expires_at_ms: u64,
}

pub const RUNNER_API_CLAIM_REQUEST_SCHEMA: &str = "homeboy/runner-api-claim-request/v1";
pub const RUNNER_API_CLAIM_RESPONSE_SCHEMA: &str = "homeboy/runner-api-claim-response/v1";

/// The transport-neutral request to claim one runner execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiClaimRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_limit: Option<usize>,
    /// Every claim carries an authenticated execution context, so a worker
    /// must advertise the execution-context protocol it verifies.
    pub execution_protocol: RunnerJobExecutionProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_protocol: Option<WorkspaceClaimProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease_protocol: Option<WorkspaceOwnerLeaseProtocol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiClaimResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub outcome: RunnerApiClaimOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunnerApiClaimOutcome {
    Claimed { claim: RunnerApiClaimedExecution },
    Empty,
    Rejected { failure: RunnerApiOperationFailure },
}

/// The canonical, core-free execution projection returned for a claimed job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiClaimedExecution {
    pub job_id: String,
    pub claim_id: String,
    pub claim_expires_at_ms: u64,
    pub envelope: RunnerExecutionEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_binding: Option<WorkspaceClaimBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease: Option<WorkspaceOwnerLease>,
    pub execution_context: RunnerJobExecutionContextAssertion,
    pub execution_protocol: RunnerJobExecutionProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_protocol: Option<WorkspaceClaimProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease_protocol: Option<WorkspaceOwnerLeaseProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_delivery: Option<RunnerCredentialDeliveryDescriptor>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RunnerApiVersion, RunnerExecutionEnvelope, RunnerJobExecutionVerification, RUNNER_API_V1,
    };

    fn execution_context_assertion() -> RunnerJobExecutionContextAssertion {
        RunnerJobExecutionContextAssertion {
            schema: crate::RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA.to_string(),
            id: "context-1".to_string(),
            controller_run_id: "run-1".to_string(),
            controller_attempt_id: "attempt-1".to_string(),
            runner_id: "lab-1".to_string(),
            runner_job_id: "job-1".to_string(),
            accepted_handoff_id: "handoff-1".to_string(),
            runtime_id: "runtime-1".to_string(),
            claim_ref: "claim-1".to_string(),
            dispatch_receipt: "sha256:receipt".to_string(),
            verification: RunnerJobExecutionVerification {
                state: "verified".to_string(),
                verified_at_ms: 1,
            },
        }
    }

    #[test]
    fn claim_request_is_strict_and_versioned() {
        let request = RunnerApiClaimRequest {
            schema: RUNNER_API_CLAIM_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "lab-1".to_string(),
            project_id: None,
            lease_ms: Some(30_000),
            concurrency_limit: None,
            execution_protocol: RunnerJobExecutionProtocol::current(),
            workspace_claim_protocol: None,
            workspace_owner_lease_protocol: None,
        };
        let value = serde_json::to_value(&request).expect("claim request JSON");
        assert_eq!(value["schema"], RUNNER_API_CLAIM_REQUEST_SCHEMA);
        assert!(serde_json::from_value::<RunnerApiClaimRequest>(value.clone()).is_ok());
        let mut context_free = value;
        context_free
            .as_object_mut()
            .expect("claim request object")
            .remove("execution_protocol");
        let error = serde_json::from_value::<RunnerApiClaimRequest>(context_free)
            .expect_err("a claim without an execution protocol is refused");
        assert!(error.to_string().contains("execution_protocol"), "{error}");
        assert!(
            serde_json::from_value::<RunnerApiClaimRequest>(serde_json::json!({
                "schema": RUNNER_API_CLAIM_REQUEST_SCHEMA,
                "api_version": { "major": 1 }, "runner_id": "lab-1", "unexpected": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn claim_outcomes_pin_claimed_empty_and_rejected_wires() {
        let claimed = RunnerApiClaimResponse {
            schema: RUNNER_API_CLAIM_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiClaimOutcome::Claimed {
                claim: RunnerApiClaimedExecution {
                    job_id: "job-1".to_string(),
                    claim_id: "claim-1".to_string(),
                    claim_expires_at_ms: 10,
                    envelope: RunnerExecutionEnvelope::planned("run-1", "test"),
                    workspace_claim_binding: None,
                    workspace_owner_lease: None,
                    execution_context: execution_context_assertion(),
                    execution_protocol: RunnerJobExecutionProtocol::current(),
                    workspace_claim_protocol: None,
                    workspace_owner_lease_protocol: None,
                    credential_delivery: None,
                },
            },
        };
        let encoded = serde_json::to_value(claimed).expect("claimed JSON");
        assert_eq!(encoded["outcome"]["status"], "claimed");
        let mut with_request_projection = encoded["outcome"]["claim"].clone();
        with_request_projection["legacy_request"] = serde_json::json!({ "command": ["true"] });
        assert!(
            serde_json::from_value::<RunnerApiClaimedExecution>(with_request_projection).is_err()
        );
        let mut context_free = encoded["outcome"]["claim"].clone();
        context_free
            .as_object_mut()
            .expect("claim object")
            .remove("execution_context");
        assert!(serde_json::from_value::<RunnerApiClaimedExecution>(context_free).is_err());
        let empty = RunnerApiClaimResponse {
            schema: RUNNER_API_CLAIM_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiClaimOutcome::Empty,
        };
        assert_eq!(
            serde_json::to_value(empty).expect("empty JSON")["outcome"],
            serde_json::json!({ "status": "empty" })
        );
        let rejected = RunnerApiClaimResponse {
            schema: RUNNER_API_CLAIM_RESPONSE_SCHEMA.to_string(),
            api_version: RunnerApiVersion { major: 1 },
            outcome: RunnerApiClaimOutcome::Rejected {
                failure: RunnerApiOperationFailure {
                    code: crate::RunnerApiOperationFailureCode::SubmissionRejected,
                    message: "no".to_string(),
                },
            },
        };
        assert_eq!(
            serde_json::to_value(rejected).expect("rejected JSON")["outcome"]["status"],
            "rejected"
        );
    }
}
