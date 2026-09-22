use super::*;
use crate::agent_task::AgentTaskEvidenceRef;
use crate::agent_task_lifecycle;
use chrono::{DateTime, Utc};
use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome,
    ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneRun, ControlPlaneRunState,
    RunId, CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA, CONTROL_PLANE_CANCEL_RESULT_SCHEMA,
};
use homeboy_core::control_plane::{
    register_control_plane_action_delegate as register_core_action_delegate,
    ControlPlaneActionDelegate, ControlPlaneActionDelegateResult,
};
use homeboy_core::engine::local_files::write_json_file as write_json;
use homeboy_core::observation::{ControlPlaneResourceProjection, RunRecord};
use homeboy_core::{paths, Error, Result};
use homeboy_engine_primitives::content_hash;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// A bounded read of the shared detached work owner. Errors are deliberately
/// represented as unavailable data: a failed daemon inspection is not success.
pub fn loop_work_status(metadata: &Value) -> Value {
    let Some(job_id) = metadata.pointer("/work_job/job_id").and_then(Value::as_str) else {
        return Value::Null;
    };
    match homeboy_core::daemon::LocalControllerJobClient::connect_existing_job(job_id)
        .and_then(|client| client.status(job_id))
    {
        Ok(job) => serde_json::json!({
            "job_id": job_id,
            "status": job.status,
            "event_count": job.event_count,
            "updated_at_ms": job.updated_at_ms,
        }),
        Err(error) => serde_json::json!({
            "job_id": job_id,
            "status": "unavailable",
            "error": { "code": format!("{:?}", error.code) },
        }),
    }
}

pub const LOOP_CONTROL_PLANE_RESOURCE_TYPE: &str = "agent_task_loop";
const LOOP_RESUME_PARAMETERS_SCHEMA: &str = "homeboy/agent-task-loop-resume-parameters/v1";

/// The loop identity is not a work-job identity. This explicit mapping gives
/// the loop domain a stable control-plane resource without aliasing either ID.
pub fn control_plane_run_id(loop_id: &str) -> Result<RunId> {
    RunId::new(format!("loop:{loop_id}")).map_err(|error| {
        Error::validation_invalid_argument("loop_id", error.to_string(), None, None)
    })
}

/// Adapt the CLI stop request to the canonical action service.
pub fn stop_loop(
    loop_id: &str,
    reason: &str,
) -> Result<(
    AgentTaskLoopControllerRecord,
    ControlPlaneActionAcknowledgement,
)> {
    let record = load_controller(loop_id)?;
    // Legacy controller JSON predates the SQLite resource projection. Stop is
    // the explicit mutation boundary that may rebuild that projection; status
    // remains a pure JSON read.
    prepare_control_plane_loop(&record)?;
    let run = control_plane_run_id(&record.loop_id)?;
    let generation = record.updated_at.clone();
    let request = ControlPlaneActionRequest {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
        effect_id: homeboy_control_plane_contract::EffectId(format!(
            "loop-stop:{}:{}",
            run, generation
        )),
        action: ControlPlaneAction::Cancel,
        idempotency_key: format!("loop-stop:{}:{}", run, generation),
        actor: "homeboy-agent-task-loop".to_string(),
        expected_updated_at: Some(generation),
        parameters: ControlPlaneActionPayload {
            schema: CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA.to_string(),
            data: serde_json::json!({ "reason": reason }),
        },
        confirmed: true,
    };
    let acknowledgement =
        homeboy_core::control_plane::execute_action(&run, &request).map_err(|error| match error
            .class
        {
            homeboy_control_plane_contract::ControlPlaneErrorClass::InvalidArgument
            | homeboy_control_plane_contract::ControlPlaneErrorClass::NotFound => {
                Error::validation_invalid_argument("loop_id", error.message, None, None)
            }
            _ => Error::internal_unexpected(error.message),
        })?;
    if acknowledgement.outcome == ControlPlaneActionOutcome::Failed {
        return Err(Error::internal_unexpected(
            acknowledgement
                .message
                .clone()
                .unwrap_or_else(|| "loop stop action failed".to_string()),
        ));
    }
    Ok((load_controller(loop_id)?, acknowledgement))
}

/// Adapt loop resume to the canonical action service. The CLI supplies only
/// dispatch defaults; generation fencing and WorkJob submission stay here.
pub fn resume_loop(
    loop_id: &str,
    revolution_limit: Option<u32>,
    dispatch_defaults: Value,
) -> Result<ControlPlaneActionAcknowledgement> {
    let record = load_controller(loop_id)?;
    let run = control_plane_run_id(&record.loop_id)?;
    let dispatch_defaults = admitted_dispatch_defaults(dispatch_defaults)?;
    let request = ControlPlaneActionRequest {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
        effect_id: homeboy_control_plane_contract::EffectId(format!(
            "loop-resume:{}:{}",
            run, record.updated_at
        )),
        action: ControlPlaneAction::Resume,
        idempotency_key: format!("loop-resume:{}:{}", run, record.updated_at),
        actor: "homeboy-agent-task-loop".to_string(),
        expected_updated_at: Some(record.updated_at.clone()),
        parameters: ControlPlaneActionPayload {
            schema: LOOP_RESUME_PARAMETERS_SCHEMA.to_string(),
            data: serde_json::json!({
                "revolution_limit": revolution_limit,
                "dispatch_defaults": dispatch_defaults,
            }),
        },
        confirmed: true,
    };
    let acknowledgement =
        homeboy_core::control_plane::execute_action(&run, &request).map_err(|error| match error
            .class
        {
            homeboy_control_plane_contract::ControlPlaneErrorClass::InvalidArgument
            | homeboy_control_plane_contract::ControlPlaneErrorClass::NotFound => {
                Error::validation_invalid_argument("loop_id", error.message, None, None)
            }
            _ => Error::internal_unexpected(error.message),
        })?;
    if acknowledgement.outcome == ControlPlaneActionOutcome::Failed {
        return Err(Error::internal_unexpected(
            acknowledgement
                .message
                .clone()
                .unwrap_or_else(|| "loop resume action failed".to_string()),
        ));
    }
    Ok(acknowledgement)
}

