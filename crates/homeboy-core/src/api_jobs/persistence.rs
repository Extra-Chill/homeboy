use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use homeboy_lab_contract::lab::execution_envelope::lab_runner_workload_from_execution_envelope;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::store::{DurableJobStore, StoredJob};
use super::types::{Job, JobEvent, JobEventKind, JobStatus};
use crate::error::{Error, Result};

pub(super) const DEFAULT_EVENT_RETENTION_LIMIT: usize = 1000;
/// Keep enough completed jobs for status and log reconciliation without letting
/// the daemon's append-only store grow with every historical execution.
pub(super) const DEFAULT_TERMINAL_JOB_RETENTION_LIMIT: usize = 1000;
/// Bound terminal history independently of active jobs, whose recovery records
/// must remain durable until they reach a terminal state. Every durable
/// transaction (including each appended job event) serializes and fsyncs the
/// whole store, so retained history directly multiplies per-event cost.
/// Compacted jobs keep exactly-once replay evidence in the tombstone index.
pub(super) const DEFAULT_TERMINAL_JOB_RETENTION_BYTES: usize = 1024 * 1024;

/// A compacted key is permanent exactly-once evidence. It intentionally carries
/// no request or event payload. Controller tombstones retain only their existing
/// safe terminal job projection so lost-response retries stay observable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ReplayTombstone {
    pub(super) kind: ReplayTombstoneKind,
    pub(super) key: String,
    pub(super) fingerprint: String,
    pub(super) job_id: Uuid,
    /// Controller callers need the safe terminal status projection to make a
    /// lost-response retry observable. Remote runner tombstones omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) terminal_job: Option<Job>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReplayTombstoneKind {
    RemoteRunner,
    Controller,
}

fn tombstone_kind_name(kind: ReplayTombstoneKind) -> &'static str {
    match kind {
        ReplayTombstoneKind::RemoteRunner => "remote_runner",
        ReplayTombstoneKind::Controller => "controller",
    }
}

pub(super) fn tombstone_error(path: &Path, operation: &str, error: rusqlite::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("{operation} {}", path.display())),
    )
}

/// Open the observation store at `path` for a ledger write. The observation
/// store's own open creates the schema and WAL and installs its busy wait;
/// ledger commits are fsynced, so this connection runs `synchronous=FULL`
/// (a per-connection setting).
pub(crate) fn open_ledger(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let connection = crate::observation::store::open_initialized_connection(path)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| tombstone_error(path, "configure daemon job ledger", error))?;
    Ok(connection)
}

/// Open the observation store at `path` read-only, without creating or
/// initializing anything. `None` means there is no ledger yet: no jobs.
pub(super) fn open_ledger_readonly(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = crate::observation::store::open_readonly_connection(path)?;
    // The baseline schema is created in one transaction, so one ledger table
    // stands for all of them.
    let initialized = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'daemon_store_state'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| tombstone_error(path, "inspect daemon job ledger", error))?
        .is_some();
    Ok(initialized.then_some(connection))
}

pub(super) fn insert_tombstone(
    connection: &Connection,
    path: &Path,
    tombstone: &ReplayTombstone,
) -> Result<()> {
    let terminal_job = tombstone
        .terminal_job
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize replay tombstone".to_string()),
            )
        })?;
    let changed = connection
        .execute(
            "INSERT OR IGNORE INTO replay_tombstones (kind, key, fingerprint, job_id, terminal_job)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                tombstone_kind_name(tombstone.kind),
                tombstone.key,
                tombstone.fingerprint,
                tombstone.job_id.to_string(),
                terminal_job,
            ],
        )
        .map_err(|error| tombstone_error(path, "insert replay tombstone", error))?;
    if changed == 0 {
        let existing =
            lookup_tombstone_in_connection(connection, path, tombstone.kind, &tombstone.key)?
                .expect("existing replay tombstone must be readable");
        let expected = serde_json::to_value(tombstone).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize replay tombstone".to_string()),
            )
        })?;
        let actual = serde_json::to_value(existing).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize replay tombstone".to_string()),
            )
        })?;
        if actual != expected {
            return Err(Error::internal_unexpected(
                "replay tombstone index contains conflicting exactly-once evidence",
            ));
        }
    }
    Ok(())
}

