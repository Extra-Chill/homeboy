use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::remote_runner::RemoteRunnerJobRequest;
use super::store::{DurableJobStore, StoredJob};
use super::types::{Job, JobEvent, JobEventKind, JobStatus};
use crate::error::{Error, Result};

pub(super) const DEFAULT_EVENT_RETENTION_LIMIT: usize = 1000;
/// Keep enough completed jobs for status and log reconciliation without letting
/// the daemon's append-only store grow with every historical execution.
pub(super) const DEFAULT_TERMINAL_JOB_RETENTION_LIMIT: usize = 1000;
/// Bound terminal history independently of active jobs, whose recovery records
/// must remain durable until they reach a terminal state.
pub(super) const DEFAULT_TERMINAL_JOB_RETENTION_BYTES: usize = 4 * 1024 * 1024;

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

pub(super) fn tombstone_path(path: &Path) -> std::path::PathBuf {
    path.with_file_name(format!(
        "{}.replay-tombstones.sqlite",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("jobs.json")
    ))
}

fn legacy_tombstone_path(path: &Path) -> std::path::PathBuf {
    path.with_file_name(format!(
        "{}.replay-tombstones.jsonl",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("jobs.json")
    ))
}

fn tombstone_kind_name(kind: ReplayTombstoneKind) -> &'static str {
    match kind {
        ReplayTombstoneKind::RemoteRunner => "remote_runner",
        ReplayTombstoneKind::Controller => "controller",
    }
}

fn tombstone_error(path: &Path, operation: &str, error: rusqlite::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("{operation} {}", path.display())),
    )
}

fn open_tombstone_store(path: &Path) -> Result<Connection> {
    let journal = tombstone_path(path);
    if let Some(parent) = journal.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let connection = Connection::open(&journal)
        .map_err(|error| tombstone_error(&journal, "open replay tombstone index", error))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| tombstone_error(&journal, "configure replay tombstone index", error))?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS replay_tombstones (
                 kind TEXT NOT NULL,
                 key TEXT NOT NULL,
                 fingerprint TEXT NOT NULL,
                 job_id TEXT NOT NULL,
                 terminal_job TEXT,
                 PRIMARY KEY (kind, key)
             );",
        )
        .map_err(|error| tombstone_error(&journal, "initialize replay tombstone index", error))?;
    Ok(connection)
}

fn insert_tombstone(
    connection: &Connection,
    path: &Path,
    tombstone: &ReplayTombstone,
) -> Result<()> {
    let journal = tombstone_path(path);
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
        .map_err(|error| tombstone_error(&journal, "insert replay tombstone", error))?;
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

/// Migrate the short-lived JSONL format introduced by this branch before any
/// lookup. Renaming only after the SQLite transaction commits makes a crash
/// repeat migration safely instead of ever treating an accepted key as absent.
pub(super) fn prepare_tombstone_store(path: &Path) -> Result<()> {
    let legacy = legacy_tombstone_path(path);
    if !legacy.exists() {
        return Ok(());
    }
    let connection = open_tombstone_store(path)?;
    let transaction = connection.unchecked_transaction().map_err(|error| {
        tombstone_error(
            &tombstone_path(path),
            "begin replay tombstone migration",
            error,
        )
    })?;
    let file = fs::File::open(&legacy).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read {}", legacy.display())),
        )
    })?;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read {}", legacy.display())),
            )
        })?;
        let tombstone: ReplayTombstone = serde_json::from_str(&line)
            .map_err(|error| Error::config_invalid_json(legacy.display().to_string(), error))?;
        insert_tombstone(&transaction, path, &tombstone)?;
    }
    transaction.commit().map_err(|error| {
        tombstone_error(
            &tombstone_path(path),
            "commit replay tombstone migration",
            error,
        )
    })?;
    fs::rename(&legacy, legacy.with_extension("jsonl.migrated")).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("migrate {}", legacy.display())),
        )
    })
}