/// Only route selection is durable loop intent. Provider configuration and
/// credential material belong to the caller's admitted catalog/Runner handoff,
/// never to a generic control-plane action payload.
fn admitted_dispatch_defaults(value: Value) -> Result<Value> {
    let object = value.as_object().ok_or_else(|| {
        Error::validation_invalid_argument(
            "dispatch_defaults",
            "dispatch defaults must be an object",
            None,
            None,
        )
    })?;
    let mut admitted = serde_json::Map::new();
    for key in ["backend", "selector", "model"] {
        if let Some(value) = object.get(key) {
            admitted.insert(key.to_string(), value.clone());
        }
    }
    Ok(Value::Object(admitted))
}

fn cancel_work_job(record: &AgentTaskLoopControllerRecord, reason: &str) -> Result<Value> {
    let Some(job_id) = record
        .metadata
        .pointer("/work_job/job_id")
        .and_then(Value::as_str)
    else {
        return Ok(Value::Null);
    };
    let job = homeboy_core::daemon::LocalControllerJobClient::connect_existing_job(job_id)?
        .cancel(job_id, reason)?;
    Ok(serde_json::json!({ "job_id": job_id, "status": job.status }))
}

/// Cancel provider runs already owned by a loop before the WorkJob reaches its
/// terminal projection. The lifecycle store is the scheduler's cancellation
/// authority; setting only the loop state would leave a provider child alive.
pub fn cancel_owned_provider_runs(loop_id: &str, reason: &str) -> Result<()> {
    let mut record = load_controller(loop_id)?;
    let mut run_ids = record
        .task_lineage
        .iter()
        .map(|lineage| lineage.run_id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(active) = record
        .metadata
        .get("active_provider_runs")
        .and_then(Value::as_array)
    {
        run_ids.extend(active.iter().filter_map(Value::as_str).map(str::to_string));
    }
    for run_id in run_ids {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match crate::agent_task_lifecycle::cancel_run(&run_id, Some(reason)) {
                Ok(_) => break,
                Err(error) if std::time::Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    }
    if !matches!(
        record.state,
        AgentTaskLoopControllerState::HumanReady
            | AgentTaskLoopControllerState::Completed
            | AgentTaskLoopControllerState::Abandoned
            | AgentTaskLoopControllerState::Escalated
            | AgentTaskLoopControllerState::Failed
    ) {
        record.state = AgentTaskLoopControllerState::Abandoned;
        write_controller(&record)?;
    }
    Ok(())
}

fn admit_loop_work_job(loop_id: &str, generation: &str, dispatch_defaults: Value) -> Result<Value> {
    let provider_catalog = crate::agent_task_provider::AgentTaskProviderCatalog::discover();
    let submission = crate::agent_task_service::loop_work_job_execution_submission(
        loop_id,
        generation,
        dispatch_defaults,
        provider_catalog,
    )?;
    let client = homeboy_core::daemon::LocalControllerJobClient::connect_current_build()?;
    let job = client.submit(submission)?;
    let job_id = job.id.to_string();
    persist_loop_work_identity(loop_id, &job_id)?;
    client.start(&job_id)?;
    Ok(serde_json::json!({
        "schema": "homeboy/agent-task-loop-work-submission/v1",
        "loop_id": loop_id,
        "job_id": job_id,
        "state": "submitted",
    }))
}

fn persist_loop_work_identity(loop_id: &str, job_id: &str) -> Result<()> {
    let mut record = load_controller(loop_id)?;
    if !record.metadata.is_object() {
        record.metadata = serde_json::json!({});
    }
    record.metadata["work_job"] = serde_json::json!({
        "schema": "homeboy/agent-task-loop-work-ref/v1",
        "job_id": job_id,
        "state": "submitted",
    });
    if record.metadata["resume_operation"].is_object() {
        record.metadata["resume_operation"]["state"] = Value::String("published".to_string());
    }
    write_controller(&record)
}

fn work_job_is_terminal(work: &Value) -> bool {
    matches!(
        work.get("status").and_then(Value::as_str),
        Some("completed" | "cancelled" | "failed" | "succeeded" | "terminated")
    )
}

struct LoopActionDelegate;

impl ControlPlaneActionDelegate for LoopActionDelegate {
    fn run_kind(&self) -> &'static str {
        "agent-task-loop"
    }

    fn execute(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
    ) -> std::result::Result<
        ControlPlaneActionDelegateResult,
        homeboy_control_plane_contract::ControlPlaneError,
    > {
        match request.action {
            ControlPlaneAction::Cancel => self.stop(run, request, false),
            ControlPlaneAction::Resume => self.resume(run, request),
            _ => Err(
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                    "agent-task loop supports only cancel and resume",
                ),
            ),
        }
    }

    fn recover(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
    ) -> std::result::Result<
        ControlPlaneActionDelegateResult,
        homeboy_control_plane_contract::ControlPlaneError,
    > {
        match request.action {
            ControlPlaneAction::Cancel => self.stop(run, request, true),
            ControlPlaneAction::Resume => self.resume(run, request),
            _ => Err(
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                    "agent-task loop supports only cancel and resume",
                ),
            ),
        }
    }
}

