use serde_json::{json, Value};

use crate::core::api_jobs::JobStatus;
use crate::core::error::{Error, Result};

use super::super::Runner;

pub(super) fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        Error::internal_json(
            format!("remote run detail missing {field}"),
            Some("mirror runner evidence".to_string()),
        )
    })
}

pub(super) fn requested_fuzz_run_id(detail: &Value) -> Option<&str> {
    if detail.get("kind").and_then(Value::as_str) != Some("fuzz") {
        return None;
    }
    detail
        .get("command")
        .and_then(Value::as_str)
        .and_then(fuzz_run_id_from_command)
}

pub(super) fn fuzz_run_id_from_command(command: &str) -> Option<&str> {
    let mut previous_was_run_id = false;
    for token in command.split_whitespace() {
        if previous_was_run_id {
            return (!token.is_empty()).then_some(token);
        }
        if token == "--run-id" {
            previous_was_run_id = true;
            continue;
        }
        if let Some(value) = token.strip_prefix("--run-id=") {
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

pub(super) fn runner_metadata(runner: &Runner) -> Value {
    json!({
        "id": runner.id,
        "kind": runner.kind,
        "server_id": runner.server_id,
        "workspace_root": runner.workspace_root,
        "homeboy_path": runner.settings.homeboy_path,
        "daemon": runner.settings.daemon,
        "artifact_policy": runner.settings.artifact_policy,
    })
}

pub(super) fn result_summary(result: &Value) -> Value {
    json!({
        "command": result.get("command").cloned(),
        "exit_code": result.get("exit_code").cloned(),
        "output_command": result.pointer("/output/command").cloned(),
        "output_status": result.pointer("/output/status").cloned(),
    })
}

pub(super) fn source_snapshot_from_result(value: &Value) -> Option<Value> {
    [
        "/source_snapshot",
        "/source",
        "/metadata/source_snapshot",
        "/metadata/source",
        "/output/source_snapshot",
        "/output/source",
        "/output/metadata/source_snapshot",
        "/output/metadata/source",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).cloned())
}

pub(super) fn push_unique_string(values: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

pub(super) fn job_status_as_run_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued | JobStatus::Running => "running",
        JobStatus::Succeeded => "pass",
        JobStatus::Failed => "fail",
        JobStatus::Cancelled => "skipped",
    }
}

pub(super) fn local_job_run_id(runner_id: &str, job_id: &str) -> String {
    format!("runner-exec-{}-{}", sanitize_id_segment(runner_id), job_id)
}

pub(super) fn sanitize_id_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub(super) fn ms_to_rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(0))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}