fn lookup_tombstone_in_connection(
    connection: &Connection,
    path: &Path,
    kind: ReplayTombstoneKind,
    key: &str,
) -> Result<Option<ReplayTombstone>> {
    let row = connection
        .query_row(
            "SELECT fingerprint, job_id, terminal_job FROM replay_tombstones WHERE kind = ?1 AND key = ?2",
            params![tombstone_kind_name(kind), key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
        )
        .optional()
        .map_err(|error| tombstone_error(path, "lookup replay tombstone", error))?;
    row.map(|(fingerprint, job_id, terminal_job)| {
        Ok(ReplayTombstone {
            kind,
            key: key.to_string(),
            fingerprint,
            job_id: Uuid::parse_str(&job_id).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?,
            terminal_job: terminal_job
                .map(|value| {
                    serde_json::from_str(&value).map_err(|error| {
                        Error::config_invalid_json(path.display().to_string(), error)
                    })
                })
                .transpose()?,
        })
    })
    .transpose()
}

/// SQLite's primary key provides bounded indexed lookup and an atomic durable
/// commit. A damaged index is a fail-closed store corruption, never absence.
pub(super) fn lookup_tombstone(
    path: &Path,
    kind: ReplayTombstoneKind,
    key: &str,
) -> Result<Option<ReplayTombstone>> {
    let connection = open_ledger(path)?;
    lookup_tombstone_in_connection(&connection, path, kind, key)
}

/// Read permanent terminal custody by exact identity, without replaying an
/// admission or interpreting an absent row as proof of workload absence.
pub(super) fn lookup_terminal_controller_job(path: &Path, job_id: Uuid) -> Result<Option<Job>> {
    let Some(connection) = open_ledger_readonly(path)? else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT terminal_job FROM replay_tombstones WHERE kind = 'controller' AND job_id = ?1 AND terminal_job IS NOT NULL",
    ).map_err(|error| tombstone_error(path, "lookup terminal controller identity", error))?;
    let rows = statement
        .query_map([job_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(|error| tombstone_error(path, "read terminal controller identity", error))?;
    let mut winner: Option<Job> = None;
    for raw in rows {
        let raw =
            raw.map_err(|error| tombstone_error(path, "read terminal controller custody", error))?;
        let job: Job = serde_json::from_str(&raw)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        validate_terminal_controller_identity(&job, job_id)?;
        if winner.as_ref().is_some_and(|prior| {
            serde_json::to_value(prior).ok() != serde_json::to_value(&job).ok()
        }) {
            return Err(Error::internal_unexpected(
                "conflicting terminal controller custody",
            ));
        }
        winner = Some(job);
    }
    Ok(winner)
}

pub(super) fn validate_terminal_controller_identity(job: &Job, job_id: Uuid) -> Result<()> {
    if job.id != job_id || !job.status.is_terminal() || !job.operation.starts_with("controller.") {
        return Err(Error::internal_unexpected(
            "terminal controller custody has mismatched identity or nonterminal status",
        ));
    }
    Ok(())
}

/// The replay tombstone count and the bytes their rows hold.
pub(super) fn tombstone_store_report(path: &Path) -> Result<(usize, u64)> {
    let Some(connection) = open_ledger_readonly(path)? else {
        return Ok((0, 0));
    };
    connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(kind) + length(key) + length(fingerprint)
                 + length(job_id) + COALESCE(length(terminal_job), 0)), 0)
             FROM replay_tombstones",
            [],
            |row| Ok((row.get::<_, i64>(0)? as usize, row.get::<_, i64>(1)? as u64)),
        )
        .map_err(|error| tombstone_error(path, "count replay tombstones", error))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct JobStoreCompactionEvidence {
    pub(super) timestamp_ms: u64,
    pub(super) removed_terminal_jobs: usize,
    pub(super) retained_terminal_jobs: usize,
    #[serde(default)]
    pub(super) retained_terminal_bytes: usize,
    pub(super) active_jobs: usize,
}

