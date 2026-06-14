//! Durable agent-task controller execution service.
//!
//! Owns the controller execution policy that used to live in the CLI adapter.
//! Callers (CLI, daemon, future automation) build typed requests, hand them to
//! the service, and serialize the typed reports the service returns. The CLI
//! adapter is responsible only for argument parsing and JSON envelope rendering.
//!
//! Reports keep their existing JSON shapes via `serde` so the CLI continues to
//! emit the same envelopes after the move.

use serde::Serialize;
use serde_json::Value;

use crate::core::agent_task_lifecycle as lifecycle;
use crate::core::agent_task_loop_controller::{
    self as controller, AgentTaskLoopActionStatus, AgentTaskLoopControllerRecord,
    AgentTaskLoopControllerState, AgentTaskLoopExternalEvent, AgentTaskLoopHistoryEvent,
    AgentTaskLoopPolicyAction, AgentTaskLoopPolicyActionRecord, AgentTaskLoopRunRef,
    AgentTaskLoopTaskLineage,
};
use crate::core::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};
use crate::core::agent_task_service::{self, AgentTaskRunResult};
use crate::core::{Error, Result};

/// Schema for the apply-event report envelope.
pub const APPLY_EVENT_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-event-result/v1";
/// Schema for single-action run reports (run-next and run).
pub const ACTION_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-action-result/v1";
/// Schema for the multi-action resume report envelope.
pub const RESUME_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-resume-result/v1";
/// Schema for the list-controllers report envelope.
pub const LIST_RESULT_SCHEMA: &str = "homeboy/agent-task-loop-controller-list/v1";

/// Request to create a new durable controller record.
#[derive(Debug, Clone)]
pub struct ControllerInitRequest {
    pub loop_id: String,
    pub phase: String,
    pub config_version: String,
}

/// Request to apply an external event to a controller.
#[derive(Debug, Clone)]
pub struct ControllerApplyEventRequest {
    pub loop_id: String,
    pub event_type: String,
    /// Optional stable event id. Generated from the loop history length when omitted.
    pub event_id: Option<String>,
    pub event_key: Option<String>,
    pub entity_id: Option<String>,
    /// Event payload JSON. May contain a `policy` object to evaluate.
    pub payload: Value,
}

/// Request to mark a tracked entity as human-ready work.
#[derive(Debug, Clone)]
pub struct ControllerMarkHumanReadyRequest {
    pub loop_id: String,
    pub entity_id: String,
    pub reason: Option<String>,
}

/// Typed report returned by `apply_event`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerEventReport {
    pub schema: &'static str,
    pub controller: AgentTaskLoopControllerRecord,
    pub actions: Vec<AgentTaskLoopPolicyActionRecord>,
}

/// Typed report returned by `run_next` and `run_action`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerActionReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<Value>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Typed report returned by `resume`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerResumeReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub claimed: bool,
    pub results: Vec<Value>,
    pub controller: AgentTaskLoopControllerRecord,
}

/// Typed list report returned by `list`.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerListReport {
    pub schema: &'static str,
    pub controllers: Vec<AgentTaskLoopControllerRecord>,
}

/// Optional dispatch hook used when a `spawn_task` request asks for `"mode": "dispatch"`.
///
/// The CLI adapter implements this to bridge controller-driven dispatch into the
/// existing `agent-task dispatch` command. Callers that do not need dispatch
/// mode can pass [`NoopDispatchHook`].
pub trait ControllerDispatchHook {
    /// Run a dispatch request and return its JSON envelope and process exit code.
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)>;
}

/// Default dispatch hook that refuses dispatch requests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDispatchHook;

impl ControllerDispatchHook for NoopDispatchHook {
    fn dispatch(&self, _request: &Value) -> Result<(Value, i32)> {
        Err(Error::validation_invalid_argument(
            "request.mode",
            "controller dispatch hook is not wired; pass a ControllerDispatchHook to enable mode 'dispatch'",
            None,
            None,
        ))
    }
}

/// Create a new durable controller record.
pub fn init(request: ControllerInitRequest) -> Result<AgentTaskLoopControllerRecord> {
    controller::create_controller(&request.loop_id, &request.phase, &request.config_version)
}

