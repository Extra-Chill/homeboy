use serde::{Deserialize, Serialize};

use crate::{
    RunnerApiVersion, RunnerExecutionEnvelope, WorkspaceClaimBinding, WorkspaceOwnerLease,
};

pub const RUNNER_API_SUBMIT_REQUEST_SCHEMA: &str = "homeboy/runner-api-submit-request/v1";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        WorkspaceClaim, WorkspaceClaimProtocol, WorkspaceIdentity, WorkspaceOwnerLeaseProtocol,
        RUNNER_API_V1, WORKSPACE_CLAIM_SCHEMA, WORKSPACE_OWNER_LEASE_SCHEMA,
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
}