pub(super) fn compact_terminal_jobs(
    durable: &mut DurableJobStore,
    event_retention_limit: usize,
    terminal_job_retention_limit: usize,
    terminal_job_retention_bytes: usize,
) -> Option<JobStoreCompactionEvidence> {
    for stored in &mut durable.jobs {
        apply_event_retention(&mut stored.events, event_retention_limit);
        stored.job.event_count = stored.events.len();
    }
    let mut terminal = durable
        .jobs
        .iter()
        .filter(|stored| stored.job.status.is_terminal())
        .map(|stored| {
            let serialized_len = serde_json::to_vec(stored)
                .expect("stored daemon job must serialize")
                .len();
            (stored, serialized_len)
        })
        .collect::<Vec<_>>();
    let original_terminal_count = terminal.len();
    let original_terminal_bytes = terminal
        .iter()
        .map(|(_, serialized_len)| serialized_len)
        .sum::<usize>();
    if original_terminal_count <= terminal_job_retention_limit
        && original_terminal_bytes <= terminal_job_retention_bytes
    {
        return None;
    }

    terminal.sort_unstable_by_key(|(stored, _)| {
        (
            stored
                .job
                .finished_at_ms
                .unwrap_or(stored.job.updated_at_ms),
            stored.job.updated_at_ms,
            stored.job.created_at_ms,
            stored.job.id,
        )
    });
    let mut retained_terminal_jobs = original_terminal_count;
    let mut retained_terminal_bytes = original_terminal_bytes;
    let mut removed_job_ids = HashSet::new();
    for (stored, serialized_len) in terminal {
        if retained_terminal_jobs <= 1
            || (retained_terminal_jobs <= terminal_job_retention_limit
                && retained_terminal_bytes <= terminal_job_retention_bytes)
        {
            break;
        }
        removed_job_ids.insert(stored.job.id);
        retained_terminal_jobs -= 1;
        retained_terminal_bytes -= serialized_len;
    }
    let removed_terminal_jobs = removed_job_ids.len();
    let removed_controller_jobs = durable
        .jobs
        .iter()
        .filter(|stored| {
            removed_job_ids.contains(&stored.job.id) && stored.controller_job.is_some()
        })
        .map(|stored| (stored.job.id, stored.job.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    durable
        .jobs
        .retain(|stored| !removed_job_ids.contains(&stored.job.id));
    expire_submission_keys(
        &mut durable.submission_keys,
        &mut durable.expired_submission_keys,
        &mut durable.controller_submissions,
        &mut durable.expired_controller_submissions,
        &removed_job_ids,
        &removed_controller_jobs,
    );
    let evidence = JobStoreCompactionEvidence {
        timestamp_ms: timestamp_ms(),
        removed_terminal_jobs,
        retained_terminal_jobs,
        retained_terminal_bytes,
        active_jobs: durable.jobs.len() - retained_terminal_jobs,
    };
    durable.compaction = Some(evidence.clone());
    eprintln!(
        "Homeboy compacted daemon job store: removed {} terminal jobs; retained {} terminal jobs ({} bytes) and {} active jobs",
        evidence.removed_terminal_jobs,
        evidence.retained_terminal_jobs,
        evidence.retained_terminal_bytes,
        evidence.active_jobs,
    );
    Some(evidence)
}

/// Preserve accepted submission fingerprints when terminal job evidence is
/// explicitly pruned or automatically compacted. Replaying an expired key must
/// never dispatch a second execution after its original job was removed.
pub(super) fn expire_submission_keys(
    submission_keys: &mut HashMap<String, super::store::RemoteRunnerSubmission>,
    expired_submission_keys: &mut HashMap<String, super::store::RemoteRunnerSubmission>,
    controller_submissions: &mut HashMap<String, super::store::ControllerJobSubmission>,
    expired_controller_submissions: &mut HashMap<String, super::store::ControllerJobSubmission>,
    removed_job_ids: &HashSet<Uuid>,
    removed_controller_jobs: &HashMap<Uuid, Job>,
) {
    for (key, submission) in std::mem::take(submission_keys) {
        if removed_job_ids.contains(&submission.job_id) {
            expired_submission_keys.insert(key, submission);
        } else {
            submission_keys.insert(key, submission);
        }
    }
    for (key, submission) in std::mem::take(controller_submissions) {
        if removed_job_ids.contains(&submission.job_id) {
            let terminal_job = removed_controller_jobs.get(&submission.job_id).cloned();
            expired_controller_submissions.insert(
                key,
                super::store::ControllerJobSubmission {
                    terminal_job,
                    ..submission
                },
            );
        } else {
            controller_submissions.insert(key, submission);
        }
    }
}
#[cfg(test)]
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
            "control plane lost before the remote runner claimed secret execution env".to_string()
        } else {
            "control plane lost before the job reached a terminal status".to_string()
        };
        let classification = stale_after_restart_classification(stored);
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
            data: Some(serde_json::json!({
                "reason": "orphaned_after_control_plane_loss",
                "classification": classification,
            })),
        });
        next_sequence += 1;
        stored.events.push(JobEvent {
            sequence: next_sequence,
            job_id: stored.job.id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some("job marked failed after control-plane loss".to_string()),
            data: Some(serde_json::json!({
                "status": JobStatus::Failed,
                "reason": "orphaned_after_control_plane_loss",
                "classification": classification,
            })),
        });
        apply_event_retention(&mut stored.events, event_retention_limit);
        stored.job.event_count = stored.events.len();
    }

    next_sequence
}