/// Read a durable controller record.
pub fn status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    controller::load_controller(loop_id)
}

/// List every durable controller record.
pub fn list() -> Result<ControllerListReport> {
    Ok(ControllerListReport {
        schema: LIST_RESULT_SCHEMA,
        controllers: controller::list_controllers()?,
    })
}

/// Mark a tracked entity as human-ready work and persist the controller.
pub fn mark_human_ready(
    request: ControllerMarkHumanReadyRequest,
) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = controller::load_controller(&request.loop_id)?;
    record.mark_human_ready(&request.entity_id, request.reason)?;
    controller::write_controller(&record)?;
    Ok(record)
}

/// Apply an external event to the controller and return the resulting actions.
pub fn apply_event(request: ControllerApplyEventRequest) -> Result<ControllerEventReport> {
    let mut record = controller::load_controller(&request.loop_id)?;
    let event_id = request
        .event_id
        .unwrap_or_else(|| format!("event-{}", record.history.len() + 1));
    let actions = record.apply_event(AgentTaskLoopExternalEvent {
        event_id,
        event_type: request.event_type,
        event_key: request.event_key,
        entity_id: request.entity_id,
        payload: request.payload,
    });
    controller::write_controller(&record)?;
    Ok(ControllerEventReport {
        schema: APPLY_EVENT_RESULT_SCHEMA,
        controller: record,
        actions,
    })
}

/// Claim and execute the first pending controller action, if any.
pub fn run_next<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut record = controller::load_controller(loop_id)?;
    let Some(action_id) = first_pending_action_id(&record) else {
        return Ok(AgentTaskRunResult {
            value: ControllerActionReport {
                schema: ACTION_RESULT_SCHEMA,
                loop_id: record.loop_id.clone(),
                claimed: false,
                action_id: None,
                status: None,
                execution: None,
                controller: record,
            },
            exit_code: 0,
        });
    };
    execute_controller_action(&mut record, &action_id, executor, dispatch)
}

/// Claim and execute the named pending controller action.
pub fn run_action<E, D>(
    loop_id: &str,
    action_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut record = controller::load_controller(loop_id)?;
    execute_controller_action(&mut record, action_id, executor, dispatch)
}

/// Drain pending controller actions until none remain or one fails.
pub fn resume<E, D>(
    loop_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerResumeReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut results = Vec::new();
    loop {
        let record = controller::load_controller(loop_id)?;
        let Some(action_id) = first_pending_action_id(&record) else {
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: false,
                    results,
                    controller: record,
                },
                exit_code: 0,
            });
        };
        let action_result = run_action(loop_id, &action_id, executor.clone(), dispatch)?;
        let value = serde_json::to_value(&action_result.value)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        results.push(value);
        if action_result.exit_code != 0 {
            let record = controller::load_controller(loop_id)?;
            return Ok(AgentTaskRunResult {
                value: ControllerResumeReport {
                    schema: RESUME_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    results,
                    controller: record,
                },
                exit_code: action_result.exit_code,
            });
        }
    }
}

fn execute_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let action = claim_controller_action(record, action_id)?;
    controller::write_controller(record)?;

    match execute_claimed_controller_action(record, &action, executor, dispatch) {
        Ok((execution, exit_code)) => {
            complete_controller_action(record, action_id, &execution, exit_code)?;
            controller::write_controller(record)?;
            Ok(AgentTaskRunResult {
                value: ControllerActionReport {
                    schema: ACTION_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    action_id: Some(action_id.to_string()),
                    status: Some(
                        if exit_code == 0 {
                            "completed"
                        } else {
                            "failed"
                        }
                        .to_string(),
                    ),
                    execution: Some(execution),
                    controller: record.clone(),
                },
                exit_code,
            })
        }
        Err(error) => {
            fail_controller_action(record, action_id, &error.to_string())?;
            controller::write_controller(record)?;
            Err(error)
        }
    }
}

