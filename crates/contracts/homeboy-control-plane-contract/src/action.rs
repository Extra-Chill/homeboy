//! Versioned requests and acknowledgements for run mutations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ControlPlaneAction, ControlPlaneRun, RunId};

pub const CONTROL_PLANE_ACTION_REQUEST_SCHEMA: &str = "homeboy/control-plane-action-request/v1";
pub const CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA: &str =
    "homeboy/control-plane-action-acknowledgement/v1";
pub const CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA: &str =
    "homeboy/control-plane-empty-action-payload/v1";
pub const CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-cancel-parameters/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionPayload {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl ControlPlaneActionPayload {
    pub fn empty() -> Self {
        Self {
            schema: CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA.to_string(),
            data: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneCancelParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionRequest {
    pub schema: String,
    pub action: ControlPlaneAction,
    pub idempotency_key: String,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_updated_at: Option<String>,
    pub parameters: ControlPlaneActionPayload,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneActionOutcome {
    Succeeded,
    Failed,
    AlreadySatisfied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionAcknowledgement {
    pub schema: String,
    pub acknowledgement: String,
    pub run: RunId,
    pub action: ControlPlaneAction,
    pub idempotency_key: String,
    pub actor: String,
    pub accepted_at: String,
    pub completed_at: String,
    pub outcome: ControlPlaneActionOutcome,
    pub resource: ControlPlaneRun,
    pub result: ControlPlaneActionPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ControlPlaneRunState;

    #[test]
    fn action_request_and_acknowledgement_round_trip() {
        let run = RunId::new("run-1").expect("run");
        let request = ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: ControlPlaneAction::Cancel,
            idempotency_key: "request-1".to_string(),
            actor: "test".to_string(),
            expected_updated_at: Some("2026-01-01T00:00:00Z".to_string()),
            parameters: ControlPlaneActionPayload {
                schema: CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA.to_string(),
                data: serde_json::json!({ "reason": "stop" }),
            },
            confirmed: true,
        };
        assert_eq!(
            serde_json::from_value::<ControlPlaneActionRequest>(
                serde_json::to_value(&request).expect("serialize")
            )
            .expect("deserialize"),
            request
        );

        let mut resource = ControlPlaneRun::new(run.clone());
        resource.state = ControlPlaneRunState::Cancelled;
        let acknowledgement = ControlPlaneActionAcknowledgement {
            schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
            acknowledgement: "run-1:action:cancel:request-1".to_string(),
            run,
            action: request.action,
            idempotency_key: request.idempotency_key,
            actor: request.actor,
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: "2026-01-01T00:00:01Z".to_string(),
            outcome: ControlPlaneActionOutcome::Succeeded,
            resource,
            result: ControlPlaneActionPayload::empty(),
            message: None,
        };
        let value = serde_json::to_value(&acknowledgement).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA);
        assert_eq!(value["outcome"], "succeeded");
    }
}
