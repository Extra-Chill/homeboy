use super::*;
use homeboy_control_plane_contract::{
    ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEventAppendRequest,
    ControlPlaneEventSource, RunId, TaskId, CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
};
use homeboy_core::observation::PreparedControlPlaneEventAppend;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) const DURABLE_EVENT_HISTORY_METADATA_KEY: &str = "durable_event_history";
pub const EVENT_HISTORY_MIGRATION_SCHEMA: &str = "homeboy/agent-task-event-history-migration/v1";
const PROMOTION_PROGRESS_OUTPUT_TAIL_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskEventHistoryMigration {
    pub schema: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub durable_event_history: String,
}

pub(crate) fn stamp_durable_event_history(record: &mut AgentTaskRunRecord) {
    record.metadata[DURABLE_EVENT_HISTORY_METADATA_KEY] =
        json!(crate::orchestration::EVENT_STREAM_DURABLE_PROGRESS);
}

pub(crate) fn has_durable_event_history(record: &AgentTaskRunRecord) -> bool {
    record
        .metadata
        .get(DURABLE_EVENT_HISTORY_METADATA_KEY)
        .and_then(Value::as_str)
        == Some(crate::orchestration::EVENT_STREAM_DURABLE_PROGRESS)
}

/// Persist recoverable historical progress and unreceipted claims onto the
/// canonical ledger for one exact run. Uses the locked atomic writer without
/// workspace-claim renewal, terminal projection, provider execution, or
/// lifecycle-state change.
pub fn migrate_durable_event_history(run_id: &str) -> Result<AgentTaskEventHistoryMigration> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    migrate_durable_event_history_in_store(&lifecycle_store, run_id)
}

/// [`migrate_durable_event_history`] against explicitly injected durable roots.
pub fn migrate_durable_event_history_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskEventHistoryMigration> {
    let run_id = sanitize_run_id(run_id);
    let committed = lifecycle_store.with_config_lock(|| {
        let record = lifecycle_store.read_record(&run_id)?;
        let original_state = record.state;
        let committed = lifecycle_store.write_record_locked_without_terminal_projection(&record)?;
        if committed.state != original_state {
            return Err(Error::internal_unexpected(format!(
                "event history migration changed lifecycle state for {run_id}"
            )));
        }
        Ok(committed)
    })?;
    if !has_durable_event_history(&committed) {
        return Err(Error::internal_unexpected(format!(
            "durable event history stamp missing after migration: {run_id}"
        )));
    }
    Ok(AgentTaskEventHistoryMigration {
        schema: EVENT_HISTORY_MIGRATION_SCHEMA.to_string(),
        run_id: committed.run_id,
        state: committed.state,
        durable_event_history: crate::orchestration::EVENT_STREAM_DURABLE_PROGRESS.to_string(),
    })
}

