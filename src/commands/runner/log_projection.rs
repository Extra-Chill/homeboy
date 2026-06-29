//! Projection of a runner daemon job-log snapshot into the CLI output shape.
//!
//! The raw snapshot embeds the full command stdout twice: once as a raw
//! `Stdout` event and again inside the structured `Result` event's
//! `data.stdout`. For a bench job that is ~12KB duplicated. This module
//! de-duplicates that (preferring the structured result) and offers a compact
//! projection that returns only lifecycle events, the exit code, and a bounded
//! stdout/stderr tail.

use serde_json::Value;

use homeboy::core::api_jobs::{JobEvent, JobEventKind};

use super::types::RunnerJobLogStream;

/// Default tail size (bytes) applied to stdout/stderr in compact mode when no
/// explicit `--tail` is supplied.
pub(super) const DEFAULT_COMPACT_TAIL_BYTES: usize = 4096;

/// Outcome of projecting a job-log snapshot's events.
pub(super) struct JobLogProjection {
    pub events: Vec<JobEvent>,
    pub exit_code: Option<i32>,
    pub stdout: Option<RunnerJobLogStream>,
    pub stderr: Option<RunnerJobLogStream>,
}

/// Project `events` into the CLI payload.
///
/// - `compact`: keep only lifecycle events (status/progress/error), drop the
///   stdout/stderr/result blobs, and surface exit code + bounded tails.
/// - `tail_bytes`: bound the stdout/stderr tail. When set (or in compact mode)
///   the stdout/stderr payload is lifted out of `events` entirely and returned
///   via the stream fields. When `None` and not compact, stdout stays once in
///   the structured result event (raw duplicate `Stdout` events are still
///   dropped).
pub(super) fn project_job_log(
    mut events: Vec<JobEvent>,
    compact: bool,
    tail_bytes: Option<usize>,
) -> JobLogProjection {
    let result_data = last_result_data(&events);

    let stdout_full = stream_text(&events, &result_data, JobEventKind::Stdout, "stdout");
    let stderr_full = stream_text(&events, &result_data, JobEventKind::Stderr, "stderr");
    let exit_code = result_data
        .as_ref()
        .and_then(|data| data.get("exit_code"))
        .and_then(Value::as_i64)
        .map(|code| code as i32);

    // Bound tails when compacting or when an explicit --tail was given.
    let effective_tail = tail_bytes.or(if compact {
        Some(DEFAULT_COMPACT_TAIL_BYTES)
    } else {
        None
    });
    let lift_streams = compact || tail_bytes.is_some();

    if compact {
        // Lifecycle only: status/progress/error. The blobs are surfaced via the
        // dedicated stream fields below.
        events.retain(|event| {
            matches!(
                event.kind,
                JobEventKind::Status | JobEventKind::Progress | JobEventKind::Error
            )
        });
    } else {
        // De-dup: the structured result event already carries stdout/stderr, so
        // drop the raw duplicate events whenever a result is present.
        if result_data.is_some() {
            events.retain(|event| {
                !matches!(event.kind, JobEventKind::Stdout | JobEventKind::Stderr)
            });
        }
        if lift_streams {
            // Strip the blobs out of the retained result event so stdout/stderr
            // are surfaced exactly once, via the bounded stream fields.
            for event in &mut events {
                if event.kind == JobEventKind::Result {
                    if let Some(data) = event.data.as_mut().and_then(Value::as_object_mut) {
                        data.remove("stdout");
                        data.remove("stderr");
                    }
                }
            }
        }
    }

    let (stdout, stderr) = if lift_streams {
        (
            stdout_full.map(|text| bound_stream(&text, effective_tail)),
            stderr_full.map(|text| bound_stream(&text, effective_tail)),
        )
    } else {
        (None, None)
    };

    JobLogProjection {
        events,
        exit_code: if lift_streams { exit_code } else { None },
        stdout,
        stderr,
    }
}

/// Clone the `data` of the last `Result` event, if any.
fn last_result_data(events: &[JobEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.clone())
}

