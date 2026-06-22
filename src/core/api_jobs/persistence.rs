use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use uuid::Uuid;

use super::remote_runner::RemoteRunnerJobRequest;
use super::store::{DurableJobStore, StoredJob};
use super::types::{JobEvent, JobEventKind, JobStatus};
use crate::core::error::{Error, Result};

pub(super) const DEFAULT_EVENT_RETENTION_LIMIT: usize = 1000;

pub(super) fn request_metadata_string(
    request: &RemoteRunnerJobRequest,
    key: &str,
) -> Option<String> {
    request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn read_durable_store(path: &Path) -> Result<DurableJobStore> {
    if !path.exists() {
        return Ok(DurableJobStore::default());
    }

    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", path.display()))))?;
    match serde_json::from_str(&content) {
        Ok(store) => Ok(store),
        Err(err) => {
            let quarantine_path = path.with_file_name(format!(
                "{}.corrupt-{}",
                path.file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("jobs.json"),
                timestamp_ms()
            ));
            fs::rename(path, &quarantine_path).map_err(|rename_err| {
                Error::config_invalid_json(path.display().to_string(), err).with_hint(format!(
                    "Homeboy could not quarantine the corrupt durable job store to {}: {}",
                    quarantine_path.display(),
                    rename_err
                ))
            })?;
            eprintln!(
                "Homeboy quarantined corrupt daemon job store {} to {} and started with an empty queue",
                path.display(),
                quarantine_path.display()
            );
            Ok(DurableJobStore::default())
        }
    }
}

pub(super) fn write_durable_store(path: &Path, durable: &DurableJobStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let body = serde_json::to_string_pretty(durable).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("serialize daemon job store".to_string()),
        )
    })?;
    fs::write(path, body)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("write {}", path.display()))))
}

pub(super) fn reconcile_stale_jobs(
    durable: &mut DurableJobStore,
    event_retention_limit: usize,
) -> u64 {
    let now = timestamp_ms();
    let mut next_sequence = durable
        .jobs
        .iter()
        .flat_map(|stored| stored.events.iter().map(|event| event.sequence))
        .max()
        .unwrap_or(0);

    for stored in &mut durable.jobs {
        if !matches!(stored.job.status, JobStatus::Queued | JobStatus::Running) {
            continue;
        }
        // Remote-runner jobs that are still Queued are waiting for a runner to
        // claim them; a daemon restart does not invalidate that work unless the
        // non-serialized execution request carried secret env values.
        if stored.remote_runner.is_some() && stored.job.status == JobStatus::Queued {
            if !remote_runner_job_has_unrecoverable_execution_env(stored) {
                continue;
            }
        }

        // Recover the real terminal status when the underlying command already
        // recorded a terminal Result event before the daemon restarted. Without
        // this, a job that actually succeeded (or that recorded its own
        // non-zero exit code) is blindly reported as a daemon-restart failure,
        // leaving the caller without the real result for in-flight work (#4770).
        if let Some((recovered_status, exit_code)) = recovered_terminal_from_result(&stored.events)
        {
            stored.job.status = recovered_status;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            stored.job.stale_reason = None;

            next_sequence += 1;
            stored.events.push(JobEvent {
                sequence: next_sequence,
                job_id: stored.job.id,
                kind: JobEventKind::Status,
                timestamp_ms: now,
                message: Some(
                    "job terminal status recovered from recorded result after daemon restart"
                        .to_string(),
                ),
                data: Some(serde_json::json!({
                    "status": recovered_status,
                    "reason": "recovered_after_daemon_restart",
                    "exit_code": exit_code,
                })),
            });
            apply_event_retention(&mut stored.events, event_retention_limit);
            stored.job.event_count = stored.events.len();
            continue;
        }

        let reason = if remote_runner_job_has_unrecoverable_execution_env(stored) {
            "daemon restarted before the remote runner claimed secret execution env".to_string()
        } else {
            "daemon restarted before the job reached a terminal status".to_string()
        };
        stored.job.status = JobStatus::Failed;
        stored.job.updated_at_ms = now;
        stored.job.finished_at_ms = Some(now);
        stored.job.stale_reason = Some(reason.clone());

        next_sequence += 1;
        stored.events.push(JobEvent {
            sequence: next_sequence,
            job_id: stored.job.id,
            kind: JobEventKind::Error,
            timestamp_ms: now,
            message: Some(reason.clone()),
            data: Some(serde_json::json!({ "reason": "stale_after_daemon_restart" })),
        });
        next_sequence += 1;
        stored.events.push(JobEvent {
            sequence: next_sequence,
            job_id: stored.job.id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some("job marked failed after daemon restart".to_string()),
            data: Some(serde_json::json!({
                "status": JobStatus::Failed,
                "reason": "stale_after_daemon_restart"
            })),
        });
        apply_event_retention(&mut stored.events, event_retention_limit);
        stored.job.event_count = stored.events.len();
    }

    next_sequence
}

fn remote_runner_job_has_unrecoverable_execution_env(stored: &StoredJob) -> bool {
    let Some(remote_runner) = stored.remote_runner.as_ref() else {
        return false;
    };
    if remote_runner.execution_request.is_some() {
        return false;
    }
    remote_runner.request.secret_env_names.iter().any(|name| {
        remote_runner
            .request
            .env
            .get(name)
            .is_some_and(|value| value == "<redacted>")
    })
}

/// Recover a terminal job status from a recorded `Result` event when a job was
/// left non-terminal by a daemon restart. The daemon worker records the command
/// result (including its `exit_code`) before transitioning the job to its
/// terminal status; if the restart lands in that window the stored result is the
/// authoritative outcome. Returns the recovered status and the exit code that
/// justified it, or `None` when no terminal result was recorded.
pub(super) fn recovered_terminal_from_result(events: &[JobEvent]) -> Option<(JobStatus, i64)> {
    let result = events
        .iter()
        .rev()
        .find(|event| event.kind == JobEventKind::Result)?;
    let data = result.data.as_ref()?;
    // A recorded cancellation outcome is honored as Cancelled regardless of exit code.
    if data.get("status").and_then(Value::as_str) == Some("cancelled") {
        return Some((
            JobStatus::Cancelled,
            data.get("exit_code").and_then(Value::as_i64).unwrap_or(0),
        ));
    }
    let exit_code = data.get("exit_code").and_then(Value::as_i64)?;
    let status = if exit_code == 0 {
        JobStatus::Succeeded
    } else {
        JobStatus::Failed
    };
    Some((status, exit_code))
}

pub(super) fn apply_event_retention(events: &mut Vec<JobEvent>, limit: usize) {
    if events.len() > limit {
        let excess = events.len() - limit;
        events.drain(0..excess);
    }
}

pub(super) fn validate_transition(current: JobStatus, next: JobStatus) -> Result<()> {
    let allowed = matches!(
        (current, next),
        (JobStatus::Queued, JobStatus::Running)
            | (JobStatus::Queued, JobStatus::Cancelled)
            | (JobStatus::Running, JobStatus::Succeeded)
            | (JobStatus::Running, JobStatus::Failed)
            | (JobStatus::Running, JobStatus::Cancelled)
    );

    if allowed {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "status",
            format!("cannot transition job from {:?} to {:?}", current, next),
            None,
            None,
        ))
    }
}

pub(super) fn job_not_found(job_id: Uuid) -> Error {
    Error::validation_invalid_argument("job_id", "job not found", Some(job_id.to_string()), None)
}

pub(super) fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_millis() as u64
}
