use super::*;
use homeboy_control_plane_contract::{
    ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEventAppendRequest,
    ControlPlaneEventSource, RunId, TaskId, CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
};
use homeboy_core::observation::PreparedControlPlaneEventAppend;
use serde_json::{json, Value};

pub(crate) fn prepared_progress_events(
    record: &AgentTaskRunRecord,
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
    Ok(events)
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
        _ => return Ok(None),
    };
    let task_id = execution
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| record.tasks.first().map(|task| task.task_id.clone()))
        .unwrap_or_else(|| record.run_id.clone());
    let attempt = execution
        .get("attempt")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
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
    let occurred_at = match state {
        AgentTaskState::Running => execution
            .get("started_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => execution
            .get("finished_at")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                execution
                    .get("started_at")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
    };
    Ok(Some(progress_request(
        record,
        &format!(
            "provider-execution\0{}\0{task_id}\0{attempt}\0{}",
            record.run_id,
            execution["state"].as_str().unwrap_or("unknown")
        ),
        "task.state_changed",
        "agent-task",
        Some(task_id.as_str()),
        occurred_at,
        json!({
            "state": state,
            "message": message,
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
        record,
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

fn progress_request(
    record: &AgentTaskRunRecord,
    identity: &str,
    kind: &str,
    source: &str,
    task_id: Option<&str>,
    occurred_at: Option<String>,
    data: Value,
) -> Result<ControlPlaneEventAppendRequest> {
    let task = match task_id {
        Some(task_id) if record.tasks.iter().any(|task| task.task_id == task_id) => {
            Some(TaskId::new(task_id).map_err(|error| {
                Error::validation_invalid_argument(
                    "task_id",
                    error.to_string(),
                    Some(task_id.to_string()),
                    None,
                )
            })?)
        }
        _ => None,
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
