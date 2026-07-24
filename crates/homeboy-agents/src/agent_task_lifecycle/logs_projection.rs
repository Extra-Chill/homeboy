//! Run log and event projection: builds `agent-task logs` / bridge-status event
//! streams from aggregates, runner-job events, and durable local provider
//! executions. Extracted from `lifecycle_ops` to keep that module within the
//! god-file threshold (#9927).

use serde_json::Value;

use super::*;

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    logs_with_raw(run_id, false)
}

pub fn logs_with_raw(run_id: &str, include_raw: bool) -> Result<AgentTaskRunLog> {
    // Status reconciliation fetches the live daemon snapshot for a bound Lab
    // child, making executor progress visible before the child is terminal.
    let record = status(run_id)?;
    let run_id = record.run_id.clone();
    let (events, artifact_refs, raw_events) = match store::read_aggregate(&run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs, Vec::new())
        }
        Err(_) => {
            let raw_events = runner_job_raw_events(&record);
            // Before any aggregate exists, a local (in-process) cook that is
            // actively running the provider otherwise shows only "task submitted".
            // Surface the durable running provider execution so `agent-task logs`
            // distinguishes active provider execution from a hung preflight (#8396).
            let progress = runner_job_progress_events(&record).unwrap_or_else(|| {
                let mut events = queued_events(&record.tasks);
                events.extend(local_provider_execution_events(&record));
                events
            });
            (progress, record.artifact_refs.clone(), raw_events)
        }
    };
    let events = if raw_events.is_empty() {
        normalize_progress_events(&run_id, &events, &artifact_refs)
    } else {
        normalize_runner_job_events(&run_id, &raw_events, &record, &artifact_refs)
    };
    Ok(AgentTaskRunLog {
        schema: schemas::RUN_LOG.to_string(),
        run_id,
        events,
        raw_events: include_raw.then_some(raw_events).unwrap_or_default(),
    })
}

/// Synthesize progress events from the durable running provider executions of a
/// local (in-process) cook. `reserve_provider_execution` records each attempt
/// (backend, model, started_at, `state:"running"`) before the scheduler blocks
/// on the backend, but until an aggregate exists `agent-task logs` shows only
/// "task submitted". Surface the running executions so logs reflect active
/// provider work rather than a stalled preflight (#8396).
pub(super) fn local_provider_execution_events(
    record: &AgentTaskRunRecord,
) -> Vec<AgentTaskProgressEvent> {
    let Some(executions) = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    executions
        .iter()
        .filter(|execution| execution.get("state").and_then(Value::as_str) == Some("running"))
        .map(|execution| {
            let task_id = execution
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| record.tasks.first().map(|task| task.task_id.clone()))
                .unwrap_or_else(|| record.run_id.clone());
            let backend = execution
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("provider");
            let mut message = format!("provider execution running: {backend}");
            if let Some(model) = execution.get("model").and_then(Value::as_str) {
                if !model.is_empty() {
                    message.push_str(&format!(" ({model})"));
                }
            }
            if let Some(started_at) = execution.get("started_at").and_then(Value::as_str) {
                message.push_str(&format!("; started {started_at}"));
            }
            AgentTaskProgressEvent {
                task_id,
                state: AgentTaskState::Running,
                attempt: execution
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as u32,
                message: Some(message),
            }
        })
        .collect()
}

fn runner_job_progress_events(record: &AgentTaskRunRecord) -> Option<Vec<AgentTaskProgressEvent>> {
    let events = record.metadata.get("runner_job_events")?.as_array()?;
    let task_id = record
        .tasks
        .first()
        .map(|task| task.task_id.clone())
        .unwrap_or_else(|| record.run_id.clone());
    Some(
        events
            .iter()
            .map(|event| AgentTaskProgressEvent {
                task_id: task_id.clone(),
                state: AgentTaskState::Running,
                attempt: 0,
                message: event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        event
                            .pointer("/data/message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    }),
            })
            .collect(),
    )
}

fn runner_job_raw_events(record: &AgentTaskRunRecord) -> Vec<Value> {
    record
        .metadata
        .get("runner_job_events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn normalize_runner_job_events(
    run_id: &str,
    raw_events: &[Value],
    record: &AgentTaskRunRecord,
    artifact_refs: &[AgentTaskArtifactRef],
) -> Vec<AgentTaskEventEnvelope> {
    let task_id = record
        .tasks
        .first()
        .map(|task| task.task_id.clone())
        .unwrap_or_else(|| record.run_id.clone());
    let provider = record
        .provider_handles
        .first()
        .map(|handle| handle.backend.clone());

    raw_events
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let data = raw.get("data").cloned().unwrap_or(Value::Null);
            let kind = raw
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("progress");
            let phase =
                string_field(&data, "phase").or_else(|| string_field(&record.metadata, "phase"));
            let activity = string_field(&data, "activity")
                .or_else(|| string_field(&data, "status_note"))
                .or_else(|| string_field(&data, "progress"));
            AgentTaskEventEnvelope {
                schema: schemas::EVENT.to_string(),
                run_id: run_id.to_string(),
                task_id: task_id.clone(),
                // The lifecycle cursor is positional and has always been one-based.
                sequence: (index + 1) as u64,
                event_type: format!("agent_task.runner_{kind}"),
                status: AgentTaskState::Running,
                message: raw
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| string_field(&data, "message")),
                provider: string_field(&data, "provider")
                    .or_else(|| string_field(&data, "backend"))
                    .or_else(|| provider.clone()),
                phase,
                activity,
                heartbeat_at_ms: matches!(kind, "progress" | "status")
                    .then(|| raw.get("timestamp_ms").and_then(Value::as_u64))
                    .flatten(),
                progress: json!({ "attempt": 0 }),
                artifact_refs: artifact_refs
                    .iter()
                    .filter(|reference| reference.task_id == task_id)
                    .cloned()
                    .collect(),
                metadata: data,
            }
        })
        .collect()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
