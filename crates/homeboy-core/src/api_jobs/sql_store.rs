//! SQLite-backed durable job store.
//!
//! The daemon's durable job queue lives in the observation store
//! (`homeboy-vN.sqlite`, opened through [`open_ledger`]) as one row per job,
//! one row per job event and one row per live submission key. A mutation
//! commits one SQLite transaction that writes only the rows whose content
//! changed, so appending an event to one job inserts one event row and leaves
//! every other job row untouched.
//!
//! `JobStoreInner` stays the in-memory view. [`SqlSyncState`] remembers what
//! this process last read or wrote (a content digest per job, the event
//! sequences per job, a digest per submission) so a commit can diff the
//! in-memory view against the database without re-reading it.
//!
//! The observation store is never migrated: a schema bump starts a fresh
//! file, and jobs that were in flight in the previous one are dropped.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use uuid::Uuid;

use super::persistence::{
    insert_tombstone, open_ledger, open_ledger_readonly, tombstone_error,
    JobStoreCompactionEvidence, ReplayTombstone, ReplayTombstoneKind,
};
use super::store::{ControllerJobSubmission, DurableJobStore, RemoteRunnerSubmission, StoredJob};
use super::types::JobEvent;
use crate::error::{Error, Result};

const REMOTE_RUNNER_KIND: &str = "remote_runner";
const CONTROLLER_KIND: &str = "controller";