fn execute_claimed_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id,
            request,
        } => execute_spawn_task_action(
            record,
            action,
            dedupe_key,
            entity_id.as_deref(),
            request,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids,
            request_template,
        } => execute_fan_out_action(
            record,
            action,
            dedupe_key,
            entity_ids,
            request_template,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::SpawnController {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_controller",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::SpawnSubloop {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_subloop",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::Join { wait_key } => Ok((
            serde_json::json!({ "mode": "join", "wait_key": wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Ok((
            serde_json::json!({ "mode": "wait_for_event", "wait_key": wait.wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForController {
            loop_id,
            entity_id,
            wait_key,
            terminal_states,
        } => execute_wait_for_controller_action(
            loop_id,
            entity_id.as_deref(),
            wait_key.as_deref(),
            terminal_states,
        ),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => Ok((
            serde_json::json!({
                "mode": "run_gates",
                "bundle_id": bundle_id,
                "entity_id": entity_id,
                "queued": true,
                "note": "gate bundle execution is represented in controller state; concrete gate runners consume gate_bundles"
            }),
            0,
        )),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
            record.mark_human_ready(entity_id, reason.clone())?;
            Ok((
                serde_json::json!({ "mode": "mark_human_ready", "entity_id": entity_id }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Complete { reason } => Ok((
            serde_json::json!({ "mode": "complete", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::Abandon { reason } => Ok((
            serde_json::json!({ "mode": "abandon", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::Escalate { reason } => Ok((
            serde_json::json!({ "mode": "escalate", "reason": reason }),
            0,
        )),
        AgentTaskLoopPolicyAction::RouteFinding { .. }
        | AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. }
        | AgentTaskLoopPolicyAction::Retry { .. }
        | AgentTaskLoopPolicyAction::RequestChanges { .. } => {
            Err(Error::validation_invalid_argument(
                "action_id",
                format!(
                    "controller action '{}' is not executable by the generic controller runner yet",
                    action.action_id
                ),
                Some(action.action_id.clone()),
                None,
            ))
        }
    }
}

fn execute_spawn_task_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("run_plan");
    match mode {
        "run_plan" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            let run_result = agent_task_service::run_submitted(submitted.run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                    "aggregate": aggregate_value,
                }),
                run_result.exit_code,
            ))
        }
        "submit" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            Ok((
                serde_json::json!({
                    "mode": mode,
                    "run_id": submitted.run_id,
                    "submitted": submitted,
                }),
                0,
            ))
        }
        "run" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::run_submitted(run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                run_result.exit_code,
            ))
        }
        "resume" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::resume(run_id.clone(), executor)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                run_result.exit_code,
            ))
        }
        "run_next" => {
            let run_result = agent_task_service::run_next(executor)?;
            let value = match run_result.value {
                Some(aggregate) => serde_json::to_value(&aggregate)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
                None => serde_json::json!({ "claimed": false }),
            };
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                run_result.exit_code,
            ))
        }
        "dispatch" => {
            let (value, exit_code) = dispatch.dispatch(request)?;
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                serde_json::json!({ "mode": mode, "result": value }),
                exit_code,
            ))
        }
        other => Err(Error::validation_invalid_argument(
            "request.mode",
            format!("unsupported spawn_task request mode '{other}'"),
            Some(other.to_string()),
            Some(vec![
                "Supported modes: run_plan, submit, run, resume, run_next, dispatch".to_string(),
            ]),
        )),
    }
}

fn execute_fan_out_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_ids: &[String],
    request_template: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let mut results = Vec::new();
    let mut exit_code = 0;
    for entity_id in entity_ids {
        let request = materialize_fan_out_request(request_template, entity_id);
        let child_dedupe_key = format!("{dedupe_key}:{entity_id}");
        let child_action = AgentTaskLoopPolicyActionRecord {
            action_id: format!("{}:{entity_id}", action.action_id),
            action: AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: child_dedupe_key.clone(),
                entity_id: Some(entity_id.clone()),
                request: request.clone(),
            },
            status: AgentTaskLoopActionStatus::Running,
            reason: action.reason.clone(),
            created_at: action.created_at.clone(),
            dedupe_key: Some(child_dedupe_key.clone()),
            diagnostics: Vec::new(),
        };
        let (result, child_exit_code) = execute_spawn_task_action(
            record,
            &child_action,
            &child_dedupe_key,
            Some(entity_id),
            &request,
            executor.clone(),
            dispatch,
        )?;
        if child_exit_code != 0 {
            exit_code = child_exit_code;
        }
        results.push(result);
    }
    Ok((
        serde_json::json!({ "mode": "fan_out", "results": results }),
        exit_code,
    ))
}

#[allow(clippy::too_many_arguments)]
fn execute_spawn_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    mode: &str,
    dedupe_key: &str,
    loop_id: &str,
    entity_id: Option<&str>,
    phase: &str,
    config_version: &str,
    request: &Value,
) -> Result<(Value, i32)> {
    let mut created = false;
    let mut child = match controller::load_controller(loop_id) {
        Ok(child) => child,
        Err(_) => {
            created = true;
            let mut child = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
            child.parent_loop_id = Some(record.loop_id.clone());
            child.parent_action_id = Some(action.action_id.clone());
            child.parent_entity_id = entity_id.map(str::to_string);
            child.metadata = request.clone();
            child
        }
    };
    let mut child_changed = false;
    if child.parent_loop_id.is_none() {
        child.parent_loop_id = Some(record.loop_id.clone());
        child_changed = true;
    }
    if child.parent_action_id.is_none() {
        child.parent_action_id = Some(action.action_id.clone());
        child_changed = true;
    }
    if child.parent_entity_id.is_none() {
        child.parent_entity_id = entity_id.map(str::to_string);
        child_changed = child.parent_entity_id.is_some();
    }
    if created || child_changed {
        controller::write_controller(&child)?;
    }
    push_controller_history(
        record,
        "controller.action.spawned_controller",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "loop_id": child.loop_id,
            "mode": mode,
            "created": created,
        }),
    );
    Ok((
        serde_json::json!({
            "mode": mode,
            "loop_id": child.loop_id,
            "phase": child.phase,
            "config_version": child.config_version,
            "created": created,
        }),
        0,
    ))
}

