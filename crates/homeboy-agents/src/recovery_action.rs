//! Control-plane recovery performer and typed recovery inputs.
use crate::agent_task_service::{self as service, CookRecoveryOverrides, CookRecoveryPerformer};
use homeboy_core::Result;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

pub type RecoverParameters = homeboy_control_plane_contract::ControlPlaneRecoverParameters<
    crate::agent_task_gate::VerifyGateOptions,
    service::CookPublishRequest,
    service::CookReplaceGatesRequest,
>;
type Dispatcher = fn(&Value) -> Result<Option<Arc<dyn service::AgentTaskCookAttemptDispatcher>>>;
static DISPATCHER: OnceLock<Dispatcher> = OnceLock::new();
type Preflight = fn(&str, bool) -> Result<(Value, i32)>;
static PREFLIGHT: OnceLock<Preflight> = OnceLock::new();
pub fn register_preflight(preflight: Preflight) {
    let _ = PREFLIGHT.set(preflight);
}

/// Register the host's transport reconstruction at startup, including daemon startup.
pub fn register_dispatcher(dispatcher: Dispatcher) {
    let _ = DISPATCHER.set(dispatcher);
}

pub struct ControlPlaneRecoveryPerformer;
impl CookRecoveryPerformer for ControlPlaneRecoveryPerformer {
    fn admit_step(
        &self,
        report: &service::CookRecoveryReport,
        overrides: &CookRecoveryOverrides,
    ) -> Result<Option<Value>> {
        service::admit_recovery_execution_step(report, overrides).map(Some)
    }
    fn preflight_continue_cook(&self, id: &str, rearm: bool) -> Result<(Value, i32)> {
        if let Some(preflight) = PREFLIGHT.get() {
            return preflight(id, rearm);
        }
        Ok((
            serde_json::to_value(service::plan_cook_recovery(id)?)
                .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?,
            0,
        ))
    }
    fn retry(&self, id: &str) -> Result<(Value, i32)> {
        action(
            id,
            homeboy_control_plane_contract::ControlPlaneAction::Retry,
            homeboy_control_plane_contract::CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA,
            serde_json::json!({}),
        )
    }
    fn reconcile(&self, id: &str) -> Result<(Value, i32)> {
        action(
            id,
            homeboy_control_plane_contract::ControlPlaneAction::Reconcile,
            homeboy_control_plane_contract::CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
            Value::Null,
        )
    }
    fn adopt(&self, id: &str, candidate: &str) -> Result<(Value, i32)> {
        adopt(id, candidate, None, None, false)
    }
}
fn action(
    id: &str,
    action: homeboy_control_plane_contract::ControlPlaneAction,
    schema: &str,
    data: Value,
) -> Result<(Value, i32)> {
    use homeboy_control_plane_contract::*;
    let key = format!("recovery-{}", uuid::Uuid::new_v4());
    let request = ControlPlaneActionRequest {
        schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.into(),
        effect_id: action_effect_id("recovery", id, &format!("{action:?}"), &key),
        idempotency_key: key,
        action,
        actor: "recovery".into(),
        confirmed: true,
        expected_updated_at: None,
        parameters: ControlPlaneActionPayload {
            schema: schema.into(),
            data,
        },
    };
    let ack = crate::orchestration::execute_action_from_current_environment(id, &request)?;
    let mut result = ack.result.data;
    let failed = ack.outcome == ControlPlaneActionOutcome::Failed;
    if failed {
        // Refused control-plane actions can have an empty payload. Retain the
        // acknowledgement's concrete refusal instead of returning an empty result.
        if !result.is_object() {
            result = serde_json::json!({});
        }
        if result.get("failure").is_none() {
            if let Some(message) = ack.message {
                result["failure"] = serde_json::json!({
                    "code": "agent_task.recover_action_refused",
                    "message": message,
                });
            }
        }
    } else if action == ControlPlaneAction::Retry {
        // The retry action reserves a daemon-scheduled successor and returns
        // its identity nested under `record`, not the recovery source id.
        if let Some(run_id) = result.pointer("/record/run_id").cloned() {
            result["run_id"] = run_id;
            result["state"] = serde_json::to_value(ack.resource.state)
                .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))?;
        }
    }
    Ok((result, i32::from(failed)))
}
pub fn adopt(
    id: &str,
    candidate: &str,
    attempt: Option<u32>,
    model: Option<String>,
    replace_interrupted: bool,
) -> Result<(Value, i32)> {
    let result = service::adopt_cook_candidate(
        id,
        candidate,
        service::AgentTaskCandidateAdoptionOptions {
            attempt,
            ai_model: model,
            replace_interrupted,
            reconstruct_dispatcher: Some(Box::new(|recipe: &Value| match DISPATCHER.get() {
                Some(dispatcher) => dispatcher(recipe),
                None => Ok(None),
            })),
            ..Default::default()
        },
    )?;
    Ok((
        serde_json::to_value(result.value)
            .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?,
        result.exit_code,
    ))
}

/// Perform `recover` for `id`. A nonzero exit always carries a structured
/// failure naming the run state, the refusal and the next command (#18845).
pub fn execute(id: &str, parameters: RecoverParameters) -> Result<(Value, i32)> {
    let (report, code) = execute_step(id, parameters)?;
    Ok((
        service::ensure_recover_failure_contract(report, code, id),
        code,
    ))
}

fn execute_step(id: &str, parameters: RecoverParameters) -> Result<(Value, i32)> {
    if let Some(candidate) = parameters.candidate_ref {
        return adopt(
            id,
            &candidate,
            parameters.attempt,
            parameters.adoption_model,
            parameters.replace_interrupted,
        );
    }
    if !parameters.preflight {
        if let Some(receipt) = service::recovery_step_in_flight(id)? {
            return Ok((receipt, 0));
        }
    }
    // With `regate`, verify commands replace the candidate's declared gates
    // for that re-gate instead of correcting a pre-provider recipe (#17802).
    let (corrected_gates, regate_inputs) = if parameters.regate {
        (
            None,
            service::CookRegateInputs {
                gates: parameters.corrected_gates,
                gate_timeout_seconds: parameters.gate_timeout_seconds,
                gate_no_progress_timeout_seconds: parameters.gate_no_progress_timeout_seconds,
            },
        )
    } else {
        (parameters.corrected_gates, Default::default())
    };
    let request = service::CookRecoveryRequest {
        preflight: parameters.preflight,
        regate: parameters.regate,
        regate_inputs,
        unblock: parameters.unblock,
        overrides: CookRecoveryOverrides {
            backend: parameters.backend,
            selector: parameters.selector,
            model: parameters.model,
            acknowledge_model_override: parameters.acknowledge_model_override,
            timeout_ms: parameters.timeout_ms,
        },
        corrected_gates,
        replace_gates: parameters.replace_gates,
        publish: parameters.publish.unwrap_or_default(),
    };
    let (report, code) = service::recover_cook(id, request, &ControlPlaneRecoveryPerformer)?;
    Ok((
        serde_json::to_value(report)
            .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?,
        code,
    ))
}