/// What this process last read from or wrote to the SQL store. When the
/// database still reports `generation`, the in-memory snapshot is already
/// authoritative and a transaction skips the reload.
#[derive(Debug, Default)]
pub(super) struct SqlSyncState {
    pub(super) generation: Option<i64>,
    jobs: HashMap<Uuid, SyncedJob>,
    submissions: HashMap<(&'static str, String), u64>,
    compaction_digest: u64,
}

#[derive(Debug)]
struct SyncedJob {
    body_digest: u64,
    /// Event sequences persisted for this job. Events are append-only apart
    /// from retention, which removes the oldest, so sequences identify them.
    event_sequences: Vec<u64>,
}

fn digest(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn serialize_error(error: serde_json::Error, what: &str) -> Error {
    Error::internal_json(error.to_string(), Some(format!("serialize {what}")))
}

fn store_error(path: &Path, operation: &str, error: rusqlite::Error) -> Error {
    tombstone_error(path, operation, error)
}

/// The row body of a job: the whole `StoredJob` except its events, which are
/// rows of their own.
fn job_row_body(stored: &mut StoredJob) -> Result<String> {
    let events = std::mem::take(&mut stored.events);
    let body = serde_json::to_string(&*stored);
    stored.events = events;
    body.map_err(|error| serialize_error(error, "daemon job row"))
}

fn store_state_exists(connection: &Connection) -> rusqlite::Result<bool> {
    let table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'daemon_store_state'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.is_none() {
        return Ok(false);
    }
    Ok(connection
        .query_row("SELECT 1 FROM daemon_store_state WHERE id = 1", [], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// Open the SQL store read-only without creating or initializing anything.
/// `None` means no job has been stored yet.
pub(super) fn open_sql_store_readonly(path: &Path) -> Result<Option<Connection>> {
    let Some(connection) = open_ledger_readonly(path)? else {
        return Ok(None);
    };
    let initialized = store_state_exists(&connection)
        .map_err(|error| store_error(path, "inspect daemon job store", error))?;
    Ok(initialized.then_some(connection))
}

pub(super) fn sql_store_initialized(path: &Path) -> Result<bool> {
    Ok(open_sql_store_readonly(path)?.is_some())
}

pub(super) fn read_store_generation(connection: &Connection) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT generation FROM daemon_store_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
}

/// Commit counter of the SQL store, `None` before its first open.
pub(super) fn store_generation_at_path(path: &Path) -> Result<Option<i64>> {
    let Some(connection) = open_sql_store_readonly(path)? else {
        return Ok(None);
    };
    read_store_generation(&connection)
        .map_err(|error| store_error(path, "read daemon job store generation", error))
}

/// Open the SQL store for writing, creating its state row the first time.
pub(super) fn open_sql_store(path: &Path) -> Result<Connection> {
    let connection = open_ledger(path)?;
    let initialized = store_state_exists(&connection)
        .map_err(|error| store_error(path, "inspect daemon job store", error))?;
    if !initialized {
        connection
            .execute(
                "INSERT OR IGNORE INTO daemon_store_state (id, generation, compaction)
                 VALUES (1, 0, NULL)",
                [],
            )
            .map_err(|error| store_error(path, "initialize daemon job store", error))?;
    }
    Ok(connection)
}

/// Read the whole store plus the sync state that describes exactly what was
/// read.
pub(super) fn load_sql_store(
    connection: &Connection,
    path: &Path,
) -> Result<(DurableJobStore, SqlSyncState)> {
    let mut state = SqlSyncState::default();
    let mut durable = DurableJobStore::default();

    let (generation, compaction): (i64, Option<String>) = connection
        .query_row(
            "SELECT generation, compaction FROM daemon_store_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| store_error(path, "read daemon job store state", error))?;
    state.generation = Some(generation);
    durable.compaction = compaction
        .map(|raw| serde_json::from_str::<JobStoreCompactionEvidence>(&raw))
        .transpose()
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;

    state.compaction_digest = compaction_digest(&durable.compaction)?;

    let mut events_by_job: HashMap<String, Vec<JobEvent>> = HashMap::new();
    {
        let mut statement = connection
            .prepare("SELECT job_id, body FROM daemon_job_events ORDER BY sequence")
            .map_err(|error| store_error(path, "read daemon job events", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| store_error(path, "read daemon job events", error))?;
        for row in rows {
            let (job_id, body) =
                row.map_err(|error| store_error(path, "read daemon job event", error))?;
            let event: JobEvent = serde_json::from_str(&body)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
            events_by_job.entry(job_id).or_default().push(event);
        }
    }

    {
        let mut statement = connection
            .prepare("SELECT job_id, body FROM daemon_jobs")
            .map_err(|error| store_error(path, "read daemon jobs", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| store_error(path, "read daemon jobs", error))?;
        for row in rows {
            let (job_id, body) =
                row.map_err(|error| store_error(path, "read daemon job", error))?;
            let mut stored: StoredJob = serde_json::from_str(&body)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
            // Digest the re-serialized value, not the stored text, so the
            // digest matches what a later commit computes for the same value.
            let body_digest = digest(&job_row_body(&mut stored)?);
            stored.events = events_by_job.remove(&job_id).unwrap_or_default();
            state.jobs.insert(
                stored.job.id,
                SyncedJob {
                    body_digest,
                    event_sequences: stored.events.iter().map(|e| e.sequence).collect(),
                },
            );
            durable.jobs.push(stored);
        }
    }

    {
        let mut statement = connection
            .prepare("SELECT kind, key, body FROM daemon_job_submissions")
            .map_err(|error| store_error(path, "read daemon job submissions", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| store_error(path, "read daemon job submissions", error))?;
        for row in rows {
            let (kind, key, body) =
                row.map_err(|error| store_error(path, "read daemon job submission", error))?;
            let invalid = |error| Error::config_invalid_json(path.display().to_string(), error);
            match kind.as_str() {
                REMOTE_RUNNER_KIND => {
                    let submission: RemoteRunnerSubmission =
                        serde_json::from_str(&body).map_err(invalid)?;
                    state.submissions.insert(
                        (REMOTE_RUNNER_KIND, key.clone()),
                        digest(&submission_body(&submission)?),
                    );
                    durable.submission_keys.insert(key, submission);
                }
                CONTROLLER_KIND => {
                    let submission: ControllerJobSubmission =
                        serde_json::from_str(&body).map_err(invalid)?;
                    state.submissions.insert(
                        (CONTROLLER_KIND, key.clone()),
                        digest(&submission_body(&submission)?),
                    );
                    durable.controller_submissions.insert(key, submission);
                }
                other => {
                    return Err(Error::internal_unexpected(format!(
                        "unknown daemon job submission kind `{other}`"
                    )))
                }
            }
        }
    }
    Ok((durable, state))
}

fn compaction_digest(compaction: &Option<JobStoreCompactionEvidence>) -> Result<u64> {
    serde_json::to_string(compaction)
        .map(|body| digest(&body))
        .map_err(|error| serialize_error(error, "daemon job store compaction evidence"))
}

fn submission_body<T: serde::Serialize>(submission: &T) -> Result<String> {
    serde_json::to_string(submission).map_err(|error| serialize_error(error, "job submission"))
}

/// Replay evidence for submission keys that expired in this mutation. Written
/// in the same transaction as the job rows that were removed.
fn expired_tombstones(durable: &DurableJobStore) -> Vec<ReplayTombstone> {
    let mut tombstones = Vec::new();
    for (key, submission) in &durable.expired_submission_keys {
        tombstones.push(ReplayTombstone {
            kind: ReplayTombstoneKind::RemoteRunner,
            key: key.clone(),
            fingerprint: submission.fingerprint.clone(),
            job_id: submission.job_id,
            terminal_job: None,
        });
    }
    for (key, submission) in &durable.expired_controller_submissions {
        tombstones.push(ReplayTombstone {
            kind: ReplayTombstoneKind::Controller,
            key: key.clone(),
            fingerprint: submission.fingerprint.clone(),
            job_id: submission.job_id,
            terminal_job: submission.terminal_job.clone(),
        });
    }
    tombstones
}

/// Write the difference between `durable` and the database state described by
/// `baseline`, then bump the commit generation. Returns the new sync state.
fn write_changes(
    transaction: &Transaction<'_>,
    path: &Path,
    durable: &mut DurableJobStore,
    baseline: &SqlSyncState,
) -> Result<SqlSyncState> {
    let sql =
        |operation: &'static str| move |error: rusqlite::Error| store_error(path, operation, error);
    let mut next = SqlSyncState::default();
    // A mutation that changes nothing commits nothing: no row is written and
    // the generation does not advance, so a reader's cached view stays valid.
    let mut changed = false;

    for tombstone in expired_tombstones(durable) {
        insert_tombstone(transaction, path, &tombstone)?;
        changed = true;
    }

    for stored in &mut durable.jobs {
        let id = stored.job.id;
        let body = job_row_body(stored)?;
        let body_digest = digest(&body);
        let prior = baseline.jobs.get(&id);
        let id_text = id.to_string();
        if prior.map(|prior| prior.body_digest) != Some(body_digest) {
            if let Some(controller) = &stored.controller_job {
                if controller.job_type == crate::daemon::step_job::STEP_JOB_TYPE {
                    if let Some(delivery) = crate::daemon::step_job::terminal_delivery(
                        &id.to_string(),
                        &controller.request,
                        controller.checkpoint.as_ref(),
                        stored.job.status.is_terminal().then_some(stored.job.status),
                        None,
                        controller.cancellation_requested,
                    )? {
                        super::store::terminal_delivery::retain_on(transaction, &delivery, false)?;
                    }
                }
            }
            changed = true;
            transaction
                .prepare_cached(
                    "INSERT INTO daemon_jobs (job_id, status, active, created_at_ms, updated_at_ms, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(job_id) DO UPDATE SET
                         status = excluded.status,
                         active = excluded.active,
                         created_at_ms = excluded.created_at_ms,
                         updated_at_ms = excluded.updated_at_ms,
                         body = excluded.body",
                )
                .and_then(|mut statement| {
                    statement.execute(params![
                        id_text,
                        stored.job.status.as_str(),
                        !stored.job.status.is_terminal() as i64,
                        stored.job.created_at_ms as i64,
                        stored.job.updated_at_ms as i64,
                        body,
                    ])
                })
                .map_err(sql("write daemon job"))?;
        }

        let sequences: Vec<u64> = stored.events.iter().map(|event| event.sequence).collect();
        let prior_sequences = prior.map(|prior| prior.event_sequences.as_slice());
        if prior_sequences != Some(sequences.as_slice()) {
            changed = true;
            let persisted: HashSet<u64> = prior_sequences
                .unwrap_or_default()
                .iter()
                .copied()
                .collect();
            let current: HashSet<u64> = sequences.iter().copied().collect();
            for event in &stored.events {
                if persisted.contains(&event.sequence) {
                    continue;
                }
                let event_body = serde_json::to_string(event)
                    .map_err(|error| serialize_error(error, "daemon job event"))?;
                transaction
                    .prepare_cached(
                        "INSERT OR REPLACE INTO daemon_job_events (job_id, sequence, body)
                         VALUES (?1, ?2, ?3)",
                    )
                    .and_then(|mut statement| {
                        statement.execute(params![id_text, event.sequence as i64, event_body])
                    })
                    .map_err(sql("insert daemon job event"))?;
            }
            for sequence in persisted.difference(&current) {
                transaction
                    .prepare_cached(
                        "DELETE FROM daemon_job_events WHERE job_id = ?1 AND sequence = ?2",
                    )
                    .and_then(|mut statement| statement.execute(params![id_text, *sequence as i64]))
                    .map_err(sql("delete daemon job event"))?;
            }
        }
        next.jobs.insert(
            id,
            SyncedJob {
                body_digest,
                event_sequences: sequences,
            },
        );
    }

    for id in baseline.jobs.keys() {
        if next.jobs.contains_key(id) {
            continue;
        }
        let id_text = id.to_string();
        transaction
            .execute(
                "DELETE FROM daemon_job_events WHERE job_id = ?1",
                [&id_text],
            )
            .map_err(sql("delete daemon job events"))?;
        transaction
            .execute("DELETE FROM daemon_jobs WHERE job_id = ?1", [&id_text])
            .map_err(sql("delete daemon job"))?;
    }

    let mut live_submissions: Vec<(&'static str, &String, String, Uuid)> = Vec::new();
    for (key, submission) in &durable.submission_keys {
        live_submissions.push((
            REMOTE_RUNNER_KIND,
            key,
            submission_body(submission)?,
            submission.job_id,
        ));
    }
    for (key, submission) in &durable.controller_submissions {
        live_submissions.push((
            CONTROLLER_KIND,
            key,
            submission_body(submission)?,
            submission.job_id,
        ));
    }
    for (kind, key, body, job_id) in live_submissions {
        let body_digest = digest(&body);
        let entry = (kind, key.clone());
        if baseline.submissions.get(&entry) != Some(&body_digest) {
            changed = true;
            transaction
                .prepare_cached(
                    "INSERT INTO daemon_job_submissions (kind, key, job_id, body)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(kind, key) DO UPDATE SET
                         job_id = excluded.job_id, body = excluded.body",
                )
                .and_then(|mut statement| {
                    statement.execute(params![kind, key, job_id.to_string(), body])
                })
                .map_err(sql("write daemon job submission"))?;
        }
        next.submissions.insert(entry, body_digest);
    }
    for (kind, key) in baseline.submissions.keys() {
        if next.submissions.contains_key(&(*kind, key.clone())) {
            continue;
        }
        changed = true;
        transaction
            .execute(
                "DELETE FROM daemon_job_submissions WHERE kind = ?1 AND key = ?2",
                params![kind, key],
            )
            .map_err(sql("delete daemon job submission"))?;
    }

    next.compaction_digest = compaction_digest(&durable.compaction)?;
    if next.compaction_digest != baseline.compaction_digest {
        changed = true;
    }
    if changed {
        let compaction = durable
            .compaction
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| serialize_error(error, "daemon job store compaction evidence"))?;
        transaction
            .execute(
                "UPDATE daemon_store_state SET generation = generation + 1, compaction = ?1 WHERE id = 1",
                [compaction],
            )
            .map_err(sql("advance daemon job store generation"))?;
    }
    next.generation =
        read_store_generation(transaction).map_err(sql("read daemon job store generation"))?;
    Ok(next)
}

/// Commit `durable` as one SQLite transaction. `synced` describes the database
/// state this process last observed; if the database has moved on (or the
/// state is unknown) the baseline is re-read inside the transaction, so a
/// stale cache can only cost extra reads, never a lost row.
///
/// Expired submission keys are written as replay tombstones in the same
/// transaction and cleared from `durable` once it commits.
pub(super) fn commit_sql_store(
    path: &Path,
    durable: &mut DurableJobStore,
    synced: &std::sync::Mutex<SqlSyncState>,
    terminal_deliveries: &[crate::daemon::step_job::StepTerminalDelivery],
) -> Result<()> {
    let known = std::mem::take(&mut *synced.lock().expect("store sync mutex poisoned"));
    let mut connection = open_sql_store(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| store_error(path, "begin daemon job store commit", error))?;
    let database_generation = read_store_generation(&transaction)
        .map_err(|error| store_error(path, "read daemon job store generation", error))?;
    let baseline = if known.generation.is_some() && known.generation == database_generation {
        known
    } else {
        load_sql_store(&transaction, path)?.1
    };
    for delivery in terminal_deliveries {
        super::store::terminal_delivery::retain_on(&transaction, delivery, false)?;
    }
    let next = write_changes(&transaction, path, durable, &baseline)?;
    transaction
        .commit()
        .map_err(|error| store_error(path, "commit daemon job store", error))?;
    durable.expired_submission_keys.clear();
    durable.expired_controller_submissions.clear();
    *synced.lock().expect("store sync mutex poisoned") = next;
    Ok(())
}

/// Read the durable store without initializing or reconciling it. `None`
/// means no job has been stored yet.
pub(super) fn read_durable_store_readonly(path: &Path) -> Result<Option<DurableJobStore>> {
    let Some(connection) = open_sql_store_readonly(path)? else {
        return Ok(None);
    };
    Ok(Some(load_sql_store(&connection, path)?.0))
}

/// Non-terminal jobs, read without events and without initializing the store.
pub(super) fn read_active_jobs_readonly(path: &Path) -> Result<Option<Vec<StoredJob>>> {
    let Some(connection) = open_sql_store_readonly(path)? else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare("SELECT body FROM daemon_jobs WHERE active = 1")
        .map_err(|error| store_error(path, "read active daemon jobs", error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| store_error(path, "read active daemon jobs", error))?;
    let mut jobs = Vec::new();
    for row in rows {
        let body = row.map_err(|error| store_error(path, "read active daemon job", error))?;
        jobs.push(
            serde_json::from_str(&body)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?,
        );
    }
    Ok(Some(jobs))
}

/// One job (without events) and the controller submissions that name it, for
/// exact terminal-custody lookups.
pub(super) fn read_job_and_controller_submissions(
    path: &Path,
    job_id: Uuid,
) -> Result<Option<(Option<StoredJob>, Vec<ControllerJobSubmission>)>> {
    let Some(connection) = open_sql_store_readonly(path)? else {
        return Ok(None);
    };
    let invalid = |error| Error::config_invalid_json(path.display().to_string(), error);
    let stored = connection
        .query_row(
            "SELECT body FROM daemon_jobs WHERE job_id = ?1",
            [job_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| store_error(path, "read daemon job", error))?
        .map(|body| serde_json::from_str::<StoredJob>(&body).map_err(invalid))
        .transpose()?;
    let mut statement = connection
        .prepare(
            "SELECT body FROM daemon_job_submissions WHERE kind = 'controller' AND job_id = ?1",
        )
        .map_err(|error| store_error(path, "read controller submissions", error))?;
    let rows = statement
        .query_map([job_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(|error| store_error(path, "read controller submissions", error))?;
    let mut submissions = Vec::new();
    for row in rows {
        let body = row.map_err(|error| store_error(path, "read controller submission", error))?;
        submissions.push(serde_json::from_str(&body).map_err(invalid)?);
    }
    Ok(Some((stored, submissions)))
}

pub(super) fn pending_step_delivery_count_readonly(path: &Path) -> Result<usize> {
    let Some(connection) = open_sql_store_readonly(path)? else {
        return Ok(0);
    };
    connection
        .query_row(
            "SELECT COUNT(*) FROM step_terminal_deliveries WHERE receipt IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| store_error(path, "count pending terminal deliveries", error))
}

/// The durable job ledger as one JSON document, for operator evidence
/// snapshots and tests. Not a persistence format.
pub(super) fn ledger_snapshot(path: &Path) -> Result<Vec<u8>> {
    match read_durable_store_readonly(path)? {
        Some(mut durable) => {
            durable
                .jobs
                .sort_by_key(|stored| (stored.job.created_at_ms, stored.job.id));
            // Submission maps are HashMaps; route through a Value so object
            // keys serialize sorted and equal stores yield identical bytes.
            let value = serde_json::to_value(&durable)
                .map_err(|error| serialize_error(error, "daemon job ledger snapshot"))?;
            serde_json::to_vec_pretty(&value)
                .map_err(|error| serialize_error(error, "daemon job ledger snapshot"))
        }
        None => Ok(b"{\"jobs\":[]}".to_vec()),
    }
}
