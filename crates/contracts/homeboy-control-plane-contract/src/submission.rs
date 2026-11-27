//! Versioned request and acknowledgement for submitting an already prepared plan.
//!
//! The generic control-plane contract names only identities, queue-only intent,
//! and the canonical run resource. Agent, provider, model, and runner details
//! stay on adapter-owned prepared input.

use serde::{Deserialize, Serialize};

use crate::{ControlPlaneActionOutcome, ControlPlaneError, ControlPlaneRun, RunId};

pub const CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA: &str =
    "homeboy/control-plane-submission-request/v1";
pub const CONTROL_PLANE_SUBMISSION_ACKNOWLEDGEMENT_SCHEMA: &str =
    "homeboy/control-plane-submission-acknowledgement/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneSubmissionRequest {
    pub schema: String,
    pub idempotency_key: String,
    pub actor: String,
    pub run: RunId,
    #[serde(default)]
    pub queue_only: bool,
}

impl ControlPlaneSubmissionRequest {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if self.schema != CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA {
            return Err(ControlPlaneError::invalid_argument(format!(
                "control-plane submission request schema must be {CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA}"
            )));
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane submission request requires an idempotency key",
            ));
        }
        if self.actor.trim().is_empty() {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane submission request requires an actor",
            ));
        }
        if self.idempotency_key != self.run.as_str() {
            return Err(ControlPlaneError::invalid_argument(
                "control-plane submission idempotency key must equal the canonical run id",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneSubmissionAcknowledgement {
    pub schema: String,
    pub acknowledgement: String,
    pub run: RunId,
    pub idempotency_key: String,
    pub actor: String,
    pub accepted_at: String,
    pub outcome: ControlPlaneActionOutcome,
    pub queued: bool,
    pub resource: ControlPlaneRun,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_request_rejects_invalid_identity_metadata() {
        let mut request = ControlPlaneSubmissionRequest {
            schema: "homeboy/control-plane-submission-request/v2".to_string(),
            idempotency_key: "different".to_string(),
            actor: String::new(),
            run: RunId::new("run-1").expect("run"),
            queue_only: true,
        };
        assert!(request.validate().is_err());
        request.schema = CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA.to_string();
        assert!(request.validate().is_err());
        request.actor = "test".to_string();
        assert!(request.validate().is_err());
        request.idempotency_key = "run-1".to_string();
        request.validate().expect("valid request");
    }
}