fn lookup_tombstone_in_connection(
    connection: &Connection,
    path: &Path,
    kind: ReplayTombstoneKind,
    key: &str,
) -> Result<Option<ReplayTombstone>> {
    let journal = tombstone_path(path);
    let row = connection
        .query_row(
            "SELECT fingerprint, job_id, terminal_job FROM replay_tombstones WHERE kind = ?1 AND key = ?2",
            params![tombstone_kind_name(kind), key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
        )
        .optional()
        .map_err(|error| tombstone_error(&journal, "lookup replay tombstone", error))?;
    row.map(|(fingerprint, job_id, terminal_job)| {
        Ok(ReplayTombstone {
            kind,
            key: key.to_string(),
            fingerprint,
            job_id: Uuid::parse_str(&job_id).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read {}", journal.display())),
                )
            })?,
            terminal_job: terminal_job
                .map(|value| {
                    serde_json::from_str(&value).map_err(|error| {
                        Error::config_invalid_json(journal.display().to_string(), error)
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
    prepare_tombstone_store(path)?;
    let connection = open_tombstone_store(path)?;
    lookup_tombstone_in_connection(&connection, path, kind, key)
}

pub(super) fn tombstone_store_report(path: &Path) -> Result<(usize, u64)> {
    prepare_tombstone_store(path)?;
    let journal = tombstone_path(path);
    if !journal.exists() {
        return Ok((0, 0));
    }
    let connection = open_tombstone_store(path)?;
    let count = connection
        .query_row("SELECT COUNT(*) FROM replay_tombstones", [], |row| {
            row.get(0)
        })
        .map_err(|error| tombstone_error(&journal, "count replay tombstones", error))?;
    let bytes = fs::metadata(&journal)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read {}", journal.display())),
            )
        })?
        .len();
    Ok((count, bytes))
}

/// The indexed tombstone transaction commits before the compacted primary store
/// is atomically replaced. A crash can retain an extra rejection, but can never
/// forget an accepted key and replay it as new work.
pub(super) fn write_durable_store_with_tombstones(
    path: &Path,
    durable: &mut DurableJobStore,
) -> Result<()> {
    let mut tombstones = Vec::new();
    for (key, submission) in std::mem::take(&mut durable.expired_submission_keys) {
        tombstones.push(ReplayTombstone {
            kind: ReplayTombstoneKind::RemoteRunner,
            key,
            fingerprint: submission.fingerprint,
            job_id: submission.job_id,
            terminal_job: None,
        });
    }
    for (key, submission) in std::mem::take(&mut durable.expired_controller_submissions) {
        tombstones.push(ReplayTombstone {
            kind: ReplayTombstoneKind::Controller,
            key,
            fingerprint: submission.fingerprint,
            job_id: submission.job_id,
            terminal_job: submission.terminal_job,
        });
    }
    if !tombstones.is_empty() {
        if let Some(parent) = tombstone_path(path).parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        prepare_tombstone_store(path)?;
        let connection = open_tombstone_store(path)?;
        let transaction = connection.unchecked_transaction().map_err(|error| {
            tombstone_error(
                &tombstone_path(path),
                "begin replay tombstone commit",
                error,
            )
        })?;
        for tombstone in tombstones {
            insert_tombstone(&transaction, path, &tombstone)?;
        }
        transaction.commit().map_err(|error| {
            tombstone_error(
                &tombstone_path(path),
                "commit replay tombstone index",
                error,
            )
        })?;
    }
    write_durable_store(path, durable)
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("jobs.json"),
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::write(&temp_path, body).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write {}", temp_path.display())),
        )
    })?;
    if let Ok(file) = fs::File::open(&temp_path) {
        file.sync_all().map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("sync {}", temp_path.display())))
        })?;
    }
    fs::rename(&temp_path, path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "rename {} to {}",
                temp_path.display(),
                path.display()
            )),
        )
    })?;
    // The file contents are synced above; syncing the directory makes the
    // rename itself durable across a power loss on filesystems that require it.
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })?;
    Ok(())
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
    // Keep an explicit tombstone when compaction expires an idempotency key.
    // Replaying an expired accepted submission must fail closed, never enqueue
    // a second execution after the original evidence has been pruned.
    for (key, submission) in std::mem::take(&mut durable.submission_keys) {
        if removed_job_ids.contains(&submission.job_id) {
            durable.expired_submission_keys.insert(key, submission);
        } else {
            durable.submission_keys.insert(key, submission);
        }
    }
    // Controller keys are permanent once accepted. Their tombstones retain the
    // canonical fingerprint after terminal-job compaction, preventing a retry
    // from executing the same logical work a second time.
    for (key, submission) in std::mem::take(&mut durable.controller_submissions) {
        if removed_job_ids.contains(&submission.job_id) {
            let terminal_job = removed_controller_jobs.get(&submission.job_id).cloned();
            durable.expired_controller_submissions.insert(
                key,
                super::store::ControllerJobSubmission {
                    terminal_job,
                    ..submission
                },
            );
        } else {
            durable.controller_submissions.insert(key, submission);
        }
    }
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
    let linked_agent_task_run_id = stored
        .remote_runner
        .as_ref()
        .and_then(|remote_runner| remote_runner.request.lab_runner_workload.as_ref())
        .and_then(|workload| workload.agent_task.as_ref())
        .map(|agent_task| agent_task.run_id.trim())
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
            "linked_durable_run": linked_agent_task_run_id.map(|run_id| serde_json::json!({
                "kind": "agent_task",
                "run_id": run_id,
                "terminal_result_observed": false,
            })),
        },
        "remote_runner": stored.remote_runner.as_ref().map(|remote_runner| serde_json::json!({
            "runner_id": remote_runner.request.runner_id.clone(),
            "project_id": remote_runner.request.project_id.clone(),
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