pub(crate) fn prepared_unreceipted_action_events(
    record: &AgentTaskRunRecord,
    store: &homeboy_core::observation::ObservationStore,
    receipts: &BTreeSet<String>,
    ledger: &[homeboy_control_plane_contract::ControlPlaneEvent],
) -> Result<Vec<PreparedControlPlaneEventAppend>> {
    let run = RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let mut events = Vec::new();
    let claims = record
        .metadata
        .get("cook_operation_claims")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|claim| {
            claim["operation_key"]
                .as_str()
                .is_some_and(|key| key.starts_with("control-plane-action:"))
        });
    for claim in claims {
        let operation_key = claim["operation_key"].as_str().expect("filtered claim key");
        // Action ownership outlives the retained event payloads.
        if let Some((_, effect_id)) = operation_key
            .strip_prefix("control-plane-action:")
            .and_then(|key| key.split_once(':'))
        {
            if store
                .control_plane_effect_status(&homeboy_control_plane_contract::EffectId(
                    effect_id.to_string(),
                ))?
                .is_some_and(|effect| effect.intent.resource.run == run)
            {
                continue;
            }
        }
        if canonical_action_events_own_operation(operation_key, ledger) {
            continue;
        }
        let accepted_key =
            crate::orchestration::action_event_idempotency_key(operation_key, "action.accepted");
        if !receipts.contains(&homeboy_engine_primitives::content_hash::sha256_hex(
            accepted_key.as_bytes(),
        )) {
            events.push(prepare_request(
                &run,
                record,
                action_claim_request(
                    operation_key,
                    "action.accepted",
                    claim["leased_at"].as_str().map(str::to_string),
                    homeboy_core::redaction::redact_json(&json!({
                        "operation_key": claim["operation_key"],
                        "request": claim["intent"],
                    })),
                )?,
            )?);
        }
        let Some(result) = claim.get("result") else {
            continue;
        };
        let kind = match result["outcome"].as_str() {
            Some("already_satisfied") => "action.already_satisfied",
            Some("failed") => "action.failed",
            _ => "action.succeeded",
        };
        let terminal_key = crate::orchestration::action_event_idempotency_key(operation_key, kind);
        if receipts.contains(&homeboy_engine_primitives::content_hash::sha256_hex(
            terminal_key.as_bytes(),
        )) {
            continue;
        }
        let mut terminal_data = result.clone();
        if let Some(data) = terminal_data.as_object_mut() {
            data.insert("operation_key".to_string(), json!(operation_key));
        }
        events.push(prepare_request(
            &run,
            record,
            action_claim_request(
                operation_key,
                kind,
                claim["completed_at"].as_str().map(str::to_string),
                homeboy_core::redaction::redact_json(&terminal_data),
            )?,
        )?);
    }
    Ok(events)
}

pub(crate) fn prepared_progress_events(
    record: &AgentTaskRunRecord,
    aggregate: Option<&AgentTaskAggregate>,
) -> Result<Vec<PreparedControlPlaneEventAppend>> {
    let run = RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let mut events = Vec::new();
    if aggregate.is_none() {
        for task in &record.tasks {
            events.push(prepare_request(
                &run,
                record,
                progress_request(
                    &format!("submit\0{}\0{}", record.run_id, task.task_id),
                    "task.state_changed",
                    "agent-task",
                    Some(task.task_id.as_str()),
                    None,
                    json!({
                        "state": AgentTaskState::Queued,
                        "message": "task submitted",
                        "progress": { "attempt": 1 },
                        "source_schema": AGENT_TASK_AGGREGATE_SCHEMA,
                    }),
                )?,
            )?);
        }
    }
    if let Some(executions) = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
    {
        for execution in executions {
            if let Some(request) = provider_execution_request(record, execution)? {
                events.push(prepare_request(&run, record, request)?);
            }
        }
    }
    if let Some(aggregate) = aggregate {
        for (index, event) in aggregate.events.iter().enumerate() {
            events.push(prepare_request(
                &run,
                record,
                progress_request(
                    &aggregate_event_identity(&record.run_id, &aggregate.events, index),
                    "task.state_changed",
                    "agent-task",
                    Some(event.task_id.as_str()),
                    None,
                    json!({
                        "state": event.state,
                        "message": event.message,
                        "progress": { "attempt": event.attempt },
                        "source_schema": AGENT_TASK_AGGREGATE_SCHEMA,
                    }),
                )?,
            )?);
        }
    }
    if let Some(raw_events) = record
        .metadata
        .get("runner_job_events")
        .and_then(Value::as_array)
    {
        for raw in raw_events {
            if let Some(request) = runner_job_event_request(record, raw)? {
                events.push(prepare_request(&run, record, request)?);
            }
        }
    }
    for request in gate_lifecycle_requests(record)? {
        events.push(prepare_request(&run, record, request)?);
    }
    Ok(events)
}

pub fn record_promotion_progress(
    run_id: &str,
    phase: &str,
    gate: Option<&str>,
    detail: Option<&str>,
    output_tail: Option<&str>,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_promotion_progress_in_store(&lifecycle_store, run_id, phase, gate, detail, output_tail)
}

