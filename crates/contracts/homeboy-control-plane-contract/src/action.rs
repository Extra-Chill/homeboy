//! Versioned requests and acknowledgements for run mutations.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

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
pub const CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-recover-parameters/v1";
pub const CONTROL_PLANE_RECOVER_RESULT_SCHEMA: &str = "homeboy/control-plane-recover-result/v1";

/// Recovery inputs; domain owners supply the typed gate and publication values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRecoverParameters<G = Value, P = Value, R = Value> {
    #[serde(default)]
    pub preflight: bool,
    #[serde(default)]
    pub regate: bool,
    #[serde(default)]
    pub unblock: bool,
    pub backend: Option<String>,
    pub selector: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub acknowledge_model_override: bool,
    pub timeout_ms: Option<u64>,
    pub candidate_ref: Option<String>,
    pub attempt: Option<u32>,
    pub adoption_model: Option<String>,
    #[serde(default)]
    pub replace_interrupted: bool,
    /// With `regate`, the commands that replace the candidate's declared
    /// verify commands for that re-gate; otherwise a pre-provider recipe
    /// gate correction.
    pub corrected_gates: Option<G>,
    pub publish: Option<P>,
    pub replace_gates: Option<R>,
    /// Per-gate wall-clock limit for a `regate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_timeout_seconds: Option<u64>,
    /// Per-gate no-progress window for a `regate`; `0` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_no_progress_timeout_seconds: Option<u64>,
}

pub const CONTROL_PLANE_ACTION_ID_BOUND: usize = 128;

/// Builds the durable effect identity for an action without coupling it to the
/// independently persisted idempotency key. Existing short identifiers retain
/// their wire representation; longer inputs use a deterministic digest of each
/// component so retries remain replayable and collision-resistant.
pub fn action_effect_id(
    provenance: &str,
    target_id: &str,
    action: &str,
    idempotency_key: &str,
) -> EffectId {
    let legacy = format!("{provenance}:{target_id}:{action}:{idempotency_key}");
    if legacy.len() <= CONTROL_PLANE_ACTION_ID_BOUND {
        return EffectId(legacy);
    }

    let mut digest = homeboy_engine_primitives::content_hash::sha256_hasher();
    for component in [provenance, target_id, action, idempotency_key] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    EffectId(format!("{provenance}:{action}:{:x}", digest.finalize()))
}
pub const CONTROL_PLANE_RESUME_RESULT_SCHEMA: &str = "homeboy/control-plane-resume-result/v1";
pub const CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA: &str =
    "homeboy/control-plane-placement-update-parameters/v1";
pub const CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA: &str =
    "homeboy/control-plane-placement-update-result/v1";
pub const CONTROL_PLANE_ACTION_INTENT_SCHEMA: &str = "homeboy/control-plane-action-intent/v1";
pub const CONTROL_PLANE_ACTION_FENCE_SCHEMA: &str = "homeboy/control-plane-action-fence/v1";
pub const CONTROL_PLANE_EFFECT_LEASE_SCHEMA: &str = "homeboy/control-plane-effect-lease/v1";
pub const CONTROL_PLANE_EFFECT_AUDIT_SCHEMA: &str = "homeboy/control-plane-effect-audit/v1";
pub const CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA: &str = "homeboy/control-plane-effect-terminal/v1";
pub const CONTROL_PLANE_EFFECT_STATUS_SCHEMA: &str = "homeboy/control-plane-effect-status/v1";

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