fn execute_wait_for_controller_action(
    loop_id: &str,
    entity_id: Option<&str>,
    wait_key: Option<&str>,
    terminal_states: &[AgentTaskLoopControllerState],
) -> Result<(Value, i32)> {
    let child_state = controller::load_controller(loop_id)
        .ok()
        .map(|child| format!("{:?}", child.state));
    Ok((
        serde_json::json!({
            "mode": "wait_for_controller",
            "loop_id": loop_id,
            "entity_id": entity_id,
            "wait_key": wait_key,
            "terminal_states": terminal_states,
            "child_state": child_state,
        }),
        0,
    ))
}

fn claim_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
) -> Result<AgentTaskLoopPolicyActionRecord> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    if action.status != AgentTaskLoopActionStatus::Pending {
        return Err(Error::validation_invalid_argument(
            "action_id",
            format!(
                "controller action '{}' is {:?}, not pending",
                action.action_id, action.status
            ),
            Some(action.action_id.clone()),
            None,
        ));
    }
    action.status = AgentTaskLoopActionStatus::Running;
    let action = action.clone();
    push_controller_history(
        record,
        "controller.action.claimed",
        None,
        serde_json::json!({ "action_id": action.action_id, "dedupe_key": action.dedupe_key }),
    );
    Ok(action)
}

fn complete_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    execution: &Value,
    exit_code: i32,
) -> Result<()> {
    let status = if exit_code == 0 {
        AgentTaskLoopActionStatus::Completed
    } else {
        AgentTaskLoopActionStatus::Failed
    };
    set_controller_action_status(record, action_id, status)?;
    push_controller_history(
        record,
        if exit_code == 0 {
            "controller.action.completed"
        } else {
            "controller.action.failed"
        },
        None,
        serde_json::json!({ "action_id": action_id, "exit_code": exit_code, "execution": execution }),
    );
    Ok(())
}

fn fail_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    message: &str,
) -> Result<()> {
    set_controller_action_status(record, action_id, AgentTaskLoopActionStatus::Failed)?;
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({ "action_id": action_id, "error": message }),
    );
    Ok(())
}

fn set_controller_action_status(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    status: AgentTaskLoopActionStatus,
) -> Result<()> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = status;
    Ok(())
}