pub fn record_promotion_progress_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    gate: Option<&str>,
    detail: Option<&str>,
    output_tail: Option<&str>,
) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    let output_tail = output_tail.map(bound_promotion_progress_output);
    let record = lifecycle_store
        .mutate_record(&run_id, |record| {
            let now = now_timestamp();
            let previous = record.metadata.get("promotion_progress").cloned();
            let started_at = previous
                .as_ref()
                .and_then(|progress| progress.get("started_at"))
                .and_then(Value::as_str)
                .unwrap_or(&now)
                .to_string();
            let elapsed_seconds = chrono::DateTime::parse_from_rfc3339(&started_at)
                .ok()
                .map(|started| {
                    chrono::Utc::now()
                        .signed_duration_since(started.with_timezone(&chrono::Utc))
                        .num_seconds()
                        .max(0) as u64
                })
                .unwrap_or(0);
            let progress = json!({
                "schema": "homeboy/agent-task-promotion-progress/v1",
                "active": phase != "terminal",
                "phase": phase,
                "gate": gate,
                "last_progress": detail,
                "detail": detail,
                "output_tail": output_tail,
                "started_at": started_at,
                "updated_at": now,
                "elapsed_seconds": elapsed_seconds,
                "owner_pid": std::process::id(),
            });
            record.metadata["promotion_progress"] = progress;
            record.updated_at = Some(now);
            update_lifecycle_heartbeat(record);
            true
        })?
        .ok_or_else(|| Error::internal_unexpected("promotion progress record was unchanged"))?;
    let run = RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let kind = if phase == "gate" && detail == Some("gate process started") {
        "gate.started"
    } else if phase == "gate" {
        "gate.heartbeat"
    } else {
        "promotion.progress"
    };
    let request = progress_request(
        &format!("promotion\0{}\0{}\0{}", record.run_id, kind, Uuid::new_v4()),
        kind,
        "agent-task-promotion",
        record.tasks.first().map(|task| task.task_id.as_str()),
        None,
        json!({
            "state": AgentTaskState::Running,
            "message": detail,
            "phase": phase,
            "gate": gate.or_else(|| {
                record
                    .metadata
                    .pointer("/promotion_progress/gate")
                    .and_then(Value::as_str)
            }),
            "heartbeat_at": record.updated_at,
            "output_tail": output_tail,
        }),
    )?;
    let prepared = prepare_request(&run, &record, request)?;
    lifecycle_store
        .open_observation_initialized()?
        .append_control_plane_event(
            &run,
            &prepared.request,
            &prepared.idempotency_digest,
            &prepared.request_digest,
        )
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    Ok(())
}

/// Control-plane event data is bounded at 64 KiB. Gate subprocess capture is
/// bounded independently, but a long final line can still fill that budget
/// once the heartbeat envelope is added. Keep the durable heartbeat useful
/// without allowing diagnostic output to abort the gate itself.
fn bound_promotion_progress_output(output: &str) -> String {
    if output.len() <= PROMOTION_PROGRESS_OUTPUT_TAIL_MAX_BYTES {
        return output.to_string();
    }
    let start = output.len() - PROMOTION_PROGRESS_OUTPUT_TAIL_MAX_BYTES;
    let start = output
        .char_indices()
        .find(|(index, _)| *index >= start)
        .map(|(index, _)| index)
        .unwrap_or(0);
    format!("[output truncated]\n{}", &output[start..])
}

