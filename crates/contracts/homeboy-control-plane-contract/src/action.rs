//! Versioned requests and acknowledgements for run mutations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ControlPlaneAction, ControlPlaneRef, ControlPlaneRun, RunId};

pub const CONTROL_PLANE_ACTION_REQUEST_SCHEMA: &str = "homeboy/control-plane-action-request/v1";
pub const CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA: &str =
    "homeboy/control-plane-action-acknowledgement/v1";
pub const CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA: &str =
    "homeboy/control-plane-empty-action-payload/v1";
pub const CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-cancel-parameters/v1";
pub const CONTROL_PLANE_CANCEL_RESULT_SCHEMA: &str = "homeboy/control-plane-cancel-result/v1";
pub const CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA: &str = "homeboy/control-plane-retry-parameters/v1";
pub const CONTROL_PLANE_RETRY_RESULT_SCHEMA: &str = "homeboy/control-plane-retry-result/v1";
pub const CONTROL_PLANE_QUARANTINE_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-quarantine-parameters/v1";
pub const CONTROL_PLANE_QUARANTINE_RESULT_SCHEMA: &str =
    "homeboy/control-plane-quarantine-result/v1";
pub const CONTROL_PLANE_REARM_RESULT_SCHEMA: &str = "homeboy/control-plane-rearm-result/v1";
pub const CONTROL_PLANE_RESUME_RESULT_SCHEMA: &str = "homeboy/control-plane-resume-result/v1";
pub const CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-placement-update-parameters/v1";
pub const CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA: &str =
    "homeboy/control-plane-placement-update-result/v1";
pub const CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-promote-parameters/v1";
pub const CONTROL_PLANE_PROMOTE_RESULT_SCHEMA: &str = "homeboy/control-plane-promote-result/v1";
pub const CONTROL_PLANE_ACTION_INTENT_SCHEMA: &str = "homeboy/control-plane-action-intent/v1";
pub const CONTROL_PLANE_ACTION_FENCE_SCHEMA: &str = "homeboy/control-plane-action-fence/v1";
pub const CONTROL_PLANE_EFFECT_LEASE_SCHEMA: &str = "homeboy/control-plane-effect-lease/v1";
pub const CONTROL_PLANE_EFFECT_AUDIT_SCHEMA: &str = "homeboy/control-plane-effect-audit/v1";
pub const CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA: &str = "homeboy/control-plane-effect-terminal/v1";

/// Stable, caller-derived identity for one external effect. The same intent
/// must retain this value across restart and reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct EffectId(pub String);

/// Canonical durable resource plus the original external alias, when one was
/// supplied. Aliases are evidence, never a second authority for the resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionResource {
    pub resource: ControlPlaneRef,
    pub run: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_alias: Option<String>,
}

/// Immutable request persisted before any external action is attempted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionIntent {
    pub schema: String,
    pub effect_id: EffectId,
    pub resource: ControlPlaneActionResource,
    pub request: ControlPlaneActionRequest,
    pub request_digest: String,
    pub accepted_at: String,
}

/// Eligibility snapshot evaluated in the transaction that admits an intent.
/// A worker must treat a later resource change as a reconciliation boundary,
/// not silently execute against a different resource version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneActionFence {
    pub schema: String,
    pub resource_updated_at: String,
    pub eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneEffectState {
    Pending,
    Leased,
    Terminal,
}

/// Fenced, expiring ownership of an outbox effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEffectLease {
    pub schema: String,
    pub effect_id: EffectId,
    pub owner: String,
    pub fence: u64,
    pub expires_at: String,
}

/// Immutable evidence collected by the effect executor before terminalization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEffectAudit {
    pub schema: String,
    pub observed_at: String,
    pub evidence: Value,
}

/// Terminal record written atomically with the acknowledgement and audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEffectTerminal {
    pub schema: String,
    pub completed_at: String,
    pub acknowledgement: ControlPlaneActionAcknowledgement,
    pub audit: ControlPlaneEffectAudit,
}

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