impl LoopActionDelegate {
    fn stop(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
        recovering: bool,
    ) -> std::result::Result<
        ControlPlaneActionDelegateResult,
        homeboy_control_plane_contract::ControlPlaneError,
    > {
        if request.action != ControlPlaneAction::Cancel {
            return Err(
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                    "agent-task loop supports only cancel",
                ),
            );
        }
        let loop_id = run
            .metadata_json
            .get("loop_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(
                    "loop resource has no persisted loop identity",
                )
            })?;
        let mut record = load_controller(loop_id).map_err(|error| {
            homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
        })?;
        let already_off = loop_runtime_metadata(&record.metadata)["on"] == false;
        let mut performed_cancellation = false;
        // Persist the off marker before inspecting or touching the daemon. A
        // wedged daemon must not leave the controller advertising that it runs.
        if !already_off {
            let limit = loop_runtime_metadata(&record.metadata)
                .get("revolution_limit")
                .and_then(Value::as_u64)
                .map(|value| value as u32);
            stamp_loop_runtime_metadata(&mut record.metadata, false, limit, false).map_err(
                |error| {
                    homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
                },
            )?;
            record.updated_at = Utc::now().to_rfc3339();
            write_controller(&record).map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
            })?;
        }
        let existing_work = loop_work_status(&record.metadata);
        if existing_work
            .get("status")
            .is_some_and(|status| status == "unavailable")
        {
            return Err(
                homeboy_control_plane_contract::ControlPlaneError::unavailable(
                    "loop work cancellation could not be observed; loop remains off",
                ),
            );
        }
        let work = if existing_work.is_null() || work_job_is_terminal(&existing_work) {
            existing_work
        } else {
            performed_cancellation = true;
            cancel_work_job(
                &record,
                request.parameters.data["reason"]
                    .as_str()
                    .unwrap_or("loop stop requested"),
            )
            .map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
            })?
        };
        Ok(ControlPlaneActionDelegateResult {
            outcome: if performed_cancellation {
                ControlPlaneActionOutcome::Succeeded
            } else if already_off || recovering {
                ControlPlaneActionOutcome::AlreadySatisfied
            } else {
                ControlPlaneActionOutcome::Succeeded
            },
            result: ControlPlaneActionPayload {
                schema: CONTROL_PLANE_CANCEL_RESULT_SCHEMA.to_string(),
                data: serde_json::json!({ "loop_id": loop_id, "work": work }),
            },
            message: None,
        })
    }

    fn resume(
        &self,
        run: &RunRecord,
        request: &ControlPlaneActionRequest,
    ) -> std::result::Result<
        ControlPlaneActionDelegateResult,
        homeboy_control_plane_contract::ControlPlaneError,
    > {
        let loop_id = run
            .metadata_json
            .get("loop_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(
                    "loop resource has no persisted loop identity",
                )
            })?;
        let mut record = load_controller(loop_id).map_err(|error| {
            homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
        })?;
        let runtime = loop_runtime_metadata(&record.metadata);
        if !runtime["on"].as_bool().unwrap_or(true) {
            return Err(
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                    "agent-task loop resume requires an on loop",
                ),
            );
        }
        let limit = request
            .parameters
            .data
            .get("revolution_limit")
            .and_then(Value::as_u64)
            .map(|value| value as u32)
            .or_else(|| {
                runtime["revolution_limit"]
                    .as_u64()
                    .map(|value| value as u32)
            });
        let current = runtime["revolutions"].as_u64().unwrap_or(0) as u32;
        if limit.is_some_and(|limit| current >= limit) {
            return Ok(ControlPlaneActionDelegateResult {
                outcome: ControlPlaneActionOutcome::AlreadySatisfied,
                result: ControlPlaneActionPayload {
                    schema: homeboy_control_plane_contract::CONTROL_PLANE_RESUME_RESULT_SCHEMA
                        .to_string(),
                    data: serde_json::json!({
                        "aggregate": { "claimed": false },
                        "exit_code": 0,
                        "stopped_reason": "revolution_limit_reached",
                    }),
                },
                message: None,
            });
        }
        let operation_id = request.effect_id.0.clone();
        let reserved = record
            .metadata
            .get("resume_operation")
            .filter(|operation| operation["effect_id"] == operation_id)
            .cloned();
        if reserved.is_none() {
            let dispatch_defaults = request
                .parameters
                .data
                .get("dispatch_defaults")
                .cloned()
                .unwrap_or(Value::Null);
            let dispatch_defaults =
                admitted_dispatch_defaults(dispatch_defaults).map_err(|error| {
                    homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                        error.to_string(),
                    )
                })?;
            stamp_loop_runtime_metadata(&mut record.metadata, true, limit, true).map_err(
                |error| {
                    homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
                },
            )?;
            record.updated_at = Utc::now().to_rfc3339();
            record.metadata["resume_operation"] = serde_json::json!({
                "schema": "homeboy/agent-task-loop-resume-operation/v1",
                "effect_id": operation_id,
                "generation": record.updated_at,
                "state": "reserved",
                "dispatch_defaults": dispatch_defaults,
            });
            write_controller(&record).map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
            })?;
        }
        let generation = record.updated_at.clone();
        let dispatch_defaults = record.metadata["resume_operation"]["dispatch_defaults"].clone();
        let work =
            admit_loop_work_job(loop_id, &generation, dispatch_defaults).map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
            })?;
        Ok(ControlPlaneActionDelegateResult {
            outcome: ControlPlaneActionOutcome::Succeeded,
            result: ControlPlaneActionPayload {
                schema: homeboy_control_plane_contract::CONTROL_PLANE_RESUME_RESULT_SCHEMA
                    .to_string(),
                data: serde_json::json!({
                    "aggregate": { "loop_id": loop_id, "work": work },
                    "exit_code": 0,
                }),
            },
            message: None,
        })
    }
}

pub fn register_control_plane_action_delegate() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_core_action_delegate(std::sync::Arc::new(LoopActionDelegate));
    });
}

pub fn loop_runtime_metadata(metadata: &Value) -> Value {
    metadata.get("runtime").cloned().unwrap_or_else(|| {
        serde_json::json!({
            "on": true,
            "state": "on",
            "revolutions": 0,
            "continuation_policy": {
                "mode": "until_stopped_or_revolution_limit",
                "resume_command": "homeboy agent-task loop resume <loop-id>",
                "stop_command": "homeboy agent-task loop stop <loop-id>"
            }
        })
    })
}

