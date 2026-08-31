use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RunnerApiOperationFailure, RunnerApiVersion, RunnerExecutionEnvelope};

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
    pub workspace_claim_binding: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease: Option<Value>,
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
    use crate::{RunnerApiOperationFailureCode, RUNNER_API_V1};

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
