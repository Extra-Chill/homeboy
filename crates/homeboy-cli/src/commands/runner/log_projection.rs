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

/// Maximum number of lifecycle events retained in compact mode after heartbeat
/// coalescing. Bounds the projection so a long-running job cannot return an
/// unbounded event list even if it emits many distinct phase transitions. When
/// exceeded, the oldest events are dropped and a synthetic marker records how
/// many were elided (the full history stays durable and non-compact-requestable).
pub(super) const COMPACT_MAX_EVENTS: usize = 40;

/// Outcome of projecting a job-log snapshot's events.
pub(super) struct JobLogProjection {
    pub events: Vec<JobEvent>,
    pub exit_code: Option<i32>,
    pub orchestration_provenance: Option<Value>,
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
    let orchestration_provenance = result_data
        .as_ref()
        .and_then(|data| data.get("orchestration_provenance"))
        .cloned()
        .or_else(|| {
            result_data
                .as_ref()
                .and_then(|data| data.get("data"))
                .and_then(|data| data.get("orchestration_provenance"))
                .cloned()
        });

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
        // Coalesce repeated heartbeat/resource-sample progress into a single
        // latest sample plus count/time range, then bound the total event count.
        // Status transitions, errors, and non-heartbeat phase transitions are
        // preserved (#9765).
        events = coalesce_compact_events(events);
    } else {
        // De-dup: the structured result event already carries stdout/stderr, so
        // drop the raw duplicate events whenever a result is present.
        if result_data.is_some() {
            events
                .retain(|event| !matches!(event.kind, JobEventKind::Stdout | JobEventKind::Stderr));
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
        orchestration_provenance,
        stdout,
        stderr,
    }
}

/// True when a `Progress` event is a periodic heartbeat/resource sample rather
/// than a meaningful phase transition. Heartbeats carry `data.phase ==
/// "heartbeat"` (see `runner_command_heartbeat_data`); every other progress
/// event — `started`, `finished`, provider/phase transitions — is meaningful.
fn is_heartbeat_progress(event: &JobEvent) -> bool {
    event.kind == JobEventKind::Progress
        && event
            .data
            .as_ref()
            .and_then(|data| data.get("phase"))
            .and_then(Value::as_str)
            == Some("heartbeat")
}

/// Collapse a run of heartbeat progress events into one representative event:
/// the latest sample, annotated with how many heartbeats it summarizes and the
/// elapsed span they covered. Preserves the operator-relevant "current
/// resources" while dropping the near-identical intermediate samples (#9765).
fn coalesce_heartbeats(run: Vec<JobEvent>) -> JobEvent {
    let count = run.len();
    let first_elapsed = run
        .first()
        .and_then(|event| event.data.as_ref())
        .and_then(|data| data.get("elapsed_ms"))
        .and_then(Value::as_u64);
    let mut latest = run
        .into_iter()
        .next_back()
        .expect("coalesce_heartbeats requires a non-empty run");
    let last_elapsed = latest
        .data
        .as_ref()
        .and_then(|data| data.get("elapsed_ms"))
        .and_then(Value::as_u64);
    if let Some(data) = latest.data.as_mut().and_then(Value::as_object_mut) {
        data.insert(
            "coalesced_heartbeats".to_string(),
            serde_json::json!({
                "count": count,
                "first_elapsed_ms": first_elapsed,
                "last_elapsed_ms": last_elapsed,
            }),
        );
    }
    latest
}

