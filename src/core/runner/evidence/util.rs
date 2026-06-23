use crate::core::api_jobs::JobStatus;
use crate::core::error::{Error, Result};
use serde_json::Value;

pub(super) fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        Error::internal_json(
            format!("remote run detail missing {field}"),
            Some("mirror runner evidence".to_string()),
        )
    })
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