pub fn stamp_loop_runtime_metadata(
    metadata: &mut Value,
    on: bool,
    revolution_limit: Option<u32>,
    increment_revolution: bool,
) -> Result<()> {
    if metadata.is_null() {
        *metadata = serde_json::json!({});
    }
    let Some(object) = metadata.as_object_mut() else {
        return Err(Error::validation_invalid_argument(
            "metadata",
            "loop runtime metadata requires object metadata",
            Some(metadata.to_string()),
            None,
        ));
    };
    let runtime = object
        .entry("runtime".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !runtime.is_object() {
        *runtime = serde_json::json!({});
    }
    let runtime = runtime.as_object_mut().expect("runtime object");
    let current = runtime
        .get("revolutions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    runtime.insert("on".to_string(), Value::Bool(on));
    runtime.insert(
        "state".to_string(),
        Value::String(if on { "on" } else { "off" }.to_string()),
    );
    runtime.insert(
        "revolutions".to_string(),
        Value::Number(serde_json::Number::from(if increment_revolution {
            current + 1
        } else {
            current
        })),
    );
    if let Some(limit) = revolution_limit {
        runtime.insert("revolution_limit".to_string(), Value::Number(limit.into()));
    }
    runtime.insert(
        "continuation_policy".to_string(),
        serde_json::json!({
            "mode": "until_stopped_or_revolution_limit",
            "resume_command": "homeboy agent-task loop resume <loop-id>",
            "stop_command": "homeboy agent-task loop stop <loop-id>"
        }),
    );
    Ok(())
}

pub fn controller_status_report(loop_id: &str) -> Result<AgentTaskLoopControllerStatusReport> {
    // Status is a bounded read. Reconciliation belongs to the supervising work
    // job and must not be triggered by a CLI or daemon inspection.
    let controller = load_controller(loop_id)?;
    let diagnostics = controller_status_diagnostics(&controller)?;
    Ok(AgentTaskLoopControllerStatusReport {
        schema: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
        controller,
        diagnostics,
    })
}

/// Read the canonical loop resource and its compatibility/domain adjuncts.
/// This function is deliberately bounded and non-reconciling: legacy JSON is
/// read in memory when its SQLite projection has not yet been prepared.
pub fn loop_read(loop_id: &str) -> Result<AgentTaskLoopReadResult> {
    let report = controller_status_report(loop_id)?;
    let run_id = control_plane_run_id(loop_id)?;
    let resource = match homeboy_core::control_plane::run(&run_id) {
        Ok(resource) => resource,
        Err(error)
            if error.class == homeboy_control_plane_contract::ControlPlaneErrorClass::NotFound =>
        {
            legacy_loop_resource(&run_id, &report.controller)
        }
        Err(error) => {
            return Err(Error::internal_unexpected(error.message));
        }
    };
    Ok(AgentTaskLoopReadResult {
        schema: "homeboy/agent-task-loop-read-result/v1".to_string(),
        work: loop_work_status(&report.controller.metadata),
        resource,
        controller: report.controller,
        diagnostics: report.diagnostics,
    })
}

fn legacy_loop_resource(run_id: &RunId, record: &AgentTaskLoopControllerRecord) -> ControlPlaneRun {
    let mut resource = ControlPlaneRun::new(run_id.clone());
    resource.state = match record.state {
        AgentTaskLoopControllerState::Running
        | AgentTaskLoopControllerState::Waiting
        | AgentTaskLoopControllerState::HumanReady => ControlPlaneRunState::Running,
        AgentTaskLoopControllerState::Completed => ControlPlaneRunState::Succeeded,
        AgentTaskLoopControllerState::Failed | AgentTaskLoopControllerState::Escalated => {
            ControlPlaneRunState::Failed
        }
        AgentTaskLoopControllerState::Abandoned => ControlPlaneRunState::Cancelled,
    };
    resource.phase = Some(record.phase.clone());
    resource.created_at = record.created_at.clone();
    resource.updated_at = Some(record.updated_at.clone());
    resource.finished_at = (record.state != AgentTaskLoopControllerState::Running
        && record.state != AgentTaskLoopControllerState::Waiting
        && record.state != AgentTaskLoopControllerState::HumanReady)
        .then(|| record.updated_at.clone());
    resource
}

pub fn controller_status_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Result<AgentTaskLoopControllerDiagnostics> {
    controller_status_diagnostics_with(record, Utc::now(), |run_id| {
        agent_task_lifecycle::run_record_exists(run_id)
    })
}

pub(crate) fn controller_status_diagnostics_with<F>(
    record: &AgentTaskLoopControllerRecord,
    now: DateTime<Utc>,
    mut run_exists: F,
) -> Result<AgentTaskLoopControllerDiagnostics>
where
    F: FnMut(&str) -> Result<bool>,
{
    let mut pending_actions = Vec::new();
    let mut stale_pending_action_count = 0;
    let mut orphaned_pending_action_count = 0;
    let acceptance_gates = acceptance_gate_diagnostics(record);
    let failed_child_actions = failed_child_action_diagnostics(record);
    let controller_state = controller_state_diagnostic(record);
    let relevant_action = relevant_action_diagnostic(record);
    let next_commands =
        controller_next_commands(record, &controller_state, relevant_action.as_ref());
    let missing_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopGateStatus::Missing)
        .count();
    let failed_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopGateStatus::Failed)
        .count();
    let pending_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopGateStatus::Pending)
        .count();

    for action in record
        .next_actions
        .iter()
        .filter(|action| action.status == AgentTaskLoopActionStatus::Pending)
    {
        let age_seconds = parse_timestamp(&action.created_at).map(|created_at| {
            now.signed_duration_since(created_at.with_timezone(&Utc))
                .num_seconds()
                .max(0)
        });
        let stale = age_seconds.is_some_and(|age| age >= STALE_PENDING_ACTION_SECONDS);
        let runner_id = action_runner_id(action, record);
        let referenced_run_id = action_referenced_run_id(action, record);
        let missing_referenced_run = if let Some(run_id) = referenced_run_id.as_deref() {
            !run_exists(run_id)?
        } else {
            false
        };
        let orphaned = missing_referenced_run;
        let mut problems = Vec::new();
        if stale {
            problems.push("pending action is older than stale threshold".to_string());
        }
        if missing_referenced_run {
            problems.push("referenced run record is missing".to_string());
        }
        let recovery_commands = if stale || orphaned {
            recovery_commands_for(record, action)
        } else {
            Vec::new()
        };

        if stale {
            stale_pending_action_count += 1;
        }
        if orphaned {
            orphaned_pending_action_count += 1;
        }
        pending_actions.push(AgentTaskLoopPendingActionDiagnostic {
            action_id: action.action_id.clone(),
            action: action_name(&action.action).to_string(),
            dedupe_key: action.dedupe_key.clone(),
            runner_id,
            referenced_run_id,
            created_at: action.created_at.clone(),
            age_seconds,
            stale,
            orphaned,
            problems,
            recovery_commands,
        });
    }

    Ok(AgentTaskLoopControllerDiagnostics {
        schema: "homeboy/agent-task-loop-controller-diagnostics/v1".to_string(),
        stale_pending_threshold_seconds: STALE_PENDING_ACTION_SECONDS,
        summary: AgentTaskLoopControllerDiagnosticSummary {
            pending_action_count: pending_actions.len(),
            failed_child_action_count: failed_child_actions.len(),
            stale_pending_action_count,
            orphaned_pending_action_count,
            acceptance_gate_count: acceptance_gates.len(),
            missing_acceptance_gate_count,
            failed_acceptance_gate_count,
            pending_acceptance_gate_count,
        },
        controller_state,
        relevant_action,
        next_commands,
        failed_child_actions,
        pending_actions,
        acceptance_gates,
    })
}