/// Coalesce consecutive heartbeat progress runs and bound the total compact
/// event count. Non-heartbeat events pass through in order; each maximal run of
/// heartbeats collapses to one summarized latest sample. If the result still
/// exceeds [`COMPACT_MAX_EVENTS`], the oldest events are dropped and a synthetic
/// `Status` marker records how many were elided so the projection stays bounded.
fn coalesce_compact_events(events: Vec<JobEvent>) -> Vec<JobEvent> {
    let mut coalesced: Vec<JobEvent> = Vec::with_capacity(events.len());
    let mut heartbeat_run: Vec<JobEvent> = Vec::new();
    for event in events {
        if is_heartbeat_progress(&event) {
            heartbeat_run.push(event);
            continue;
        }
        if !heartbeat_run.is_empty() {
            coalesced.push(coalesce_heartbeats(std::mem::take(&mut heartbeat_run)));
        }
        coalesced.push(event);
    }
    if !heartbeat_run.is_empty() {
        coalesced.push(coalesce_heartbeats(heartbeat_run));
    }

    if coalesced.len() <= COMPACT_MAX_EVENTS {
        return coalesced;
    }

    // Keep the most recent COMPACT_MAX_EVENTS - 1 events and prepend a marker
    // noting how many older lifecycle events were elided.
    let elided = coalesced.len() - (COMPACT_MAX_EVENTS - 1);
    let tail_start = coalesced.len() - (COMPACT_MAX_EVENTS - 1);
    let first_sequence = coalesced.first().map(|event| event.sequence).unwrap_or(0);
    let mut bounded = Vec::with_capacity(COMPACT_MAX_EVENTS);
    bounded.push(JobEvent {
        sequence: first_sequence,
        job_id: coalesced[0].job_id,
        kind: JobEventKind::Status,
        timestamp_ms: coalesced[0].timestamp_ms,
        message: Some(format!(
            "compact projection elided {elided} older lifecycle event(s); request full events for complete history"
        )),
        data: Some(serde_json::json!({ "phase": "compact_elided", "elided_events": elided })),
    });
    bounded.extend(coalesced.into_iter().skip(tail_start));
    bounded
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

    fn event(
        sequence: u64,
        kind: JobEventKind,
        message: Option<&str>,
        data: Option<Value>,
    ) -> JobEvent {
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
            event(
                2,
                JobEventKind::Progress,
                None,
                Some(json!({"phase": "started"})),
            ),
            event(3, JobEventKind::Stdout, Some(blob), None),
            event(
                4,
                JobEventKind::Progress,
                None,
                Some(json!({"phase": "finished", "exit_code": 1})),
            ),
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
            result.data.as_ref().unwrap()["stdout"]
                .as_str()
                .unwrap()
                .len(),
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

    #[test]
    fn projection_lifts_orchestration_provenance_from_result_data() {
        let mut events = snapshot("ok");
        let result = events
            .iter_mut()
            .find(|event| event.kind == JobEventKind::Result)
            .expect("result event");
        result.data = Some(json!({
            "exit_code": 0,
            "data": {
                "orchestration_provenance": {
                    "schema": "homeboy/orchestration-target-provenance/v1",
                    "selected_runner_id": "lab"
                }
            }
        }));

        let projection = project_job_log(events, true, None);

        assert_eq!(
            projection.orchestration_provenance.unwrap()["selected_runner_id"],
            "lab"
        );
    }

    fn heartbeat_event(sequence: u64, elapsed_ms: u64, rss: u64) -> JobEvent {
        event(
            sequence,
            JobEventKind::Progress,
            None,
            Some(json!({
                "phase": "heartbeat",
                "elapsed_ms": elapsed_ms,
                "process": { "root_pid": 42, "resources": { "rss_bytes": rss } }
            })),
        )
    }

    #[test]
    fn compact_coalesces_heartbeat_progress_into_latest_sample() {
        // #9765: a long job emits many 30s heartbeat progress events. Compact
        // mode must collapse them to one latest sample with a count/time range,
        // while keeping status transitions and the terminal result's exit code.
        let mut events = vec![
            event(1, JobEventKind::Status, Some("running"), None),
            event(
                2,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "started" })),
            ),
        ];
        for i in 0..50u64 {
            events.push(heartbeat_event(10 + i, i * 30_000, 100 + i));
        }
        events.push(event(
            80,
            JobEventKind::Result,
            None,
            Some(json!({ "exit_code": 0, "stdout": "done", "stderr": "" })),
        ));

        let projection = project_job_log(events, true, None);

        // All 50 heartbeats collapse to exactly one progress sample.
        let heartbeats: Vec<&JobEvent> = projection
            .events
            .iter()
            .filter(|event| is_heartbeat_progress(event))
            .collect();
        assert_eq!(
            heartbeats.len(),
            1,
            "heartbeats must coalesce to one sample"
        );

        let summary = &heartbeats[0].data.as_ref().unwrap()["coalesced_heartbeats"];
        assert_eq!(summary["count"], 50);
        assert_eq!(summary["first_elapsed_ms"], 0);
        assert_eq!(summary["last_elapsed_ms"], 49 * 30_000);
        // The latest sample's resources are preserved (rss of the final beat).
        assert_eq!(
            heartbeats[0].data.as_ref().unwrap()["process"]["resources"]["rss_bytes"],
            149
        );

        // Meaningful transitions survive.
        assert!(projection
            .events
            .iter()
            .any(|event| event.kind == JobEventKind::Status));
        assert!(projection.events.iter().any(|event| event
            .data
            .as_ref()
            .and_then(|data| data.get("phase"))
            .and_then(Value::as_str)
            == Some("started")));
        assert_eq!(projection.exit_code, Some(0));
    }

    #[test]
    fn compact_preserves_distinct_phase_transitions_between_heartbeats() {
        // Non-heartbeat progress (phase transitions) must not be coalesced, even
        // when interleaved with heartbeats.
        let events = vec![
            event(1, JobEventKind::Status, Some("running"), None),
            heartbeat_event(2, 0, 100),
            heartbeat_event(3, 30_000, 101),
            event(
                4,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": "gate:test" })),
            ),
            heartbeat_event(5, 60_000, 102),
            heartbeat_event(6, 90_000, 103),
            event(7, JobEventKind::Error, Some("gate failed"), None),
        ];

        let projection = project_job_log(events, true, None);

        // Two separate heartbeat runs → two coalesced samples.
        assert_eq!(
            projection
                .events
                .iter()
                .filter(|event| is_heartbeat_progress(event))
                .count(),
            2
        );
        // The phase transition and error survive.
        assert!(projection.events.iter().any(|event| event
            .data
            .as_ref()
            .and_then(|data| data.get("phase"))
            .and_then(Value::as_str)
            == Some("gate:test")));
        assert!(projection
            .events
            .iter()
            .any(|event| event.kind == JobEventKind::Error));
    }

    #[test]
    fn compact_bounds_event_count_with_elided_marker() {
        // Many distinct phase transitions (not coalescible) must still be bounded.
        let mut events = vec![event(0, JobEventKind::Status, Some("running"), None)];
        for i in 0..100u64 {
            events.push(event(
                i + 1,
                JobEventKind::Progress,
                None,
                Some(json!({ "phase": format!("step-{i}") })),
            ));
        }

        let projection = project_job_log(events, true, None);

        assert!(
            projection.events.len() <= COMPACT_MAX_EVENTS,
            "compact events must be bounded, got {}",
            projection.events.len()
        );
        let marker = projection
            .events
            .first()
            .expect("bounded projection has a leading marker");
        assert_eq!(
            marker.data.as_ref().unwrap()["phase"],
            "compact_elided",
            "an elision marker records dropped events"
        );
        assert!(
            marker.data.as_ref().unwrap()["elided_events"]
                .as_u64()
                .unwrap()
                > 0
        );
    }
}