fn record_controller_spawn(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    run_id: &str,
    request: &Value,
) -> Result<()> {
    if let Some(dedupe) = record.dedupe_keys.get_mut(dedupe_key) {
        dedupe.run_id = Some(run_id.to_string());
    }
    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            if !entity.run_refs.iter().any(|run| run.run_id == run_id) {
                entity.run_refs.push(AgentTaskLoopRunRef {
                    run_id: run_id.to_string(),
                    task_id: None,
                    role: Some("spawn_task".to_string()),
                });
            }
        }
    }
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == run_id)
    {
        record.task_lineage.push(AgentTaskLoopTaskLineage {
            run_id: run_id.to_string(),
            task_id: None,
            parent_run_id: None,
            parent_task_id: None,
            entity_id: entity_id.map(str::to_string),
            dedupe_key: Some(dedupe_key.to_string()),
            artifact_refs: Vec::new(),
            inputs: request.clone(),
            outputs: Value::Null,
        });
    }
    push_controller_history(
        record,
        "controller.action.spawned_run",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "run_id": run_id,
        }),
    );
    controller::write_controller(record)?;
    Ok(())
}

fn first_pending_action_id(record: &AgentTaskLoopControllerRecord) -> Option<String> {
    record
        .next_actions
        .iter()
        .find(|action| action.status == AgentTaskLoopActionStatus::Pending)
        .map(|action| action.action_id.clone())
}

/// Parse an `AgentTaskPlan` out of a controller spawn-task request.
///
/// The request may either wrap the plan under a `"plan"` field or be the plan
/// directly. Exposed for callers that need to validate plans before scheduling.
pub fn plan_from_controller_request(request: &Value) -> Result<AgentTaskPlan> {
    let plan_value = request.get("plan").unwrap_or(request);
    serde_json::from_value(plan_value.clone()).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("controller spawn_task plan".to_string()),
            Some(plan_value.to_string()),
        )
    })
}

fn materialize_fan_out_request(template: &Value, entity_id: &str) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.entry("run_id".to_string()).or_insert_with(|| {
            Value::String(format!(
                "controller-{}",
                entity_id.replace([':', '/', '#'], "_")
            ))
        });
    }
    request
}

fn controller_request_run_id(request: &Value, dedupe_key: &str, action_id: &str) -> String {
    optional_string(request, "run_id").unwrap_or_else(|| {
        format!(
            "controller-{}-{}",
            action_id,
            dedupe_key.replace([':', '/', '#', ' '], "_")
        )
    })
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).ok_or_else(|| {
        Error::validation_invalid_argument(
            key,
            format!("controller action request requires string field '{key}'"),
            None,
            None,
        )
    })
}

/// Extract an optional string field from a controller request JSON value.
pub fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Extract an optional boolean field from a controller request JSON value.
pub fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

/// Extract an optional `u32` field from a controller request JSON value.
pub fn optional_u32(value: &Value, key: &str) -> Result<Option<u32>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a u32"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `usize` field from a controller request JSON value.
pub fn optional_usize(value: &Value, key: &str) -> Result<Option<usize>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        format!("controller action request field '{key}' must be a usize"),
                        Some(value.to_string()),
                        None,
                    )
                })
        })
        .transpose()
}

/// Extract an optional `Vec<String>` field from a controller request JSON value.
pub fn optional_string_array(value: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = value.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            key,
            format!("controller action request field '{key}' must be an array of strings"),
            Some(value.to_string()),
            None,
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                Error::validation_invalid_argument(
                    key,
                    format!("controller action request field '{key}' must contain only strings"),
                    Some(value.to_string()),
                    None,
                )
            })
        })
        .collect()
}