/// The phase that failed a run is absent from its own log whenever a
/// promotion runs deterministic gates: `latest_promotion`/`promotions` were
/// durable evidence, but no gate start, per-gate result, or terminal event
/// ever reached the control-plane event stream (#14735). Every promotion
/// checkpoint that reached the gate phase is projected here, so `agent-task
/// logs` carries the same gate lifecycle regardless of whether promotion ran
/// inline or was resumed later.
fn gate_lifecycle_requests(
    record: &AgentTaskRunRecord,
) -> Result<Vec<ControlPlaneEventAppendRequest>> {
    let promotions: Vec<Value> = record
        .metadata
        .get("promotions")
        .and_then(Value::as_array)
        .filter(|promotions| !promotions.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            record
                .metadata
                .get("latest_promotion")
                .cloned()
                .into_iter()
                .collect()
        });
    let task_id = record
        .tasks
        .first()
        .map(|task| task.task_id.clone())
        .filter(|task_id| !task_id.is_empty());
    let mut requests = Vec::new();
    for (index, promotion) in promotions.iter().enumerate() {
        requests.extend(gate_lifecycle_requests_for_promotion(
            record,
            promotion,
            index,
            task_id.as_deref(),
        )?);
    }
    Ok(requests)
}

fn gate_lifecycle_requests_for_promotion(
    record: &AgentTaskRunRecord,
    promotion: &Value,
    promotion_index: usize,
    task_id: Option<&str>,
) -> Result<Vec<ControlPlaneEventAppendRequest>> {
    let Some(status) = promotion.get("status").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    // A gate phase never ran for these statuses: the patch was never applied
    // (`dry_run`) or verification has not completed yet (`verification_pending`).
    if matches!(status, "dry_run" | "verification_pending") {
        return Ok(Vec::new());
    }
    let Some(gates) = promotion
        .get("deterministic_gates")
        .or_else(|| promotion.get("gate_results"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let task_id = task_id
        .or_else(|| promotion.pointer("/source/task_id").and_then(Value::as_str))
        .unwrap_or("promotion");
    let mut requests = Vec::new();
    requests.push(progress_request(
        &format!("gate\0{}\0{promotion_index}\0started", record.run_id),
        "gate.started",
        "agent-task-gate",
        Some(task_id),
        None,
        json!({
            "state": AgentTaskState::Running,
            "message": format!("running {} deterministic gate(s)", gates.len()),
            "progress": { "attempt": promotion_index as u32 + 1 },
            "promotion_index": promotion_index,
            "gate_count": gates.len(),
        }),
    )?);
    for (gate_index, gate) in gates.iter().enumerate() {
        let gate_status = gate.get("status").and_then(Value::as_str).unwrap_or("");
        let gate_passed = matches!(gate_status, "succeeded" | "passed" | "skipped");
        let gate_name = gate_display_name(gate);
        requests.push(progress_request(
            &format!(
                "gate\0{}\0{promotion_index}\0result\0{gate_index}",
                record.run_id
            ),
            "gate.result",
            "agent-task-gate",
            Some(task_id),
            None,
            json!({
                "state": if gate_passed { AgentTaskState::Running } else { AgentTaskState::Failed },
                "message": format!("gate `{gate_name}` {gate_status}"),
                "progress": { "attempt": promotion_index as u32 + 1 },
                "promotion_index": promotion_index,
                "gate_index": gate_index,
                "gate": gate_name,
                "status": gate_status,
                "output": gate_output_reference(gate),
            }),
        )?);
    }
    let gate_phase_failed = matches!(status, "gate_failed" | "no_changes_gate_failed");
    requests.push(progress_request(
        &format!("gate\0{}\0{promotion_index}\0terminal", record.run_id),
        if gate_phase_failed {
            "gate.failed"
        } else {
            "gate.completed"
        },
        "agent-task-gate",
        Some(task_id),
        None,
        json!({
            "state": if gate_phase_failed { AgentTaskState::Failed } else { AgentTaskState::Succeeded },
            "message": format!("deterministic gate phase {status}"),
            "progress": { "attempt": promotion_index as u32 + 1 },
            "promotion_index": promotion_index,
            "status": status,
        }),
    )?);
    Ok(requests)
}

/// Best-effort human name for a gate report, mirroring
/// `homeboy_cli::commands::agent_task::status::gate_display_name`: the real
/// `AgentTaskGateReport` has no `name` field, only `id` and a `command`
/// array, with any failure detail nested under `failure_evidence`.
fn gate_display_name(gate: &Value) -> String {
    gate.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| gate.get("name").and_then(Value::as_str).map(str::to_string))
        .or_else(|| {
            gate.pointer("/failure_evidence/command")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            gate.get("command").and_then(|command| match command {
                Value::String(command) => Some(command.clone()),
                Value::Array(parts) => {
                    let joined = parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ");
                    (!joined.is_empty()).then_some(joined)
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| "deterministic gate".to_string())
}

/// A resolvable reference to the gate's own output: bounded excerpts rather
/// than the full transcript, since events are retained durably.
fn gate_output_reference(gate: &Value) -> Value {
    const MAX_EXCERPT: usize = 4096;
    let bounded = |value: &Value| -> Option<String> {
        value.as_str().map(|text| {
            if text.len() > MAX_EXCERPT {
                format!("{}…", &text[..MAX_EXCERPT])
            } else {
                text.to_string()
            }
        })
    };
    json!({
        "command": gate.pointer("/failure_evidence/command").or_else(|| gate.get("command")),
        "exit_code": gate.pointer("/failure_evidence/exit_code").or_else(|| gate.get("exit_code")),
        "summary": gate.pointer("/failure_evidence/summary"),
        "stdout_tail": gate.pointer("/failure_evidence/stdout_tail").and_then(bounded)
            .or_else(|| gate.get("stdout").and_then(bounded)),
        "stderr_tail": gate.pointer("/failure_evidence/stderr_tail").and_then(bounded)
            .or_else(|| gate.get("stderr").and_then(bounded)),
    })
}

fn prepare_request(
    run: &RunId,
    record: &AgentTaskRunRecord,
    request: ControlPlaneEventAppendRequest,
) -> Result<PreparedControlPlaneEventAppend> {
    crate::orchestration::prepare_control_plane_event_append(run, record, &request)
        .map_err(map_append_error)
}

fn provider_execution_request(
    record: &AgentTaskRunRecord,
    execution: &Value,
) -> Result<Option<ControlPlaneEventAppendRequest>> {
    let state = match execution.get("state").and_then(Value::as_str) {
        Some("running") => AgentTaskState::Running,
        Some("cancelled") => AgentTaskState::Cancelled,
        Some("timed_out") => AgentTaskState::TimedOut,
        Some("failed") => AgentTaskState::Failed,
        Some("succeeded") => AgentTaskState::Succeeded,
        _ => return Ok(None),
    };
    let Some(task_id) = execution
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let attempt = execution
        .get("attempt")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let state_name = execution["state"].as_str().unwrap_or("unknown");
    Ok(Some(progress_request(
        &format!(
            "provider-execution\0{}\0{task_id}\0{attempt}\0{state_name}",
            record.run_id
        ),
        "task.state_changed",
        "agent-task",
        Some(task_id.as_str()),
        None,
        json!({
            "state": state,
            "message": format!("provider execution {state_name}"),
            "progress": { "attempt": attempt },
            "source_schema": AGENT_TASK_AGGREGATE_SCHEMA,
        }),
    )?))
}

fn runner_job_event_request(
    record: &AgentTaskRunRecord,
    raw: &Value,
) -> Result<Option<ControlPlaneEventAppendRequest>> {
    let sequence = raw.get("sequence").and_then(Value::as_u64).unwrap_or(0);
    let job_id = raw
        .get("job_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(job_id) = job_id.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if sequence == 0 {
        return Ok(None);
    }
    let data = raw.get("data").cloned().unwrap_or(Value::Null);
    let kind = raw
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("progress");
    let timestamp_ms = raw.get("timestamp_ms").and_then(Value::as_i64);
    let occurred_at = timestamp_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|value| value.to_rfc3339());
    Ok(Some(progress_request(
        &format!("runner-job\0{}\0{job_id}\0{sequence}", record.run_id),
        &format!("runner.{kind}"),
        "lab-runner",
        None,
        occurred_at,
        json!({
            "state": AgentTaskState::Running,
            "message": raw
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| string_field(&data, "message")),
            "provider": string_field(&data, "provider")
                .or_else(|| string_field(&data, "backend")),
            "phase": string_field(&data, "phase"),
            "activity": string_field(&data, "activity")
                .or_else(|| string_field(&data, "status_note"))
                .or_else(|| string_field(&data, "progress")),
            "heartbeat_at_ms": matches!(kind, "progress" | "status")
                .then_some(timestamp_ms)
                .flatten(),
            "progress": { "attempt": 0 },
            "transport": data,
        }),
    )?))
}

pub(crate) fn canonical_action_events_own_operation(
    operation_key: &str,
    ledger: &[homeboy_control_plane_contract::ControlPlaneEvent],
) -> bool {
    let digest = homeboy_engine_primitives::content_hash::sha256_hex(operation_key.as_bytes());
    ledger.iter().any(|event| {
        event.kind.starts_with("action.")
            && event.data.get("operation_digest").and_then(Value::as_str) == Some(digest.as_str())
    })
}

fn action_claim_request(
    operation_key: &str,
    kind: &str,
    occurred_at: Option<String>,
    data: Value,
) -> Result<ControlPlaneEventAppendRequest> {
    Ok(ControlPlaneEventAppendRequest {
        schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
        idempotency_key: crate::orchestration::action_event_idempotency_key(operation_key, kind),
        actor: "controller".to_string(),
        kind: kind.to_string(),
        source: ControlPlaneEventSource {
            component: "control-plane".to_string(),
            instance: None,
        },
        occurred_at,
        task: None,
        attempt: None,
        execution: None,
        data,
        artifacts: Vec::new(),
        evidence: Vec::new(),
    })
}

fn progress_request(
    identity: &str,
    kind: &str,
    source: &str,
    task_id: Option<&str>,
    occurred_at: Option<String>,
    mut data: Value,
) -> Result<ControlPlaneEventAppendRequest> {
    let task = match task_id {
        Some(task_id) => {
            if let Some(object) = data.as_object_mut() {
                object.insert("task_id".to_string(), json!(task_id));
            }
            Some(TaskId::new(task_id).map_err(|error| {
                Error::validation_invalid_argument(
                    "task_id",
                    error.to_string(),
                    Some(task_id.to_string()),
                    None,
                )
            })?)
        }
        None => None,
    };
    Ok(ControlPlaneEventAppendRequest {
        schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
        idempotency_key: crate::orchestration::progress_event_idempotency_key(kind, identity),
        actor: "controller".to_string(),
        kind: kind.to_string(),
        source: ControlPlaneEventSource {
            component: source.to_string(),
            instance: None,
        },
        occurred_at,
        task,
        attempt: None,
        execution: None,
        data,
        artifacts: Vec::new(),
        evidence: Vec::new(),
    })
}

fn aggregate_event_identity(
    run_id: &str,
    events: &[AgentTaskProgressEvent],
    index: usize,
) -> String {
    let event = &events[index];
    let content = json!([event.task_id, event.state, event.attempt, event.message]).to_string();
    let digest = homeboy_engine_primitives::content_hash::sha256_hex(content.as_bytes());
    let ordinal = events[..index]
        .iter()
        .filter(|other| {
            other.task_id == event.task_id
                && other.state == event.state
                && other.attempt == event.attempt
                && other.message == event.message
        })
        .count()
        + 1;
    format!("aggregate\0{run_id}\0{digest}\0{ordinal}")
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn map_append_error(error: ControlPlaneError) -> Error {
    match error.class {
        ControlPlaneErrorClass::InvalidArgument | ControlPlaneErrorClass::NotFound => {
            Error::validation_invalid_argument("control_plane_event", error.message, None, None)
        }
        _ => Error::internal_unexpected(error.message),
    }
}
