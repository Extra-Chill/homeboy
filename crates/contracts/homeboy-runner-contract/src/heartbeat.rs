use serde::{Deserialize, Serialize};

use crate::{
    RunnerApiOperationFailure, RunnerApiVersion, WorkspaceClaimBinding, WorkspaceOwnerLease,
};

pub const RUNNER_API_HEARTBEAT_REQUEST_SCHEMA: &str = "homeboy/runner-api-heartbeat-request/v1";
pub const RUNNER_API_HEARTBEAT_RESPONSE_SCHEMA: &str = "homeboy/runner-api-heartbeat-response/v1";

/// The transport-neutral request to renew one exact runner claim and its
/// workspace authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiHeartbeatRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    pub job_id: String,
    pub claim_id: String,
    pub lease_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_binding: Option<WorkspaceClaimBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease: Option<WorkspaceOwnerLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiHeartbeatResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub outcome: RunnerApiHeartbeatOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunnerApiHeartbeatOutcome {
    Renewed {
        claim_expires_at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_owner_lease: Option<WorkspaceOwnerLease>,
    },
    Rejected {
        failure: RunnerApiOperationFailure,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerApiOperationFailureCode, RUNNER_API_V1};

    #[test]
    fn heartbeat_wire_is_strict_and_pins_renewed_and_rejected_outcomes() {
        let request = RunnerApiHeartbeatRequest {
            schema: RUNNER_API_HEARTBEAT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "lab-1".to_string(),
            job_id: "job-1".to_string(),
            claim_id: "claim-1".to_string(),
            lease_ms: 30_000,
            workspace_claim_binding: None,
            workspace_owner_lease: None,
        };
        let encoded = serde_json::to_value(&request).expect("heartbeat request JSON");
        assert_eq!(
            encoded,
            serde_json::json!({
                "schema": RUNNER_API_HEARTBEAT_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "lab-1", "job_id": "job-1", "claim_id": "claim-1", "lease_ms": 30000,
            })
        );
        assert!(serde_json::from_value::<RunnerApiHeartbeatRequest>(encoded).is_ok());
        assert!(
            serde_json::from_value::<RunnerApiHeartbeatRequest>(serde_json::json!({
                "schema": RUNNER_API_HEARTBEAT_REQUEST_SCHEMA, "api_version": { "major": 1 },
                "runner_id": "lab-1", "job_id": "job-1", "claim_id": "claim-1", "lease_ms": 1,
                "unexpected": true,
            }))
            .is_err()
        );

        let renewed = RunnerApiHeartbeatResponse {
            schema: RUNNER_API_HEARTBEAT_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiHeartbeatOutcome::Renewed {
                claim_expires_at_ms: 10,
                workspace_owner_lease: None,
            },
        };
        assert_eq!(
            serde_json::to_value(renewed).expect("renewed JSON")["outcome"],
            serde_json::json!({
                "status": "renewed", "claim_expires_at_ms": 10
            })
        );
        let rejected = RunnerApiHeartbeatResponse {
            schema: RUNNER_API_HEARTBEAT_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            outcome: RunnerApiHeartbeatOutcome::Rejected {
                failure: RunnerApiOperationFailure {
                    code: RunnerApiOperationFailureCode::SubmissionRejected,
                    message: "stale claim".to_string(),
                },
            },
        };
        assert_eq!(
            serde_json::to_value(rejected).expect("rejected JSON")["outcome"]["status"],
            "rejected"
        );
    }
}