pub(super) fn stale_after_restart_classification(stored: &StoredJob) -> Value {
    let last_child_event = stored
        .events
        .iter()
        .rev()
        .find(|event| child_evidence_kind(event.kind));
    let artifact_ids = stored
        .job
        .artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    let remote_envelope = stored
        .remote_runner
        .as_ref()
        .map(|remote_runner| &remote_runner.envelope);
    let linked_agent_task_run_id = remote_envelope
        .and_then(|envelope| {
            lab_runner_workload_from_execution_envelope(envelope)
                .ok()
                .flatten()
        })
        .and_then(|workload| workload.agent_task)
        .map(|agent_task| agent_task.run_id.trim().to_string())
        .filter(|run_id| !run_id.is_empty());

    serde_json::json!({
        "kind": "orphaned_after_control_plane_loss",
        "recoverable": false,
        "reason": "orphaned_after_control_plane_loss",
        "terminal_status": "failed",
        "control_plane": {
            "lost": true,
        },
        "retry": {
            "recommended": true,
            "guidance": "Retry this operation after reconnecting to a live daemon; preserved child output and artifacts describe the interrupted attempt.",
        },
        "child": {
            "terminal_result_recorded": false,
            "last_known_event": last_child_event.map(last_known_child_event),
            "output_observed": last_child_event.is_some(),
            "linked_durable_run": linked_agent_task_run_id.as_deref().map(|run_id| serde_json::json!({
                "kind": "agent_task",
                "run_id": run_id,
                "terminal_result_observed": false,
            })),
        },
        "remote_runner": stored.remote_runner.as_ref().map(|_| serde_json::json!({
            "runner_id": remote_envelope.as_ref().and_then(|envelope| envelope.dispatch.as_ref()).map(|dispatch| dispatch.runner_id.clone()),
            "project_id": remote_envelope.as_ref().and_then(|envelope| envelope.dispatch.as_ref()).and_then(|dispatch| dispatch.project_id.clone()),
            "claim_id": stored.job.claim_id.clone(),
            "claimed_by_runner_id": stored.job.claimed_by_runner_id.clone(),
            "claimed_at_ms": stored.job.claimed_at_ms,
            "claim_expires_at_ms": stored.job.claim_expires_at_ms,
            "secret_execution_env_unrecoverable": remote_runner_job_has_unrecoverable_execution_env(stored),
        })),
        "artifacts": {
            "count": artifact_ids.len(),
            "ids": artifact_ids,
        },
    })
}

fn child_evidence_kind(kind: JobEventKind) -> bool {
    matches!(
        kind,
        JobEventKind::Stdout | JobEventKind::Stderr | JobEventKind::Progress | JobEventKind::Result
    )
}

fn last_known_child_event(event: &JobEvent) -> Value {
    serde_json::json!({
        "sequence": event.sequence,
        "kind": event.kind,
        "timestamp_ms": event.timestamp_ms,
        "message": event.message.clone(),
        "data": event.data.clone(),
    })
}

fn remote_runner_job_has_unrecoverable_execution_env(stored: &StoredJob) -> bool {
    // Remote runner jobs persist named references rather than execution values.
    // A restart keeps queued work replayable; dispatch hydrates each name on the
    // runner and fails closed there if the reference can no longer resolve.
    let _ = stored;
    false
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
            | (JobStatus::Queued, JobStatus::Failed)
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

pub(crate) fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_millis() as u64
}
