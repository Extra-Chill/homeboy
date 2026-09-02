use serde::{Deserialize, Serialize};

use crate::{
    RunnerApiOperationFailure, RunnerApiVersion, RunnerExecutionEnvelope, WorkspaceClaimBinding,
    WorkspaceOwnerLease,
};

pub const RUNNER_API_SUBMIT_REQUEST_SCHEMA: &str = "homeboy/runner-api-submit-request/v1";
pub const RUNNER_API_SUBMIT_RESPONSE_SCHEMA: &str = "homeboy/runner-api-submit-response/v1";

/// The transport-neutral admission request for one canonical runner execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiSubmitRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub submission_key: String,
    pub envelope: RunnerExecutionEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_binding: Option<WorkspaceClaimBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease: Option<WorkspaceOwnerLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiSubmitResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub outcome: RunnerApiSubmitOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunnerApiSubmitOutcome {
    Accepted { job_id: String, job_status: String },
    Rejected { failure: RunnerApiOperationFailure },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RunnerApiOperationFailureCode, WorkspaceClaim, WorkspaceClaimProtocol, WorkspaceIdentity,
        WorkspaceOwnerLeaseProtocol, RUNNER_API_V1, WORKSPACE_CLAIM_SCHEMA,
        WORKSPACE_OWNER_LEASE_SCHEMA,
    };

    #[test]
    fn submit_request_has_a_strict_versioned_wire_shape() {
        let request = RunnerApiSubmitRequest {
            schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            submission_key: "agent-task:v1:lab:run-1".to_string(),
            envelope: RunnerExecutionEnvelope::planned("run-1", "test"),
            workspace_claim_binding: None,
            workspace_owner_lease: None,
        };
        let value = serde_json::to_value(&request).expect("submit request JSON");
        assert_eq!(value["schema"], RUNNER_API_SUBMIT_REQUEST_SCHEMA);
        assert!(serde_json::from_value::<RunnerApiSubmitRequest>(value).is_ok());
        assert!(
            serde_json::from_value::<RunnerApiSubmitRequest>(serde_json::json!({
                "schema": RUNNER_API_SUBMIT_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "submission_key": "one",
                "envelope": RunnerExecutionEnvelope::planned("one", "test"),
                "unexpected": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn submit_request_authority_sidecars_have_canonical_wire_shapes() {
        let workspace = WorkspaceIdentity::new("managed-workspace", "repo@task").unwrap();
        let request = RunnerApiSubmitRequest {
            schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            submission_key: "agent-task:v1:lab:run-1".to_string(),
            envelope: RunnerExecutionEnvelope::planned("run-1", "test"),
            workspace_claim_binding: Some(WorkspaceClaimBinding {
                workspace: workspace.clone(),
                lifecycle_revision: 7,
                claim: Some(WorkspaceClaim {
                    schema: WORKSPACE_CLAIM_SCHEMA.to_string(),
                    protocol: WorkspaceClaimProtocol::current(),
                    workspace: workspace.clone(),
                    lifecycle_revision: 7,
                    token: "claim-token".to_string(),
                    expires_at_ms: 100,
                }),
            }),
            workspace_owner_lease: Some(crate::WorkspaceOwnerLease {
                schema: WORKSPACE_OWNER_LEASE_SCHEMA.to_string(),
                protocol: WorkspaceOwnerLeaseProtocol::current(),
                workspace,
                owner_id: "runner:one".to_string(),
                lifecycle_revision: 8,
                token: "lease-token".to_string(),
                expires_at_ms: 101,
            }),
        };

        let encoded = serde_json::to_value(&request).unwrap();
        let decoded: RunnerApiSubmitRequest = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(
            encoded["workspace_claim_binding"]["claim"]["protocol"],
            serde_json::json!({ "capability": "workspace-claim", "version": 2 })
        );
        assert_eq!(
            encoded["workspace_owner_lease"]["protocol"],
            serde_json::json!({ "capability": "workspace-owner-lease", "version": 2 })
        );
    }

    #[test]
    fn submit_outcomes_have_explicit_accepted_and_rejected_wire_shapes() {
        let accepted = RunnerApiSubmitResponse {
            schema: RUNNER_API_SUBMIT_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiSubmitOutcome::Accepted {
                job_id: "job-1".to_string(),
                job_status: "queued".to_string(),
            },
        };
        assert_eq!(
            serde_json::to_value(accepted).expect("accepted JSON"),
            serde_json::json!({
                "schema": RUNNER_API_SUBMIT_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "outcome": { "status": "accepted", "job_id": "job-1", "job_status": "queued" },
            })
        );
        let rejected = RunnerApiSubmitResponse {
            schema: RUNNER_API_SUBMIT_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiSubmitOutcome::Rejected {
                failure: RunnerApiOperationFailure {
                    code: RunnerApiOperationFailureCode::SubmissionRejected,
                    message: "payload drift".to_string(),
                },
            },
        };
        assert_eq!(
            serde_json::to_value(rejected).expect("rejected JSON"),
            serde_json::json!({
                "schema": RUNNER_API_SUBMIT_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "outcome": { "status": "rejected", "failure": { "code": "submission_rejected", "message": "payload drift" } },
            })
        );
    }
}