fn controller_state_diagnostic(
    record: &AgentTaskLoopControllerRecord,
) -> AgentTaskLoopControllerStateDiagnostic {
    if runtime_is_off(&record.metadata) {
        return AgentTaskLoopControllerStateDiagnostic {
            state: "paused_off".to_string(),
            label: "paused/off".to_string(),
            actionable: true,
            reason:
                "loop runtime metadata is off; resume only after intentionally turning the loop on"
                    .to_string(),
        };
    }

    if record
        .next_actions
        .iter()
        .any(|action| action.status == AgentTaskLoopActionStatus::Running)
    {
        return AgentTaskLoopControllerStateDiagnostic {
            state: "running_active_work".to_string(),
            label: "running with active work".to_string(),
            actionable: false,
            reason: "at least one controller action is currently running".to_string(),
        };
    }

    if record
        .next_actions
        .iter()
        .any(|action| action.status == AgentTaskLoopActionStatus::WaitingForRunner)
    {
        return AgentTaskLoopControllerStateDiagnostic {
            state: "waiting_for_runner".to_string(),
            label: "waiting for runner".to_string(),
            actionable: false,
            reason: "a Lab runner accepted a controller action and remains authoritative until it reports a terminal result".to_string(),
        };
    }

    if record.next_actions.iter().any(is_failed_or_blocked_action) {
        return AgentTaskLoopControllerStateDiagnostic {
            state: "running_blocked_failed_action".to_string(),
            label: "running but blocked on failed action".to_string(),
            actionable: true,
            reason: "controller is marked running, but a failed or blocked action must be resolved before ordinary progress is safe".to_string(),
        };
    }

    if record
        .next_actions
        .iter()
        .any(|action| action.status == AgentTaskLoopActionStatus::Pending)
    {
        return AgentTaskLoopControllerStateDiagnostic {
            state: "running_pending_work".to_string(),
            label: "running with pending work".to_string(),
            actionable: true,
            reason: "pending controller actions are available to run".to_string(),
        };
    }

    AgentTaskLoopControllerStateDiagnostic {
        state: format!("{:?}", record.state).to_ascii_lowercase(),
        label: format!("{:?}", record.state).to_ascii_lowercase(),
        actionable: !matches!(record.state, AgentTaskLoopControllerState::Completed),
        reason: "no failed, running, or pending action is recorded".to_string(),
    }
}

fn runtime_is_off(metadata: &Value) -> bool {
    metadata
        .get("runtime")
        .and_then(|runtime| runtime.get("on"))
        .and_then(Value::as_bool)
        == Some(false)
}

fn relevant_action_diagnostic(
    record: &AgentTaskLoopControllerRecord,
) -> Option<AgentTaskLoopRelevantActionDiagnostic> {
    let action = record
        .next_actions
        .iter()
        .rev()
        .find(|action| is_failed_or_blocked_action(action))
        .or_else(|| {
            record
                .next_actions
                .iter()
                .find(|action| action.status == AgentTaskLoopActionStatus::Running)
        })
        .or_else(|| {
            record
                .next_actions
                .iter()
                .find(|action| action.status == AgentTaskLoopActionStatus::WaitingForRunner)
        })
        .or_else(|| {
            record
                .next_actions
                .iter()
                .find(|action| action.status == AgentTaskLoopActionStatus::Pending)
        })?;
    let action_value = action_value(action);
    Some(AgentTaskLoopRelevantActionDiagnostic {
        action_id: action.action_id.clone(),
        action: action_name(&action.action).to_string(),
        status: action.status,
        dedupe_key: action.dedupe_key.clone(),
        selected_executor: selected_executor_diagnostic(action_value.as_ref(), &record.metadata),
        referenced_run_id: action_referenced_run_id(action, record),
    })
}

fn selected_executor_diagnostic(
    action: Option<&Value>,
    metadata: &Value,
) -> Option<AgentTaskLoopSelectedExecutorDiagnostic> {
    let executor = AgentTaskLoopSelectedExecutorDiagnostic {
        backend: first_dispatch_backend(action, metadata),
        selector: first_dispatch_selector(action, metadata),
        model: first_dispatch_model(action, metadata),
    };
    (executor.backend.is_some() || executor.selector.is_some() || executor.model.is_some())
        .then_some(executor)
}

fn controller_next_commands(
    record: &AgentTaskLoopControllerRecord,
    controller_state: &AgentTaskLoopControllerStateDiagnostic,
    relevant_action: Option<&AgentTaskLoopRelevantActionDiagnostic>,
) -> Vec<String> {
    let loop_id = shell_arg(&record.loop_id);
    match controller_state.state.as_str() {
        "paused_off" => vec![format!(
            "homeboy agent-task loop resume {loop_id}  # turns the loop back on and resumes pending work"
        )],
        "running_active_work" => vec![format!(
            "homeboy agent-task controller status {loop_id}  # active work is still running"
        )],
        "waiting_for_runner" => vec![format!(
            "homeboy agent-task controller status {loop_id}  # Lab owns the accepted action until its terminal result is reconciled"
        )],
        "running_blocked_failed_action" => {
            let mut commands = vec![format!(
                "homeboy agent-task controller diagnose {loop_id}  # inspect failed action evidence"
            )];
            if let Some(action) = relevant_action {
                commands.push(format!(
                    "homeboy agent-task controller run {loop_id} --action-id {}  # retry this persisted action",
                    shell_arg(&action.action_id)
                ));
            }
            commands.push("homeboy agent-task controller from-spec <spec> --resume --fork  # start fresh with a new backend/spec without changing this state".to_string());
            commands.push("homeboy agent-task controller from-spec <spec> --resume --replace  # discard this persisted controller state".to_string());
            commands.push("homeboy agent-task controller from-spec <spec> --resume-existing  # intentionally continue this persisted state".to_string());
            commands
        }
        "running_pending_work" => vec![format!("homeboy agent-task controller resume {loop_id}")],
        _ => vec![format!("homeboy agent-task controller status {loop_id}")],
    }
}

