use homeboy_control_plane_contract::{AttemptId, ExecutionId, MissionId, RunId, TaskId};
use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};

pub const EXECUTE_CAPABILITY_ID: &str = "execute";
pub const EXTENSION_API_EXECUTE_REQUEST_SCHEMA: &str = "homeboy/extension-api-execute-request/v1";
pub const EXTENSION_API_EXECUTE_RESPONSE_SCHEMA: &str = "homeboy/extension-api-execute-response/v1";
pub const EXTENSION_API_EXECUTE_IDEMPOTENCY_KEY_MAX_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiExecutionMode {
    Interactive,
    Captured,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiExecuteState {
    Completed,
    Replayed,
    InProgress,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiExecuteInput {
    pub id: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiExecuteStepFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiExecuteOutput {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

/// Canonical orchestration identity attached by a control-plane caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiControlPlaneIdentity {
    pub mission: MissionId,
    pub run: RunId,
    pub task: TaskId,
    pub attempt: AttemptId,
    pub attempt_number: u32,
    pub execution: ExecutionId,
}

/// Invoke the advertised `execute` capability without cwd, command, or secret authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiExecuteRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    pub extension_id: String,
    pub capability_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane: Option<ExtensionApiControlPlaneIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ExtensionApiExecuteInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    pub mode: ExtensionApiExecutionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_filter: Option<ExtensionApiExecuteStepFilter>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionApiExecuteResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane: Option<ExtensionApiControlPlaneIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ExtensionApiExecuteOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ExtensionApiExecuteState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::v1::EXTENSION_API_V1;

    fn sample_request() -> ExtensionApiExecuteRequest {
        ExtensionApiExecuteRequest {
            schema: EXTENSION_API_EXECUTE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: "fixture".to_string(),
            capability_id: EXECUTE_CAPABILITY_ID.to_string(),
            project_id: Some("site".to_string()),
            component_id: Some("app".to_string()),
            control_plane: Some(ExtensionApiControlPlaneIdentity {
                mission: MissionId::new("mission-1").unwrap(),
                run: RunId::new("run-1").unwrap(),
                task: TaskId::new("task-1").unwrap(),
                attempt: AttemptId::new("attempt-1").unwrap(),
                attempt_number: 1,
                execution: ExecutionId::new("execution-1").unwrap(),
            }),
            inputs: vec![ExtensionApiExecuteInput {
                id: "target".to_string(),
                value: "prod".to_string(),
            }],
            argv: vec!["--flag".to_string()],
            mode: ExtensionApiExecutionMode::Captured,
            step_filter: Some(ExtensionApiExecuteStepFilter {
                step: Some("test".to_string()),
                skip: Some("lint".to_string()),
            }),
            idempotency_key: "run-1".to_string(),
        }
    }

    #[test]
    fn execute_request_wire_shape_is_versioned_and_omits_implementation_authority() {
        assert_eq!(
            serde_json::to_value(sample_request()).expect("request JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_EXECUTE_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture",
                "capability_id": "execute",
                "project_id": "site",
                "component_id": "app",
                "control_plane": {
                    "mission": "mission-1",
                    "run": "run-1",
                    "task": "task-1",
                    "attempt": "attempt-1",
                    "attempt_number": 1,
                    "execution": "execution-1"
                },
                "inputs": [{ "id": "target", "value": "prod" }],
                "argv": ["--flag"],
                "mode": "captured",
                "step_filter": { "step": "test", "skip": "lint" },
                "idempotency_key": "run-1"
            })
        );
    }

    #[test]
    fn execute_response_wire_shape_is_typed() {
        let response = ExtensionApiExecuteResponse {
            schema: EXTENSION_API_EXECUTE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: Some("fixture".to_string()),
            project_id: Some("site".to_string()),
            control_plane: sample_request().control_plane,
            exit_code: Some(0),
            output: Some(ExtensionApiExecuteOutput {
                stdout: "ok".to_string(),
                stderr: String::new(),
            }),
            state: Some(ExtensionApiExecuteState::Completed),
            failure: None,
        };

        assert_eq!(
            serde_json::to_value(response).expect("response JSON"),
            serde_json::json!({
                "schema": EXTENSION_API_EXECUTE_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "extension_id": "fixture",
                "project_id": "site",
                "control_plane": {
                    "mission": "mission-1",
                    "run": "run-1",
                    "task": "task-1",
                    "attempt": "attempt-1",
                    "attempt_number": 1,
                    "execution": "execution-1"
                },
                "exit_code": 0,
                "output": { "stdout": "ok" },
                "state": "completed"
            })
        );
    }

    #[test]
    fn execute_request_rejects_unknown_fields() {
        let error = serde_json::from_value::<ExtensionApiExecuteRequest>(serde_json::json!({
            "schema": EXTENSION_API_EXECUTE_REQUEST_SCHEMA,
            "api_version": { "major": 1 },
            "extension_id": "fixture",
            "capability_id": "execute",
            "mode": "captured",
            "idempotency_key": "run-1",
            "cwd": "/tmp",
            "command": "echo"
        }))
        .expect_err("unknown fields");
        let message = error.to_string();
        assert!(message.contains("cwd") || message.contains("unknown field"));
    }

    #[test]
    fn execute_conflict_and_in_progress_outcomes_are_typed() {
        for (state, code, wire_state, wire_code) in [
            (
                ExtensionApiExecuteState::InProgress,
                super::super::ExtensionApiOperationFailureCode::InvocationInProgress,
                "in_progress",
                "invocation_in_progress",
            ),
            (
                ExtensionApiExecuteState::Conflict,
                super::super::ExtensionApiOperationFailureCode::IdempotencyConflict,
                "conflict",
                "idempotency_conflict",
            ),
        ] {
            let response = ExtensionApiExecuteResponse {
                schema: EXTENSION_API_EXECUTE_RESPONSE_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: Some("fixture".to_string()),
                project_id: None,
                control_plane: None,
                exit_code: None,
                output: None,
                state: Some(state),
                failure: Some(super::super::ExtensionApiOperationFailure {
                    code,
                    message: "typed outcome".to_string(),
                }),
            };
            assert_eq!(
                serde_json::to_value(response).expect("outcome JSON"),
                serde_json::json!({
                    "schema": EXTENSION_API_EXECUTE_RESPONSE_SCHEMA,
                    "api_version": { "major": 1 },
                    "extension_id": "fixture",
                    "state": wire_state,
                    "failure": { "code": wire_code, "message": "typed outcome" }
                })
            );
        }
    }
}