/// Resolve the full text for a stream, preferring the structured result's
/// field and falling back to concatenating raw event messages (e.g. while a job
/// is still running and has no result yet).
fn stream_text(
    events: &[JobEvent],
    result_data: &Option<Value>,
    kind: JobEventKind,
    field: &str,
) -> Option<String> {
    if let Some(text) = result_data
        .as_ref()
        .and_then(|data| data.get(field))
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    let joined: String = events
        .iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event| event.message.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Build a bounded stream view, keeping the trailing `tail_bytes` on a UTF-8
/// char boundary.
fn bound_stream(full: &str, tail_bytes: Option<usize>) -> RunnerJobLogStream {
    let total = full.len();
    match tail_bytes {
        Some(limit) if total > limit => {
            let mut start = total - limit;
            while start < total && !full.is_char_boundary(start) {
                start += 1;
            }
            let tail = full[start..].to_string();
            RunnerJobLogStream {
                total_bytes: total,
                returned_bytes: tail.len(),
                truncated: true,
                tail,
            }
        }
        _ => RunnerJobLogStream {
            total_bytes: total,
            returned_bytes: total,
            truncated: false,
            tail: full.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn event(sequence: u64, kind: JobEventKind, message: Option<&str>, data: Option<Value>) -> JobEvent {
        JobEvent {
            sequence,
            job_id: Uuid::nil(),
            kind,
            timestamp_ms: sequence,
            message: message.map(str::to_string),
            data,
        }
    }

    /// A typical failed bench job snapshot: a raw stdout event plus a structured
    /// result event that embeds the *same* stdout (the duplication we fix).
    fn snapshot(blob: &str) -> Vec<JobEvent> {
        vec![
            event(1, JobEventKind::Status, Some("queued"), None),
            event(2, JobEventKind::Progress, None, Some(json!({"phase": "started"}))),
            event(3, JobEventKind::Stdout, Some(blob), None),
            event(4, JobEventKind::Progress, None, Some(json!({"phase": "finished", "exit_code": 1}))),
            event(
                5,
                JobEventKind::Result,
                None,
                Some(json!({"exit_code": 1, "stdout": blob, "stderr": "boom"})),
            ),
        ]
    }

    #[test]
    fn default_projection_dedups_raw_stdout_event() {
        let blob = "x".repeat(12_000);
        let projection = project_job_log(snapshot(&blob), false, None);

        // The raw Stdout event is gone; stdout survives once inside the result.
        assert!(projection
            .events
            .iter()
            .all(|event| event.kind != JobEventKind::Stdout));
        let result = projection
            .events
            .iter()
            .find(|event| event.kind == JobEventKind::Result)
            .expect("result event retained");
        assert_eq!(
            result.data.as_ref().unwrap()["stdout"].as_str().unwrap().len(),
            12_000
        );
        // Default mode does not lift streams or exit code to the top level.
        assert!(projection.stdout.is_none());
        assert!(projection.exit_code.is_none());
    }

    #[test]
    fn compact_projection_keeps_lifecycle_and_bounded_tail() {
        let blob = "abcdefghij".repeat(1_000); // 10_000 bytes
        let projection = project_job_log(snapshot(&blob), true, None);

        // Only lifecycle events remain — no stdout/stderr/result blobs.
        assert!(projection.events.iter().all(|event| matches!(
            event.kind,
            JobEventKind::Status | JobEventKind::Progress | JobEventKind::Error
        )));
        assert_eq!(projection.exit_code, Some(1));

        let stdout = projection.stdout.expect("stdout stream surfaced");
        assert_eq!(stdout.total_bytes, 10_000);
        assert!(stdout.truncated);
        assert_eq!(stdout.returned_bytes, DEFAULT_COMPACT_TAIL_BYTES);
        assert!(blob.ends_with(&stdout.tail));

        let stderr = projection.stderr.expect("stderr stream surfaced");
        assert!(!stderr.truncated);
        assert_eq!(stderr.tail, "boom");
    }

    #[test]
    fn tail_without_compact_strips_result_blob_and_keeps_structure() {
        let blob = "y".repeat(5_000);
        let projection = project_job_log(snapshot(&blob), false, Some(1024));

        let result = projection
            .events
            .iter()
            .find(|event| event.kind == JobEventKind::Result)
            .expect("result event retained in tail mode");
        let data = result.data.as_ref().unwrap();
        // stdout/stderr lifted out of the result; structured fields remain.
        assert!(data.get("stdout").is_none());
        assert!(data.get("stderr").is_none());
        assert_eq!(data["exit_code"], 1);

        let stdout = projection.stdout.expect("stdout stream surfaced");
        assert_eq!(stdout.total_bytes, 5_000);
        assert_eq!(stdout.returned_bytes, 1024);
        assert!(stdout.truncated);
    }
}