fn is_failed_or_blocked_action(action: &AgentTaskLoopPolicyActionRecord) -> bool {
    matches!(
        action.status,
        AgentTaskLoopActionStatus::Failed
            | AgentTaskLoopActionStatus::BlockedRunnerUnavailable
            | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
            | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
    )
}

fn failed_child_action_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Vec<AgentTaskLoopFailedChildActionDiagnostic> {
    record
        .next_actions
        .iter()
        .filter(|action| {
            matches!(
                action.status,
                AgentTaskLoopActionStatus::Failed
                    | AgentTaskLoopActionStatus::BlockedRunnerUnavailable
                    | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
                    | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
            )
        })
        .map(|action| failed_child_action_diagnostic(record, action))
        .collect()
}

fn failed_child_action_diagnostic(
    record: &AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
) -> AgentTaskLoopFailedChildActionDiagnostic {
    let child_run_id = action_referenced_run_id(action, record);
    let child_run = child_run_id
        .as_deref()
        .and_then(|run_id| agent_task_lifecycle::reconcile_status(run_id).ok());
    let child_run_status = child_run
        .as_ref()
        .map(|run| format!("{:?}", run.state).to_ascii_lowercase());
    let aggregate = child_run_id.as_deref().and_then(load_child_aggregate_value);
    let child_task_id = aggregate.as_ref().and_then(first_failed_task_id);
    let top_diagnostic = action_top_diagnostic(action)
        .or_else(|| aggregate.as_ref().and_then(first_diagnostic))
        .unwrap_or_else(|| CollectedDiagnostic {
            class: "controller_child_action_failed".to_string(),
            message: "controller child action failed".to_string(),
        });
    let hydrated_root_cause = child_run
        .as_ref()
        .and_then(root_cause_from_run_evidence)
        .or_else(|| aggregate.as_ref().and_then(root_cause_from_aggregate))
        .filter(|message| message != &top_diagnostic.message)
        .filter(|message| {
            diagnostic_priority("", message) <= diagnostic_priority("", &top_diagnostic.message)
        });
    let evidence_refs = child_run
        .as_ref()
        .map(evidence_refs_from_run)
        .unwrap_or_default();
    let artifact_dir = child_run.as_ref().and_then(run_artifact_dir);
    let owner_surface = classify_failed_child_owner(
        hydrated_root_cause
            .as_deref()
            .unwrap_or(&top_diagnostic.message),
        &evidence_refs,
    );
    let signature_root = hydrated_root_cause
        .clone()
        .unwrap_or_else(|| top_diagnostic.message.clone());
    let failure_signature = failed_child_failure_signature(
        child_run_id.as_deref(),
        child_task_id.as_deref(),
        Some(top_diagnostic.class.as_str()),
        &signature_root,
        &owner_surface,
    );
    let repeated_failure = repeated_failure_diagnostic(record, &failure_signature);
    let next_command = child_run_id
        .as_ref()
        .map(|run_id| format!("homeboy agent-task status {run_id}"))
        .unwrap_or_else(|| {
            format!(
                "homeboy agent-task controller run {} --action-id {}",
                record.loop_id, action.action_id
            )
        });

    AgentTaskLoopFailedChildActionDiagnostic {
        action_id: action.action_id.clone(),
        dedupe_key: action.dedupe_key.clone(),
        child_run_id,
        child_task_id,
        child_run_status,
        top_diagnostic: top_diagnostic.message,
        top_diagnostic_class: Some(top_diagnostic.class),
        hydrated_root_cause,
        artifact_dir,
        owner_surface,
        failure_signature,
        repeated_failure,
        next_command,
        evidence_refs,
    }
}

fn action_top_diagnostic(action: &AgentTaskLoopPolicyActionRecord) -> Option<CollectedDiagnostic> {
    action
        .diagnostics
        .first()
        .map(|diagnostic| CollectedDiagnostic {
            class: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
        })
}