/// An authoritative effect observation. `Unknown` is intentionally distinct
/// from success: callers must reconcile an ambiguous external crash window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneEffectExecutionState {
    NotStarted,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneEffectStatus {
    pub schema: String,
    pub effect_id: EffectId,
    pub state: ControlPlaneEffectExecutionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgement: Option<ControlPlaneActionAcknowledgement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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

    /// Recover parameters with every field at its default.
    pub fn recover() -> Self {
        Self {
            schema: CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA.to_string(),
            data: serde_json::to_value(
                ControlPlaneRecoverParameters::<Value, Value, Value>::default(),
            )
            .expect("default recover parameters serialize"),
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
    /// Operator-authorized provider timeout for the successor, in
    /// milliseconds. Must exceed the source attempt's timeout. Mutually
    /// exclusive with `provider_route` and `force`. Additive within v1: older
    /// requests omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
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
    #[serde(default)]
    pub acknowledge_model_override: bool,
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
    /// Stable identity supplied by the caller and retained in the immutable
    /// intent. It is distinct from an idempotency key: status is addressed by
    /// this effect identity after a worker restart.
    pub effect_id: EffectId,
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
        const INPUT_BOUND: usize = CONTROL_PLANE_ACTION_ID_BOUND;
        const REASON_BOUND: usize = 1_024;

        if self.schema != CONTROL_PLANE_ACTION_REQUEST_SCHEMA {
            return Err(crate::ControlPlaneError::invalid_argument(
                "unsupported control-plane action request schema",
            ));
        }
        for (name, value) in [
            ("effect_id", self.effect_id.0.as_str()),
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
            ControlPlaneAction::Recover => ("recover", CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA),
            ControlPlaneAction::Cancel => ("cancel", CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA),
            ControlPlaneAction::Reconcile => {
                ("reconcile", CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA)
            }
            ControlPlaneAction::PlacementUpdate => (
                "placement_update",
                CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA,
            ),
            ControlPlaneAction::Retry => ("retry", CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA),
        };
        if self.parameters.schema != expected_schema {
            return Err(crate::ControlPlaneError::invalid_argument(format!(
                "{name} requires parameters schema {expected_schema}"
            )));
        }
        if self.action == ControlPlaneAction::Recover {
            let parameters = serde_json::from_value::<ControlPlaneRecoverParameters>(
                self.parameters.data.clone(),
            )
            .map_err(|error| {
                crate::ControlPlaneError::invalid_argument(format!("recover parameters: {error}"))
            })?;
            if parameters.preflight && parameters.candidate_ref.is_some() {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "candidate adoption cannot be combined with preflight",
                ));
            }
            if parameters.acknowledge_model_override && parameters.model.is_none() {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "acknowledge_model_override requires model",
                ));
            }
            if parameters.gate_timeout_seconds.is_some() && !parameters.regate {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "gate_timeout_seconds requires regate",
                ));
            }
            if parameters.gate_no_progress_timeout_seconds.is_some() && !parameters.regate {
                return Err(crate::ControlPlaneError::invalid_argument(
                    "gate_no_progress_timeout_seconds requires regate",
                ));
            }
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
                | ControlPlaneAction::Retry
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
    AcceptedPending,
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
    use serde_json::json;

    fn request(
        action: ControlPlaneAction,
        parameters_schema: &str,
        data: Value,
        confirmed: bool,
    ) -> ControlPlaneActionRequest {
        ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            effect_id: action_effect_id("test", "run-1", "action", "key"),
            action,
            idempotency_key: "key".to_string(),
            actor: "operator".to_string(),
            expected_updated_at: None,
            parameters: ControlPlaneActionPayload {
                schema: parameters_schema.to_string(),
                data,
            },
            confirmed,
        }
    }

    #[test]
    fn validate_enforces_typed_parameters_and_confirmation() {
        use ControlPlaneAction::{PlacementUpdate, Recover, Retry};
        let cases = [
            (
                Recover,
                CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA,
                json!({}),
                true,
                true,
            ),
            (
                Recover,
                CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA,
                json!({"preflight": true, "candidate_ref": "abc"}),
                true,
                false,
            ),
            (
                Recover,
                CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA,
                json!({"unexpected": true}),
                true,
                false,
            ),
            (
                PlacementUpdate,
                CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA,
                json!({"placement": "local"}),
                false,
                false,
            ),
            (
                PlacementUpdate,
                CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA,
                json!({"placement": "local"}),
                true,
                true,
            ),
            (
                PlacementUpdate,
                CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA,
                json!({"placement": "auto"}),
                true,
                false,
            ),
            (
                Retry,
                CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA,
                json!({"force": false, "provider_route": {"backend": "replacement"}}),
                true,
                true,
            ),
            (
                Retry,
                CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
                json!({}),
                true,
                false,
            ),
        ];
        for (action, schema, data, confirmed, valid) in cases {
            let label = format!("{action:?} {data} confirmed={confirmed}");
            let result = request(action, schema, data, confirmed).validate();
            assert_eq!(result.is_ok(), valid, "{label}: {result:?}");
        }
    }

    #[test]
    fn action_effect_id_keeps_short_ids_and_bounds_long_ones_deterministically() {
        assert_eq!(
            action_effect_id("cli", "run-1", "retry", "request-1"),
            EffectId("cli:run-1:retry:request-1".to_string())
        );

        let run_id = format!("run-{}", "x".repeat(120));
        let first = action_effect_id("cli", &run_id, "retry", "key-a");
        assert!(first.0.len() <= CONTROL_PLANE_ACTION_ID_BOUND);
        assert_eq!(first, action_effect_id("cli", &run_id, "retry", "key-a"));
        assert_ne!(first, action_effect_id("cli", &run_id, "retry", "key-b"));
        assert!(first.0.starts_with("cli:retry:"));
    }
}
