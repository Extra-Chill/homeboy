//! Run log and event projection: builds `agent-task logs` / bridge-status event
//! streams from aggregates, runner-job events, and durable local provider
//! executions. Extracted from `lifecycle_ops` to keep that module within the
//! god-file threshold (#9927).

use serde_json::Value;

use super::*;

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    logs_in_store(&lifecycle_store, run_id)
}

/// [`logs`] against explicitly injected durable lifecycle roots.
pub fn logs_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunLog> {
    logs_with_raw_in_store(lifecycle_store, run_id, false)
}

pub(crate) fn logs_with_raw(run_id: &str, include_raw: bool) -> Result<AgentTaskRunLog> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    logs_with_raw_in_store(&lifecycle_store, run_id, include_raw)
}

/// [`logs_with_raw`] against explicitly injected durable lifecycle roots.
///
/// This projection is a genuine read: the Cook-alias resolution, the record
/// read, and the aggregate read are the only durable touches, and every event
/// helper below it works from the record and aggregate already in hand. Both
/// reads have to name the same installation anyway — resolving the record in
/// one home and its aggregate in another would report the event stream of a run
/// that never produced it, and would do so without failing (#7505).
pub fn logs_with_raw_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    include_raw: bool,
) -> Result<AgentTaskRunLog> {
    // Logs are terminal inspection, not runner reconciliation. The durable
    // record remains readable when a runner is unavailable or wedged.
    let record = persisted_status_in_store(lifecycle_store, run_id)?;
    let run_id = record.run_id.clone();
    // `run_id` is already the resolved identity, so this is the store's own
    // exact aggregate read rather than the alias-resolving lifecycle_ops one.
    let (events, artifact_refs, raw_events) = match lifecycle_store.read_aggregate(&run_id) {
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
            let mut artifact_refs = record.artifact_refs.clone();
            artifact_refs.extend(local_provider_execution_artifact_refs(&record));
            (progress, artifact_refs, raw_events)
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
        raw_events: if include_raw {
            raw_events
        } else {
            Default::default()
        },
    })
}

fn local_provider_execution_artifact_refs(
    record: &AgentTaskRunRecord,
) -> Vec<AgentTaskArtifactRef> {
    record.metadata["provider_executions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|execution| {
            let task_id = execution
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| record.tasks.first().map(|task| task.task_id.clone()))
                .unwrap_or_else(|| record.run_id.clone());
            [
                ("provider-runtime-stdout", "stdout"),
                ("provider-runtime-stderr", "stderr"),
            ]
            .into_iter()
            .filter_map(move |(kind, stream)| {
                execution
                    .pointer(&format!("/runtime_evidence/{stream}"))
                    .and_then(Value::as_str)
                    .filter(|uri| !uri.trim().is_empty())
                    .map(|uri| AgentTaskArtifactRef {
                        task_id: task_id.clone(),
                        kind: kind.to_string(),
                        uri: uri.to_string(),
                        role: None,
                        label: Some(format!("provider {stream} (bounded capture)")),
                        semantic_key: None,
                        size_bytes: None,
                    })
            })
        })
        .collect()
}

/// Synthesize progress events from durable local provider executions. `reserve_provider_execution` records each attempt
/// (backend, model, started_at, `state:"running"`) before the scheduler blocks
/// on the backend, but until an aggregate exists `agent-task logs` shows only
/// "task submitted". Terminal reservations are also projected because a
/// cancellation can complete before an aggregate imports the provider outcome.
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
        .filter_map(|execution| {
            let state = match execution.get("state").and_then(Value::as_str) {
                Some("running") => AgentTaskState::Running,
                Some("cancelled") => AgentTaskState::Cancelled,
                Some("timed_out") => AgentTaskState::TimedOut,
                Some("failed") => AgentTaskState::Failed,
                _ => return None,
            };
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
            let mut message = format!(
                "provider execution {}: {backend}",
                execution["state"].as_str().unwrap_or("unknown")
            );
            if let Some(model) = execution.get("model").and_then(Value::as_str) {
                if !model.is_empty() {
                    message.push_str(&format!(" ({model})"));
                }
            }
            if let Some(started_at) = execution.get("started_at").and_then(Value::as_str) {
                message.push_str(&format!("; started {started_at}"));
            }
            Some(AgentTaskProgressEvent {
                task_id,
                state,
                attempt: execution
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as u32,
                message: Some(message),
            })
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