fn first_failed_task_id(value: &Value) -> Option<String> {
    value
        .get("outcomes")
        .and_then(Value::as_array)?
        .iter()
        .find(|outcome| {
            outcome
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status != "succeeded" && status != "no_op")
        })
        .and_then(|outcome| outcome.get("task_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn run_artifact_dir(run: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    run.aggregate_path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .map(|path| path.display().to_string())
}

fn failed_child_failure_signature(
    child_run_id: Option<&str>,
    task_id: Option<&str>,
    diagnostic_class: Option<&str>,
    root_message: &str,
    owner_surface: &str,
) -> AgentTaskLoopFailureSignature {
    let normalized_message = normalize_signature_text(root_message);
    let signature_material = format!(
        "{}\n{}\n{}",
        owner_surface,
        diagnostic_class.unwrap_or(""),
        normalized_message
    );
    let digest = format!(
        "sha256:{}",
        content_hash::sha256_hex(signature_material.as_bytes())
    );
    AgentTaskLoopFailureSignature {
        digest,
        task_id: task_id.or(child_run_id).map(str::to_string),
        diagnostic_class: diagnostic_class.map(str::to_string),
        root_message: root_message.to_string(),
        owner_surface: owner_surface.to_string(),
    }
}

fn repeated_failure_diagnostic(
    record: &AgentTaskLoopControllerRecord,
    signature: &AgentTaskLoopFailureSignature,
) -> Option<AgentTaskLoopRepeatedFailureDiagnostic> {
    let matching_failed_child_action_count = record
        .next_actions
        .iter()
        .filter(|action| {
            matches!(action.status, AgentTaskLoopActionStatus::Failed)
                && action_top_diagnostic(action).is_some_and(|diagnostic| {
                    failed_child_failure_signature(
                        action_referenced_run_id(action, record).as_deref(),
                        None,
                        Some(diagnostic.class.as_str()),
                        &diagnostic.message,
                        &signature.owner_surface,
                    )
                    .digest
                        == signature.digest
                })
        })
        .count();
    (matching_failed_child_action_count > 1).then(|| AgentTaskLoopRepeatedFailureDiagnostic {
        matching_failed_child_action_count,
        guidance: "This failure signature has repeated in this controller; inspect the child input or provider boundary before another full rerun.".to_string(),
        next_command: "homeboy agent-task evidence <child-run-id> --failure-only".to_string(),
    })
}

fn normalize_signature_text(message: &str) -> String {
    message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn load_child_aggregate_value(run_id: &str) -> Option<Value> {
    let (raw, _) = agent_task_lifecycle::aggregate_source(run_id).ok()?;
    serde_json::from_str(&raw).ok()
}

fn evidence_refs_from_run(
    run: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Vec<AgentTaskEvidenceRef> {
    let mut refs = Vec::new();
    for artifact in &run.artifact_refs {
        push_failed_child_evidence_ref(
            &mut refs,
            AgentTaskEvidenceRef {
                kind: artifact.kind.clone(),
                uri: artifact.uri.clone(),
                label: artifact.label.clone(),
            },
        );
    }
    if let Some(executor) = &run.latest_executor_evidence {
        // `refs()` already yields `AgentTaskEvidenceRef`; before #10310 this had
        // to be re-spelled field-by-field into a structurally identical clone.
        for evidence in executor.refs() {
            push_failed_child_evidence_ref(&mut refs, evidence);
        }
    }
    refs
}

fn root_cause_from_run_evidence(run: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    let executor = run.latest_executor_evidence.as_ref()?;
    let mut candidates = Vec::new();
    for evidence in executor.refs() {
        let Some(path) = evidence.uri.strip_prefix("file://") else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        candidates.extend(collect_diagnostics(&value));
    }
    candidates
        .sort_by_key(|diagnostic| diagnostic_priority(&diagnostic.class, &diagnostic.message));
    candidates
        .into_iter()
        .find(|diagnostic| is_root_cause_message(&diagnostic.message))
        .map(|diagnostic| diagnostic.message)
}

fn push_failed_child_evidence_ref(
    refs: &mut Vec<AgentTaskEvidenceRef>,
    reference: AgentTaskEvidenceRef,
) {
    if reference.uri.trim().is_empty() {
        return;
    }
    if !refs
        .iter()
        .any(|existing| existing.kind == reference.kind && existing.uri == reference.uri)
    {
        refs.push(reference);
    }
}

fn first_diagnostic(value: &Value) -> Option<CollectedDiagnostic> {
    collect_diagnostics(value).into_iter().next()
}

fn root_cause_from_aggregate(value: &Value) -> Option<String> {
    collect_diagnostics(value)
        .into_iter()
        .find(|diagnostic| is_root_cause_message(&diagnostic.message))
        .map(|diagnostic| diagnostic.message)
}

#[derive(Clone)]
struct CollectedDiagnostic {
    class: String,
    message: String,
}

fn collect_diagnostics(value: &Value) -> Vec<CollectedDiagnostic> {
    let mut diagnostics = Vec::new();
    collect_diagnostics_into(value, &mut diagnostics);
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|diagnostic| {
        seen.insert((
            diagnostic.class.to_ascii_lowercase(),
            diagnostic.message.clone(),
        ))
    });
    diagnostics
        .sort_by_key(|diagnostic| diagnostic_priority(&diagnostic.class, &diagnostic.message));
    diagnostics
}

fn collect_diagnostics_into(value: &Value, diagnostics: &mut Vec<CollectedDiagnostic>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("diagnostics") {
                for diagnostic in items {
                    if let Some(message) = diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|message| !message.is_empty())
                    {
                        let class = diagnostic
                            .get("class")
                            .or_else(|| diagnostic.get("kind"))
                            .or_else(|| diagnostic.get("level"))
                            .and_then(Value::as_str)
                            .unwrap_or("nested")
                            .to_string();
                        diagnostics.push(CollectedDiagnostic {
                            class,
                            message: message.to_string(),
                        });
                    }
                }
            }
            for nested in map.values() {
                collect_diagnostics_into(nested, diagnostics);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_diagnostics_into(nested, diagnostics);
            }
        }
        _ => {}
    }
}

fn diagnostic_priority(class: &str, message: &str) -> u8 {
    let text = format!("{} {}", class, message).to_ascii_lowercase();
    if text.contains("typed_artifacts_missing")
        || text.contains("required_typed_artifacts_missing")
        || text.contains("required typed artifacts")
        || text.contains("declared artifact result envelope")
    {
        8
    } else if text.contains("valid") || text.contains("recipe") || text.contains("schema") {
        0
    } else if text.contains("fatal") || text.contains("error") || text.contains("exception") {
        1
    } else if text.contains("registr")
        || text.contains("provider")
        || text.contains("discovery")
        || text.contains("capability")
    {
        2
    } else if text.contains("missing")
        || text.contains("not_found")
        || text.contains("path")
        || text.contains("io")
    {
        3
    } else {
        9
    }
}

fn is_root_cause_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("runtime_task_ability_unavailable")
        || lower.contains("root cause")
        || lower.contains("recipe")
        || lower.contains("validation")
        || lower.contains("php fatal")
        || lower.contains("fatal error")
        || lower.contains("missing required")
        || lower.contains("provider")
        || lower.contains("credential")
        || lower.contains("secret")
}

fn classify_failed_child_owner(diagnostic: &str, evidence_refs: &[AgentTaskEvidenceRef]) -> String {
    let lower = diagnostic.to_ascii_lowercase();
    if lower.contains("runtime_task_ability_unavailable") || lower.contains("ability unavailable") {
        "agent_runtime".to_string()
    } else if lower.contains("credential") || lower.contains("secret") || lower.contains("token") {
        "provider_credentials".to_string()
    } else if lower.contains("repo spec")
        || lower.contains("spec")
        || lower.contains("invalid input")
    {
        "repo_spec".to_string()
    } else if lower.contains("artifact") {
        "workload_artifacts".to_string()
    } else if lower.contains("agent runtime")
        || lower.contains("agent_runtime")
        || lower.contains("runtime")
        || evidence_refs
            .iter()
            .any(|reference| reference.uri.to_ascii_lowercase().contains("runtime"))
    {
        "agent_runtime".to_string()
    } else if lower.contains("provider") {
        "provider".to_string()
    } else {
        "homeboy".to_string()
    }
}