/// The durable convergence observed after a cancellation request was accepted.
///
/// `Requested` means the request is durable, while terminalization was not
/// observed within the bounded reconciliation window (or that observation
/// failed). It is deliberately distinct from a failed cancellation action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneCancelDisposition {
    Cancelled,
    TerminalWithoutCancellation,
    DeferredForTerminalProvider,
    Requested,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneCancelResult {
    pub schema: String,
    pub disposition: ControlPlaneCancelDisposition,
    pub terminal: bool,
    pub wait_timeout_seconds: u64,
    pub waited_seconds: u64,
    pub poll_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRetryParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_run_id: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_route: Option<ControlPlaneProviderRouteOverride>,
}

/// Explicit provider route selected for a retry successor. It is part of the
/// immutable retry intent rather than an untracked execution-time override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneProviderRouteOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_provider_rotation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_rotations: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneQuarantineParameters {
    pub reason: String,
}

/// A deliberate execution-route change. The control plane accepts only explicit
/// local placement today, rather than silently broadening an automatic route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlanePlacementUpdateParameters {
    pub placement: String,
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

impl ControlPlaneActionRequest {
    pub fn validate(&self) -> Result<(), crate::ControlPlaneError> {
        const INPUT_BOUND: usize = 128;
        const REASON_BOUND: usize = 1_024;

        if self.schema != CONTROL_PLANE_ACTION_REQUEST_SCHEMA {
            return Err(crate::ControlPlaneError::invalid_argument(
                "unsupported control-plane action request schema",
            ));
        }
        for (name, value) in [
            ("idempotency_key", self.idempotency_key.as_str()),
            ("actor", self.actor.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > INPUT_BOUND {
                return Err(crate::ControlPlaneError::invalid_argument(format!(
                    "{name} must contain 1 to {INPUT_BOUND} bytes"
                )));
            }
        }
        let (name, expected_schema) = match self.action {
            ControlPlaneAction::Cancel => ("cancel", CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA),
            ControlPlaneAction::Promote => ("promote", CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA),
            ControlPlaneAction::Reconcile => {
                ("reconcile", CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA)
            }
            ControlPlaneAction::Resume => ("resume", CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA),
            ControlPlaneAction::PlacementUpdate => (
                "placement_update",
                CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA,
            ),
            ControlPlaneAction::Retry => ("retry", CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA),
            ControlPlaneAction::Quarantine => {
                ("quarantine", CONTROL_PLANE_QUARANTINE_PARAMETERS_SCHEMA)
            }
            ControlPlaneAction::Rearm => ("rearm", CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA),
        };
        if self.parameters.schema != expected_schema {
            return Err(crate::ControlPlaneError::invalid_argument(format!(
                "{name} requires parameters schema {expected_schema}"
            )));
        }
        if self.action == ControlPlaneAction::Cancel {
            let parameters: ControlPlaneCancelParameters =
                serde_json::from_value(self.parameters.data.clone()).map_err(|error| {
                    crate::ControlPlaneError::invalid_argument(format!(
                        "cancel parameters: {error}"
                    ))
                })?;
            if parameters
                .reason
                .as_ref()
                .is_some_and(|reason| reason.len() > REASON_BOUND)
            {
                return Err(crate::ControlPlaneError::invalid_argument(format!(
                    "reason exceeds {REASON_BOUND} bytes"
                )));
            }
        }
        if self.action == ControlPlaneAction::Retry {
            serde_json::from_value::<ControlPlaneRetryParameters>(self.parameters.data.clone())
                .map_err(|error| {
                    crate::ControlPlaneError::invalid_argument(format!("retry parameters: {error}"))
                })?;
        }
        if self.action == ControlPlaneAction::Quarantine {
            let parameters: ControlPlaneQuarantineParameters =
                serde_json::from_value(self.parameters.data.clone()).map_err(|error| {
                    crate::ControlPlaneError::invalid_argument(format!(
                        "quarantine parameters: {error}"
                    ))
                })?;
            if parameters.reason.trim().is_empty() || parameters.reason.len() > REASON_BOUND {
                return Err(crate::ControlPlaneError::invalid_argument(format!(
                    "quarantine reason must contain 1 to {REASON_BOUND} bytes"
                )));
            }
        }
        if self.action == ControlPlaneAction::PlacementUpdate {
            let parameters: ControlPlanePlacementUpdateParameters =
                serde_json::from_value(self.parameters.data.clone()).map_err(|error| {
                    crate::ControlPlaneError::invalid_argument(format!(
                        "placement update parameters: {error}"
                    ))
                })?;
            if parameters.placement != "local" {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "placement update currently requires explicit local placement",
                ));
            }
        }
        if matches!(
            self.action,
            ControlPlaneAction::Cancel
                | ControlPlaneAction::PlacementUpdate
                | ControlPlaneAction::Promote
                | ControlPlaneAction::Retry
                | ControlPlaneAction::Quarantine
                | ControlPlaneAction::Rearm
        ) && !self.confirmed
        {
            return Err(crate::ControlPlaneError::invalid_argument(format!(
                "{name} requires explicit confirmation"
            )));
        }
        Ok(())
    }
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

    #[test]
    fn cancel_result_is_versioned_and_distinguishes_unconverged_requests() {
        let result = ControlPlaneCancelResult {
            schema: CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
            disposition: ControlPlaneCancelDisposition::Requested,
            terminal: false,
            wait_timeout_seconds: 15,
            waited_seconds: 15,
            poll_count: 15,
            observation_error: Some("controller unavailable".to_string()),
        };
        let value = serde_json::to_value(&result).expect("serialize");
        assert_eq!(value["schema"], CONTROL_PLANE_CANCEL_RESULT_SCHEMA);
        assert_eq!(value["disposition"], "requested");
        assert_eq!(
            serde_json::from_value::<ControlPlaneCancelResult>(value).expect("deserialize"),
            result
        );
    }

    #[test]
    fn placement_update_requires_confirmation_and_explicit_local() {
        let mut request = ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: ControlPlaneAction::PlacementUpdate,
            idempotency_key: "placement-1".to_string(),
            actor: "operator".to_string(),
            expected_updated_at: None,
            parameters: ControlPlaneActionPayload {
                schema: CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA.to_string(),
                data: serde_json::json!({ "placement": "local" }),
            },
            confirmed: false,
        };
        assert!(request.validate().is_err());
        request.confirmed = true;
        assert!(request.validate().is_ok());
        request.parameters.data = serde_json::json!({ "placement": "auto" });
        assert!(request.validate().is_err());
    }

    #[test]
    fn quarantine_and_route_retry_require_typed_payloads() {
        let mut quarantine = ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: ControlPlaneAction::Quarantine,
            idempotency_key: "quarantine-1".to_string(),
            actor: "operator".to_string(),
            expected_updated_at: None,
            parameters: ControlPlaneActionPayload {
                schema: CONTROL_PLANE_QUARANTINE_PARAMETERS_SCHEMA.to_string(),
                data: serde_json::json!({ "reason": "provider unavailable" }),
            },
            confirmed: true,
        };
        assert!(quarantine.validate().is_ok());
        quarantine.parameters.data = serde_json::json!({ "reason": "" });
        assert!(quarantine.validate().is_err());

        let retry = ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: ControlPlaneAction::Retry,
            idempotency_key: "retry-1".to_string(),
            actor: "operator".to_string(),
            expected_updated_at: None,
            parameters: ControlPlaneActionPayload {
                schema: CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA.to_string(),
                data: serde_json::json!({
                    "force": false,
                    "provider_route": { "backend": "replacement" },
                }),
            },
            confirmed: true,
        };
        assert!(retry.validate().is_ok());
    }
}