fn push_controller_history(
    record: &mut AgentTaskLoopControllerRecord,
    event_type: &str,
    entity_id: Option<String>,
    payload: Value,
) {
    record.history.push(AgentTaskLoopHistoryEvent {
        event_id: format!("event-{}", record.history.len() + 1),
        event_type: event_type.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entity_id,
        payload,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_loop_controller::{
        AgentTaskLoopPolicyAction, AgentTaskLoopWait, AgentTaskLoopWaitStatus,
    };
    use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
    use crate::test_support::with_isolated_home;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "controller-service-plan",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "controller-service-task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("fixture".to_string()),
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    #[test]
    fn init_and_status_round_trip_controller_record() {
        with_isolated_home(|_| {
            let record = init(ControllerInitRequest {
                loop_id: "loop-service-init".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            assert_eq!(record.loop_id, "loop-service-init");
            assert_eq!(record.phase, "repair");

            let loaded = status("loop-service-init").expect("controller loaded");
            assert_eq!(loaded, record);
        });
    }

    #[test]
    fn list_returns_existing_controllers() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-list-a".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller a initialized");
            init(ControllerInitRequest {
                loop_id: "loop-service-list-b".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller b initialized");

            let report = list().expect("controllers listed");
            assert_eq!(report.schema, LIST_RESULT_SCHEMA);
            assert_eq!(report.controllers.len(), 2);
        });
    }

    #[test]
    fn run_next_returns_unclaimed_when_no_pending_actions() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-noop".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let result = run_next(
                "loop-service-noop",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("controller polled");

            assert_eq!(result.exit_code, 0);
            assert!(!result.value.claimed);
            assert_eq!(result.value.schema, ACTION_RESULT_SCHEMA);
            assert!(result.value.action_id.is_none());
            assert!(result.value.execution.is_none());
        });
    }

    #[test]
    fn run_next_executes_spawn_task_action_and_records_lineage() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-spawn".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let plan = test_plan();
            record.record_action(
                AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "finding:abc:repair".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "run_plan",
                        "run_id": "controller-service-spawn-a",
                        "plan": plan,
                    }),
                },
                "finding emitted",
            );
            controller::write_controller(&record).expect("controller written");

            let executor = CapturingExecutor::default();
            let result = run_next("loop-service-spawn", executor.clone(), &NoopDispatchHook)
                .expect("controller action executed");

            assert_eq!(result.exit_code, 0);
            assert!(result.value.claimed);
            assert_eq!(result.value.status.as_deref(), Some("completed"));

            let loaded = controller::load_controller("loop-service-spawn").expect("controller");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Completed
            );
            assert_eq!(
                loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
                Some("controller-service-spawn-a")
            );
            assert_eq!(loaded.task_lineage[0].run_id, "controller-service-spawn-a");
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.claimed"));
            assert!(loaded
                .history
                .iter()
                .any(|event| event.event_type == "controller.action.completed"));

            let observed = executor
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("provider saw request");
            assert_eq!(observed.task_id, "controller-service-task");
        });
    }

    #[test]
    fn run_action_executes_only_requested_action_id() {
        with_isolated_home(|_| {
            let mut record = init(ControllerInitRequest {
                loop_id: "loop-service-action".to_string(),
                phase: "repair".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
                    wait_key: "wait-a".to_string(),
                    event_type: "task.completed".to_string(),
                    entity_id: None,
                    external_ref: None,
                    timeout_at: None,
                    escalation_policy: None,
                    status: AgentTaskLoopWaitStatus::Open,
                    satisfied_by_event_id: None,
                }),
                "wait first",
            );
            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("done".to_string()),
                },
                "complete second",
            );
            controller::write_controller(&record).expect("controller written");

            let result = run_action(
                "loop-service-action",
                "action-2",
                CapturingExecutor::default(),
                &NoopDispatchHook,
            )
            .expect("action executed");

            assert_eq!(result.exit_code, 0);
            assert!(result.value.claimed);
            let loaded = controller::load_controller("loop-service-action").expect("controller");
            assert_eq!(
                loaded.next_actions[0].status,
                AgentTaskLoopActionStatus::Pending
            );
            assert_eq!(
                loaded.next_actions[1].status,
                AgentTaskLoopActionStatus::Completed
            );
        });
    }

    #[test]
    fn apply_event_persists_actions_and_keeps_event_envelope() {
        with_isolated_home(|_| {
            init(ControllerInitRequest {
                loop_id: "loop-service-event".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            })
            .expect("controller initialized");

            let report = apply_event(ControllerApplyEventRequest {
                loop_id: "loop-service-event".to_string(),
                event_type: "task.completed".to_string(),
                event_id: None,
                event_key: None,
                entity_id: None,
                payload: Value::Null,
            })
            .expect("event applied");

            assert_eq!(report.schema, APPLY_EVENT_RESULT_SCHEMA);
            assert!(!report
                .controller
                .history
                .iter()
                .all(|event| event.event_type == "controller.action.claimed"));
        });
    }
}