fn acceptance_gate_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Vec<AgentTaskLoopAcceptanceGateDiagnostic> {
    let mut declared = BTreeSet::new();
    for action in &record.next_actions {
        if let AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } = &action.action
        {
            declared.insert((bundle_id.clone(), entity_id.clone()));
        }
    }
    for result in &record.gate_results {
        declared.insert((result.bundle_id.clone(), result.entity_id.clone()));
    }
    for bundle in &record.gate_bundles {
        if !declared
            .iter()
            .any(|(bundle_id, _)| bundle_id == &bundle.bundle_id)
        {
            declared.insert((bundle.bundle_id.clone(), None));
        }
    }

    declared
        .into_iter()
        .map(|(bundle_id, entity_id)| {
            let result = record
                .gate_results
                .iter()
                .rev()
                .find(|result| result.bundle_id == bundle_id && result.entity_id == entity_id);
            let status = AgentTaskLoopGateStatus::from(result.map(|result| result.status));
            let problems = match status {
                AgentTaskLoopGateStatus::Missing => {
                    vec!["acceptance gate has no recorded result".to_string()]
                }
                AgentTaskLoopGateStatus::Failed => {
                    vec!["acceptance gate recorded a failed result".to_string()]
                }
                AgentTaskLoopGateStatus::Pending => {
                    vec!["acceptance gate is pending an external/manual result".to_string()]
                }
                AgentTaskLoopGateStatus::Satisfied => Vec::new(),
            };

            AgentTaskLoopAcceptanceGateDiagnostic {
                bundle_id,
                entity_id,
                status,
                result_id: result.map(|result| result.result_id.clone()),
                result_status: result.map(|result| result.status),
                recorded_at: result.map(|result| result.recorded_at.clone()),
                problems,
            }
        })
        .collect()
}

pub fn create_controller(
    loop_id: &str,
    phase: &str,
    config_version: &str,
) -> Result<AgentTaskLoopControllerRecord> {
    let record = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
    write_controller(&record)?;
    Ok(record)
}

pub fn load_controller(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    read_json(&controller_path(&sanitize_loop_id(loop_id))?)
}

pub fn controller_status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    let refreshed_child_runs = refresh_stale_running_child_actions(&mut record)?;
    let refreshed_subcontrollers = refresh_subcontroller_statuses(&mut record)?;
    if refreshed_child_runs || refreshed_subcontrollers {
        write_controller(&record)?;
    }
    Ok(record)
}

pub fn list_controllers() -> Result<Vec<AgentTaskLoopControllerRecord>> {
    let root = controllers_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(root.display().to_string()),
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        let path = entry.path().join("controller.json");
        if path.exists() {
            records.push(read_json(&path)?);
        }
    }
    records.sort_by(|left: &AgentTaskLoopControllerRecord, right| left.loop_id.cmp(&right.loop_id));
    Ok(records)
}

pub fn write_controller(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    write_json(&controller_path(&record.loop_id)?, record)?;
    publish_control_plane_loop(record)
}

/// Rebuild the SQLite projection for a legacy or partially published JSON
/// controller. This is intentionally explicit and never called by status.
pub fn prepare_control_plane_loop(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    publish_control_plane_loop(record)
}

fn publish_control_plane_loop(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    let run_id = control_plane_run_id(&record.loop_id)?;
    let store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let observation = store.open_observation_initialized()?;
    let status = match record.state {
        AgentTaskLoopControllerState::Running
        | AgentTaskLoopControllerState::Waiting
        | AgentTaskLoopControllerState::HumanReady => "running",
        AgentTaskLoopControllerState::Completed => "pass",
        AgentTaskLoopControllerState::Failed | AgentTaskLoopControllerState::Escalated => "fail",
        AgentTaskLoopControllerState::Abandoned => "cancelled",
    };
    let metadata = serde_json::json!({
        "loop_id": record.loop_id,
        "controller": record,
            "control_plane": {
                "phase": record.phase,
                "actions": [{
                    "action": "cancel",
                    "availability": if status == "running" { "available" } else { "unavailable" },
                "reason": if status == "running" { "loop can be stopped" } else { "loop is terminal" },
                "confirmation": "required",
                "idempotent": true,
                    "requires_revalidation": true,
                    "result_resource_type": "agent_task_loop"
                }, {
                    "action": "resume",
                    "availability": if status == "running" { "available" } else { "unavailable" },
                    "reason": if status == "running" { "loop can be resumed" } else { "loop is terminal" },
                    "confirmation": "required",
                    "idempotent": true,
                    "requires_revalidation": true,
                    "result_resource_type": "agent_task_loop"
                }]
            }
    });
    let run = RunRecord {
        id: run_id.to_string(),
        kind: "agent-task-loop".to_string(),
        component_id: None,
        started_at: record.created_at.clone(),
        finished_at: (status != "running").then(|| record.updated_at.clone()),
        status: status.to_string(),
        command: Some("homeboy agent-task loop".to_string()),
        cwd: None,
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: metadata,
    };
    let projection = ControlPlaneResourceProjection {
        resource_type: LOOP_CONTROL_PLANE_RESOURCE_TYPE.to_string(),
        resource_id: run_id.to_string(),
        version: record.updated_at.clone(),
        state: status.to_string(),
        aliases: vec![record.loop_id.clone()],
        eligibility: serde_json::json!({ "actions": ["cancel"] }),
        provenance: serde_json::json!({
            "source": "agent_task_loop_controller",
            "loop_id": record.loop_id,
            "identity_mapping": "loop-id-to-control-plane-run/v1"
        }),
    };
    observation.upsert_imported_run_with_resource_projection(&run, &projection, false)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

pub fn controller_record_path(loop_id: &str) -> Result<PathBuf> {
    controller_path(loop_id)
}

fn controller_path(loop_id: &str) -> Result<PathBuf> {
    Ok(controllers_root()?
        .join(sanitize_loop_id(loop_id))
        .join("controller.json"))
}

fn controllers_root() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-loops"))
}
