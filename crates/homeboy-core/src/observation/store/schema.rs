use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use super::{sqlite_error, ObservationDbStatus};
use crate::{paths, Result};

/// The only observation-store schema this binary creates or opens.
///
/// Homeboy keeps no migrations for older stores (#15742). The 28 historical
/// migrations were squashed into [`BASELINE_SCHEMA`]; a store recorded at an
/// older version is refused with an actionable error instead of upgraded.
/// Changing the schema means editing the baseline, bumping this version, and
/// renaming [`paths::OBSERVATION_DB_FILENAME`] to match, so the new schema gets
/// its own store file while binaries on the old schema keep theirs.
pub(crate) const SCHEMA_VERSION: i64 = 32;

/// The full observation-store schema at [`SCHEMA_VERSION`].
///
/// There is no upgrade path: a store at any other version is refused and the
/// operator recreates it. `baseline_schema_matches_the_pinned_fixture` pins
/// this against a committed snapshot so accidental drift is caught.
const BASELINE_SCHEMA: &str = r#"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    component_id TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,
    command TEXT,
    cwd TEXT,
    homeboy_version TEXT,
    git_sha TEXT,
    rig_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    -- Control-plane mission this run belongs to. A run is its own mission
    -- when unset; missions are derived from runs, never stored separately.
    mission_id TEXT
);

-- Equality filters followed by started_at let latest/list queries walk a
-- bounded index instead of sorting the full observation history.
CREATE INDEX idx_runs_started_at_desc
    ON runs(started_at DESC, id DESC);
CREATE INDEX idx_runs_status_started_at_desc
    ON runs(status, started_at DESC, id DESC);
CREATE INDEX idx_runs_kind_component_started_at_desc
    ON runs(kind, component_id, started_at DESC, id DESC);
-- Mission reads group a mission's runs and order missions by recency.
CREATE INDEX idx_runs_mission_started_at
    ON runs(mission_id, started_at ASC, id ASC)
    WHERE mission_id IS NOT NULL;
-- A run joins at most one mission, once: rebinding it to another mission
-- would silently move it between control-plane resources.
CREATE TRIGGER runs_mission_is_immutable
    BEFORE UPDATE OF mission_id ON runs
    WHEN OLD.mission_id IS NOT NULL AND NEW.mission_id IS NOT OLD.mission_id
BEGIN
    SELECT RAISE(ABORT, 'run cannot move to a different mission');
END;
-- Terminal retention selects known statuses before its finished-at cutoff.
CREATE INDEX idx_runs_status_finished_at
    ON runs(status, finished_at ASC, id ASC);
-- Retry reconciliation starts from its durable predecessor identity.
CREATE INDEX idx_runs_metadata_retry_of
    ON runs(json_extract(metadata_json, '$.agent_task_run.metadata.retry_of'));
-- A running runner-exec recovery is a controller-wide singleton; the unique
-- partial index makes the durable record itself the atomic claim.
CREATE UNIQUE INDEX idx_runs_one_running_runner_exec_recovery
    ON runs(kind)
    WHERE kind = 'runner_exec_recovery' AND status = 'running';
CREATE INDEX idx_agent_task_active_scope
    ON runs (
        json_extract(metadata_json, '$.agent_task_run.metadata.client_context.caller_context'),
        json_extract(metadata_json, '$.agent_task_run.metadata.caller_workspace.repository'),
        json_extract(metadata_json, '$.agent_task_run.metadata.caller_workspace.working_directory'),
        id
    )
    WHERE kind = 'agent-task'
      AND json_extract(metadata_json, '$.agent_task_run.state') IN ('queued', 'running')
      AND (json_type(metadata_json, '$.agent_task_run.metadata.caller_workspace') IS NOT NULL
           OR json_type(metadata_json, '$.agent_task_run.metadata.detached_cook_handoff') = 'object'
           OR json_type(metadata_json, '$.agent_task_run.metadata.unmaterialized_cook_admission') = 'object'
           OR json_type(metadata_json, '$.agent_task_run.metadata.cook_id') = 'text');

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    sha256 TEXT,
    size_bytes INTEGER,
    mime TEXT,
    created_at TEXT NOT NULL,
    artifact_type TEXT NOT NULL DEFAULT 'file',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    url TEXT,
    public_url TEXT,
    viewer_url TEXT,
    viewer_links_json TEXT NOT NULL DEFAULT '[]',
    artifact_handle TEXT,
    failure_diagnostic INTEGER,
    failure_diagnostic_rank TEXT,
    FOREIGN KEY(run_id) REFERENCES runs(id)
);

-- Artifact reads are ordered within a run, while retention walks the global
-- creation order. Keep both traversals index-backed. `artifact_handle` is the
-- short opaque identifier the bounded evidence projection exposes.
CREATE INDEX idx_artifacts_run_created
    ON artifacts(run_id, created_at ASC, id ASC);
CREATE INDEX idx_artifacts_created_at
    ON artifacts(created_at ASC, id ASC);
CREATE UNIQUE INDEX idx_artifacts_handle
    ON artifacts(artifact_handle);
CREATE INDEX idx_artifacts_failure_diagnostic
    ON artifacts(run_id, failure_diagnostic, failure_diagnostic_rank);
-- Artifact inventory pagination commonly narrows by these persisted fields.
CREATE INDEX idx_artifacts_run_kind_created
    ON artifacts(run_id, kind, created_at ASC, id ASC);
CREATE INDEX idx_artifacts_run_mime_created
    ON artifacts(run_id, mime, created_at ASC, id ASC);
CREATE INDEX idx_artifacts_run_fixture_created
    ON artifacts(run_id, json_extract(metadata_json, '$.fixture_id'), created_at ASC, id ASC);
CREATE INDEX idx_artifacts_run_surface_created
    ON artifacts(run_id, json_extract(metadata_json, '$.surface_id'), created_at ASC, id ASC);
CREATE INDEX idx_artifacts_run_scenario_created
    ON artifacts(run_id, json_extract(metadata_json, '$.scenario_id'), created_at ASC, id ASC);

-- A publication is journaled before final paths are materialized. On startup
-- unfinished intents are removed with their unowned files.
CREATE TABLE artifact_publication_intents (
    publication_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    staging_path TEXT NOT NULL,
    final_path TEXT NOT NULL,
    owner_token TEXT,
    lease_expires_at_ms INTEGER,
    PRIMARY KEY(publication_id, artifact_id)
);
CREATE INDEX idx_artifact_publication_intents_publication
    ON artifact_publication_intents(publication_id);
CREATE INDEX idx_artifact_publication_intents_lease
    ON artifact_publication_intents(lease_expires_at_ms);

CREATE TABLE trace_runs (
    run_id TEXT PRIMARY KEY,
    component_id TEXT NOT NULL,
    rig_id TEXT,
    scenario_id TEXT NOT NULL,
    status TEXT NOT NULL,
    baseline_status TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY(run_id) REFERENCES runs(id)
);
CREATE INDEX idx_trace_runs_component_scenario
    ON trace_runs(component_id, scenario_id);
CREATE INDEX idx_trace_runs_rig
    ON trace_runs(rig_id);

CREATE TABLE trace_spans (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    status TEXT NOT NULL,
    duration_ms REAL,
    from_event TEXT,
    to_event TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY(run_id) REFERENCES runs(id)
);
CREATE INDEX idx_trace_spans_run
    ON trace_spans(run_id);

CREATE TABLE findings (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    rule TEXT,
    file TEXT,
    line INTEGER,
    severity TEXT,
    fingerprint TEXT,
    message TEXT NOT NULL,
    fixable INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    FOREIGN KEY(run_id) REFERENCES runs(id)
);
CREATE INDEX idx_findings_run
    ON findings(run_id);
CREATE INDEX idx_findings_tool_file
    ON findings(tool, file);
CREATE INDEX idx_findings_fingerprint
    ON findings(fingerprint);

CREATE TABLE control_plane_event_appends (
    run_id TEXT NOT NULL,
    idempotency_digest TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    event_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_json TEXT,
    created_at TEXT NOT NULL,
    durable_event_json TEXT,
    PRIMARY KEY(run_id, idempotency_digest),
    UNIQUE(run_id, sequence),
    UNIQUE(event_id),
    FOREIGN KEY(run_id) REFERENCES runs(id),
    CHECK(length(idempotency_digest) = 64),
    CHECK(length(request_digest) = 64),
    CHECK(sequence > 0)
);
-- `run.cancelled` is a lifecycle authority, not telemetry (#15718): its payload
-- is copied into `durable_event_json`, which per-run payload pruning never
-- touches, so a cancellation outlives the bounded progress window.
CREATE INDEX idx_control_plane_event_appends_run_sequence
    ON control_plane_event_appends(run_id, sequence);
CREATE INDEX idx_control_plane_event_appends_run_retained
    ON control_plane_event_appends(run_id, sequence) WHERE event_json IS NOT NULL;
CREATE INDEX idx_control_plane_event_appends_run_durable
    ON control_plane_event_appends(run_id, sequence)
    WHERE durable_event_json IS NOT NULL;

-- The kernel owns action acceptance and replay even when a domain extension
-- owns the effect: the claim is the transactional intent/outbox a replacement
-- controller reconciles instead of repeating an interrupted effect.
CREATE TABLE control_plane_action_claims (
    run_id TEXT NOT NULL,
    idempotency_digest TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    state TEXT NOT NULL,
    owner_pid INTEGER NOT NULL,
    owner_start_identity_json TEXT,
    accepted_at TEXT NOT NULL,
    acknowledgement_json TEXT,
    completed_at TEXT,
    intent_json TEXT,
    fence_json TEXT,
    effect_id TEXT,
    outbox_state TEXT,
    lease_owner TEXT,
    lease_fence INTEGER NOT NULL DEFAULT 0,
    lease_expires_at TEXT,
    terminal_json TEXT,
    audit_json TEXT,
    PRIMARY KEY(run_id, idempotency_digest),
    FOREIGN KEY(run_id) REFERENCES runs(id),
    CHECK(length(idempotency_digest) = 64),
    CHECK(length(request_digest) = 64),
    CHECK(state IN ('running', 'completed'))
);
CREATE INDEX idx_control_plane_action_claims_run_state
    ON control_plane_action_claims(run_id, state);
CREATE UNIQUE INDEX idx_control_plane_action_claims_effect_id
    ON control_plane_action_claims(effect_id) WHERE effect_id IS NOT NULL;
CREATE INDEX idx_control_plane_action_claims_outbox
    ON control_plane_action_claims(outbox_state, lease_expires_at, accepted_at);

-- Action admission resource projections. Aliases are indexed separately so
-- reassignment is an explicit transaction.
CREATE TABLE control_plane_resources (
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    version TEXT NOT NULL,
    state TEXT NOT NULL,
    eligibility_json TEXT NOT NULL DEFAULT '{}',
    provenance_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(resource_type, resource_id)
);
CREATE TABLE control_plane_resource_aliases (
    resource_type TEXT NOT NULL,
    alias TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    PRIMARY KEY(resource_type, alias),
    FOREIGN KEY(resource_type, resource_id)
        REFERENCES control_plane_resources(resource_type, resource_id)
);
CREATE INDEX idx_control_plane_resource_aliases_target
    ON control_plane_resource_aliases(resource_type, resource_id);

-- A domain's pending work and its lifecycle record are committed together.
-- The daemon drains this indexed outbox into its separate durable job store.
-- A failed delivery keeps custody with its own durable backoff so it never
-- blocks newer intents.
CREATE TABLE control_plane_work_intents (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    receipt_json TEXT,
    created_at TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE,
    CHECK(state IN ('pending', 'submitted')),
    CHECK(version > 0)
);
CREATE INDEX idx_control_plane_work_intents_pending
    ON control_plane_work_intents(created_at, id) WHERE state = 'pending';

-- Generic coordination primitives (#15947). Each domain owns a namespace;
-- none of these rows reference runs, so coordination outlives run retention.
-- Mutable keyed state guarded by revision compare-and-set.
CREATE TABLE coordination_records (
    namespace TEXT NOT NULL,
    record_key TEXT NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    PRIMARY KEY(namespace, record_key),
    CHECK(revision > 0)
);
-- Claims, exclusive leases and counting semaphores: one row per holder.
-- Exclusivity or capacity is decided inside one IMMEDIATE transaction.
CREATE TABLE coordination_leases (
    namespace TEXT NOT NULL,
    lease_key TEXT NOT NULL,
    holder_token TEXT NOT NULL,
    fence INTEGER NOT NULL,
    owner_pid INTEGER,
    owner_host TEXT,
    owner_start_identity_json TEXT,
    acquired_at_ms INTEGER NOT NULL,
    renewed_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    weight_json TEXT NOT NULL DEFAULT '{}',
    payload_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(namespace, lease_key, holder_token),
    CHECK(fence > 0)
);
CREATE INDEX idx_coordination_leases_expiry
    ON coordination_leases(namespace, expires_at_ms);
-- The last fence issued per lease key. It survives lease deletion so a fence
-- never repeats across release and reacquire.
CREATE TABLE coordination_fences (
    namespace TEXT NOT NULL,
    lease_key TEXT NOT NULL,
    last_fence INTEGER NOT NULL,
    PRIMARY KEY(namespace, lease_key),
    CHECK(last_fence > 0)
);
-- Append-only coordination audit trail.
CREATE TABLE coordination_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    namespace TEXT NOT NULL,
    subject_key TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    event TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX idx_coordination_audit_subject
    ON coordination_audit(namespace, subject_key, sequence DESC);

-- The daemon's durable job ledger (`api_jobs::sql_store`). Ledger writers run
-- with `PRAGMA synchronous=FULL` on their own connections.
-- Compacted submission keys: permanent exactly-once replay evidence.
CREATE TABLE replay_tombstones (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    job_id TEXT NOT NULL,
    terminal_job TEXT,
    PRIMARY KEY (kind, key)
);
CREATE INDEX replay_tombstones_job_identity
    ON replay_tombstones (kind, job_id);
CREATE TABLE controller_completions (
    job_id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE step_terminal_deliveries (
    job_id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    receipt TEXT
);
CREATE INDEX step_terminal_deliveries_pending
    ON step_terminal_deliveries(next_attempt_ms) WHERE receipt IS NULL;
CREATE TABLE daemon_store_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    generation INTEGER NOT NULL,
    compaction TEXT
);
CREATE TABLE daemon_jobs (
    job_id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    active INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX daemon_jobs_active ON daemon_jobs (active);
CREATE TABLE daemon_job_events (
    job_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (job_id, sequence)
) WITHOUT ROWID;
CREATE INDEX daemon_job_events_sequence
    ON daemon_job_events (sequence);
CREATE TABLE daemon_job_submissions (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    job_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (kind, key)
);
CREATE INDEX daemon_job_submissions_job
    ON daemon_job_submissions (kind, job_id);

-- Workflow cadence registrations (`daemon::workflow_cadence`).
CREATE TABLE workflow_cadences (
    workflow TEXT PRIMARY KEY NOT NULL,
    next_due_ms INTEGER NOT NULL,
    body TEXT NOT NULL
);
"#;

static SCHEMA_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn database_path() -> Result<PathBuf> {
    paths::observation_db()
}

/// Read local observation-store status without creating the database.
pub(crate) fn status() -> Result<ObservationDbStatus> {
    let path = database_path()?;
    if !path.exists() {
        return Ok(ObservationDbStatus {
            path: path.to_string_lossy().to_string(),
            exists: false,
            schema_version: 0,
            migration_count: 0,
            table_count: 0,
        });
    }

    let connection = open_readonly_connection(&path)?;
    status_for_open_connection(&connection, path, true)
}

/// Create the baseline schema in an empty store, or accept a store already at
/// [`SCHEMA_VERSION`]. Anything else is refused: there is no upgrade path.
///
/// A store recorded at a *newer* version is accepted, matching the previous
/// behaviour where an older binary simply had no further migrations to apply.
pub(crate) fn ensure_schema(connection: &Connection, path: &Path) -> Result<()> {
    let _guard = SCHEMA_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| crate::Error::internal_unexpected("observation schema lock poisoned"))?;

    // The common case is an existing current store; read it without taking
    // the writer lock so ordinary opens stay as cheap as they were.
    if store_state(connection)? == StoreState::Current {
        return Ok(());
    }

    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(sqlite_error("begin observation schema initialization"))?;
    match store_state(&tx)? {
        StoreState::Current => {}
        StoreState::Empty => {
            tx.execute_batch(BASELINE_SCHEMA)
                .map_err(sqlite_error("create observation store baseline schema"))?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![SCHEMA_VERSION, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(sqlite_error("record observation store baseline schema"))?;
        }
        StoreState::Unsupported(found) => return Err(unsupported_store_error(path, found)),
    }
    tx.commit()
        .map_err(sqlite_error("commit observation schema initialization"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreState {
    /// No tables at all: a brand-new database file.
    Empty,
    /// Recorded at (or beyond) the version this binary creates.
    Current,
    /// Has tables but is unversioned (`None`) or recorded at an older version.
    Unsupported(Option<i64>),
}

fn store_state(connection: &Connection) -> Result<StoreState> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(if table_count(connection)? == 0 {
            StoreState::Empty
        } else {
            StoreState::Unsupported(None)
        });
    }
    let version = current_schema_version(connection)?;
    Ok(if version >= SCHEMA_VERSION {
        StoreState::Current
    } else {
        StoreState::Unsupported(Some(version))
    })
}

fn unsupported_store_error(path: &Path, found: Option<i64>) -> crate::Error {
    let found_label = found.map_or_else(
        || "an unversioned schema".to_string(),
        |version| format!("schema version {version}"),
    );
    let path_label = path.display().to_string();
    crate::Error::new(
        crate::ErrorCode::ConfigInvalidValue,
        format!(
            "Observation store {path_label} has {found_label}; this Homeboy only opens schema \
             version {SCHEMA_VERSION} and does not migrate older stores"
        ),
        serde_json::json!({
            "path": path_label,
            "found_schema_version": found,
            "required_schema_version": SCHEMA_VERSION,
        }),
    )
    .with_hint(format!(
        "Move or delete {path_label} (together with its -wal and -shm files) and rerun; Homeboy \
         creates a fresh store at schema version {SCHEMA_VERSION}. Local run history in the old \
         store is not carried over."
    ))
}

/// Open an initializing connection under a bounded, cross-process lease.
/// SQLite needs an exclusive lock to convert a rollback journal into WAL, and
/// the process-local schema mutex cannot coordinate separate CLI processes.
pub(crate) fn open_initialized_connection(path: &Path) -> Result<Connection> {
    with_initialization_lock(path, || {
        let connection = open_connection(path)?;
        ensure_incremental_auto_vacuum(&connection)?;
        ensure_wal_journal_mode(&connection)?;
        ensure_schema(&connection, path)?;
        Ok(connection)
    })
}

fn with_initialization_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    const TIMEOUT: Duration = Duration::from_secs(5);
    const RETRY: Duration = Duration::from_millis(25);

    let lock_path = path.with_extension("sqlite.init.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            crate::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "open observation initialization lock {}",
                    lock_path.display()
                )),
            )
        })?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => return operation(),
            Ok(false) => {}
            Err(error) => {
                return Err(crate::Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "acquire observation initialization lock {}",
                        lock_path.display()
                    )),
                ));
            }
        }
        if started.elapsed() >= TIMEOUT {
            return Err(crate::Error::observation_store_busy(
                path.to_string_lossy(),
                "initialize observation store",
                TIMEOUT.as_millis() as u64,
            ));
        }
        std::thread::sleep(RETRY);
    }
}

pub(crate) fn status_for_open_connection(
    connection: &Connection,
    path: PathBuf,
    exists: bool,
) -> Result<ObservationDbStatus> {
    Ok(ObservationDbStatus {
        path: path.to_string_lossy().to_string(),
        exists,
        schema_version: current_schema_version(connection)?,
        migration_count: migration_count(connection)?,
        table_count: table_count(connection)?,
    })
}

pub(crate) fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).map_err(sqlite_error(format!(
        "open observation store {}",
        path.display()
    )))?;
    super::install_control_plane_busy_wait(&connection)?;
    enforce_foreign_keys(&connection)?;
    Ok(connection)
}

/// Request incremental auto-vacuum so retention can return freed pages to the
/// filesystem (`PRAGMA incremental_vacuum`) without a full `VACUUM`. SQLite
/// only honours this on an empty database before it enters WAL mode, so it
/// must run first. The pragma is applied only to a brand-new file: on an
/// auto-vacuum database SQLite rewrites header meta[6] in a write transaction,
/// which would turn every read-only open into a file mutation.
fn ensure_incremental_auto_vacuum(connection: &Connection) -> Result<()> {
    let page_count: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .map_err(sqlite_error("read observation store page count"))?;
    if page_count > 0 {
        return Ok(());
    }
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .map_err(sqlite_error(
            "request observation store incremental auto-vacuum",
        ))
}

fn ensure_wal_journal_mode(connection: &Connection) -> Result<()> {
    let mode: String = connection
        .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
        .map_err(sqlite_error("read observation store journal mode"))?;
    if !mode.eq_ignore_ascii_case("wal") {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(sqlite_error("set observation store journal mode to WAL"))?;
    }
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(sqlite_error("set observation store synchronous mode"))?;
    Ok(())
}

/// Turn on the referential integrity this schema has always declared.
///
/// Unlike the journal pragmas above this is not best effort. `journal_mode`
/// can lose a race for the writer lock and degrade to different concurrency
/// behaviour; `foreign_keys` is pure per-connection state that cannot contend,
/// so a failure here means the connection is about to write under a
/// constraint set the schema claims and the database does not enforce. That is
/// the exact condition #11129 describes and it must not be recoverable by
/// carrying on.
///
/// The value is read back rather than assumed. `PRAGMA foreign_keys` is a
/// documented no-op inside a transaction and reports no error when it is
/// ignored, so "the statement succeeded" is not evidence the setting took.
fn enforce_foreign_keys(connection: &Connection) -> Result<()> {
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(sqlite_error("enable observation store foreign keys"))?;
    if !foreign_keys_enforced(connection)? {
        return Err(crate::Error::internal_unexpected(
            "observation store could not enable PRAGMA foreign_keys, so the FOREIGN KEY(run_id) \
             REFERENCES runs(id) constraints this schema declares would not be enforced and \
             deleting a run would orphan its artifacts, findings and trace rows",
        ));
    }
    Ok(())
}

pub(crate) fn foreign_keys_enforced(connection: &Connection) -> Result<bool> {
    let enforced: i64 = connection
        .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
        .map_err(sqlite_error(
            "read observation store foreign key enforcement",
        ))?;
    Ok(enforced == 1)
}

/// Open an existing observation store for a bounded metadata read.
///
/// This deliberately skips schema initialization and journal-mode configuration: both can
/// acquire writer locks even though the caller only needs persisted metadata.
pub(crate) fn open_readonly_connection(path: &Path) -> Result<Connection> {
    const READ_TIMEOUT: Duration = Duration::from_millis(750);
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| read_store_error(path, "open read-only observation store", error))?;
    connection
        .busy_timeout(READ_TIMEOUT)
        .map_err(|error| read_store_error(path, "configure read-only observation store", error))?;
    Ok(connection)
}

/// Open an existing store for a bounded scheduler write. This deliberately
/// avoids schema initialization, pragma maintenance, and artifact reconciliation: a
/// scheduler may only claim or terminalize its own durable work.
pub(crate) fn open_bounded_writer_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).map_err(sqlite_error(format!(
        "open bounded observation scheduler store {}",
        path.display()
    )))?;
    super::install_control_plane_busy_wait(&connection)?;
    enforce_foreign_keys(&connection)?;
    Ok(connection)
}

/// Open a connection for best-effort store maintenance. It bypasses the normal
/// five-second busy handler so its explicit bounded retry is the whole wait.
pub(crate) fn open_maintenance_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).map_err(sqlite_error(format!(
        "open observation maintenance store {}",
        path.display()
    )))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(sqlite_error(
            "configure observation maintenance busy timeout",
        ))?;
    enforce_foreign_keys(&connection)?;
    Ok(connection)
}

fn read_store_error(path: &Path, operation: &str, error: rusqlite::Error) -> crate::Error {
    if super::is_transient_lock_error(&error) {
        return crate::Error::observation_store_busy(path.to_string_lossy(), operation, 750);
    }
    sqlite_error(format!("{operation} {}", path.display()))(error)
}

fn current_schema_version(connection: &Connection) -> Result<i64> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(0);
    }

    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error("read current schema version"))
}

fn migration_count(connection: &Connection) -> Result<i64> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(0);
    }

    connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(sqlite_error("count schema migrations"))
}

fn table_count(connection: &Connection) -> Result<i64> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error("count observation tables"))
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(sqlite_error(format!("check table {}", table)))?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn store_file_name_is_scoped_to_the_schema_version() {
        assert_eq!(
            crate::paths::OBSERVATION_DB_FILENAME,
            format!("homeboy-v{}.sqlite", super::SCHEMA_VERSION)
        );
    }

    use super::*;

    /// An initializing open must persist WAL before it exposes the store.
    #[test]
    fn an_initializing_connection_applies_both_concurrency_pragmas() {
        let directory = tempfile::tempdir().expect("temp dir");
        let connection = open_initialized_connection(&directory.path().join("observations.sqlite"))
            .expect("open");

        let mode: String = connection
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous;", [], |row| row.get(0))
            .expect("read synchronous mode");
        assert_eq!(synchronous, 1, "NORMAL is SQLite synchronous level 1");
    }

    #[test]
    fn wal_initialization_returns_real_lock_failures() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        let blocker = Connection::open(&path).expect("open blocker");
        blocker
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .expect("hold exclusive lock");
        let contender = Connection::open(&path).expect("open contender");
        contender
            .busy_timeout(Duration::ZERO)
            .expect("disable contender wait");

        let error =
            ensure_wal_journal_mode(&contender).expect_err("WAL setup must fail while locked");

        assert!(
            error.to_string().contains("database is locked"),
            "the concrete SQLite lock failure must be preserved: {error}"
        );
    }

    /// The pragma is the whole point of #11129: without it every
    /// `FOREIGN KEY(run_id) REFERENCES runs(id)` in this schema is decorative,
    /// because SQLite enforces foreign keys per connection and defaults them
    /// off. A real open has to leave the connection enforcing them.
    #[test]
    fn a_real_open_enforces_the_foreign_keys_the_schema_declares() {
        let directory = tempfile::tempdir().expect("temp dir");
        let connection =
            open_connection(&directory.path().join("observations.sqlite")).expect("open");

        assert!(
            foreign_keys_enforced(&connection).expect("read enforcement"),
            "the schema declares foreign keys, so the connection must enforce them"
        );
    }

    /// The schema a fresh store has at [`SCHEMA_VERSION`], captured with
    /// [`schema_snapshot`]. Each object's `sql` is kept verbatim so tests can
    /// also rebuild such a store. Regenerate it with
    /// `HOMEBOY_WRITE_SCHEMA_FIXTURE=1 cargo test -p homeboy-core --lib
    /// baseline_schema_matches_the_pinned_fixture` after an intended change.
    const SCHEMA_FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/observation-schema/v32-baseline-schema.json"
    );
    const MIGRATED_SCHEMA_FIXTURE: &str =
        include_str!("../../../tests/fixtures/observation-schema/v32-baseline-schema.json");

    fn migrated_schema_fixture() -> serde_json::Value {
        serde_json::from_str(MIGRATED_SCHEMA_FIXTURE).expect("schema fixture")
    }

    /// The baseline must match the pinned fixture exactly: every table,
    /// column (order, type, NOT NULL, default, primary-key position), foreign
    /// key, CHECK and UNIQUE constraint, index (columns, collation, direction,
    /// uniqueness, partial predicate) and trigger. `sql` text is compared after
    /// collapsing whitespace only. Any difference is a schema change, which
    /// needs a new [`SCHEMA_VERSION`] because stores are never migrated.
    #[test]
    fn baseline_schema_matches_the_pinned_fixture() {
        let directory = tempfile::tempdir().expect("temp dir");
        let connection = open_initialized_connection(&directory.path().join("observations.sqlite"))
            .expect("create baseline store");
        if std::env::var_os("HOMEBOY_WRITE_SCHEMA_FIXTURE").is_some() {
            let mut snapshot = schema_snapshot(&connection);
            snapshot["schema_version"] = SCHEMA_VERSION.into();
            snapshot["recorded_versions"] = serde_json::json!([SCHEMA_VERSION]);
            std::fs::write(
                SCHEMA_FIXTURE_PATH,
                serde_json::to_string_pretty(&snapshot).unwrap() + "\n",
            )
            .expect("write schema fixture");
        }
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(SCHEMA_FIXTURE_PATH).expect("read schema fixture"),
        )
        .expect("schema fixture");
        assert_eq!(fixture["schema_version"], SCHEMA_VERSION);

        let mut expected = fixture.clone();
        let expected = expected.as_object_mut().expect("fixture object");
        expected.remove("schema_version");
        expected.remove("recorded_versions");
        let expected = normalize_snapshot(serde_json::Value::Object(expected.clone()));
        let actual = normalize_snapshot(schema_snapshot(&connection));

        assert_eq!(
            serde_json::to_string_pretty(&actual).unwrap(),
            serde_json::to_string_pretty(&expected).unwrap(),
            "the baseline schema drifted from the pinned fixture; bump SCHEMA_VERSION and regenerate it"
        );
        assert_eq!(recorded_versions(&connection), vec![SCHEMA_VERSION]);
    }

    /// A store recorded at any older version is refused with an actionable
    /// error, and refusing it does not write anything to it.
    #[test]
    fn a_store_at_an_older_schema_version_is_refused() {
        for version in [1, 8, 20, SCHEMA_VERSION - 1] {
            let directory = tempfile::tempdir().expect("temp dir");
            let path = directory.path().join("observations.sqlite");
            {
                let connection = Connection::open(&path).expect("create old store");
                replay_migrated_schema(&connection, 1..=version);
            }
            let before = store_fingerprint(&path);

            let error = match open_initialized_connection(&path) {
                Ok(_) => panic!("a version {version} store must be refused"),
                Err(error) => error,
            };

            assert_eq!(error.code, crate::ErrorCode::ConfigInvalidValue);
            assert!(
                error
                    .message
                    .contains(&format!("has schema version {version};")),
                "the error names the version it found: {}",
                error.message
            );
            assert!(
                error.message.contains(&path.display().to_string()),
                "the error names the store: {}",
                error.message
            );
            assert_eq!(error.details["found_schema_version"], version);
            assert_eq!(error.details["required_schema_version"], SCHEMA_VERSION);
            let hint = &error.hints.first().expect("actionable hint").message;
            assert!(
                hint.contains("Move or delete") && hint.contains(&path.display().to_string()),
                "the hint tells the operator how to recover: {hint}"
            );
            assert_eq!(
                store_fingerprint(&path),
                before,
                "refusing a version {version} store must not modify it"
            );
        }
    }

    /// Tables without a version record are not an empty store either.
    #[test]
    fn an_unversioned_store_with_tables_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        Connection::open(&path)
            .expect("create unversioned store")
            .execute_batch("CREATE TABLE runs (id TEXT PRIMARY KEY);")
            .expect("seed unversioned table");

        let error = match open_initialized_connection(&path) {
            Ok(_) => panic!("an unversioned store must be refused"),
            Err(error) => error,
        };

        assert!(
            error.message.contains("has an unversioned schema"),
            "{}",
            error.message
        );
        assert!(error.details["found_schema_version"].is_null());
        assert!(
            !error.hints.is_empty(),
            "the refusal carries a recovery hint"
        );
    }

    /// A store already at the current version, with its rows, opens without a
    /// single change.
    #[test]
    fn a_store_at_the_current_version_opens_unchanged() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        {
            let connection = Connection::open(&path).expect("create current store");
            replay_migrated_schema(&connection, SCHEMA_VERSION..=SCHEMA_VERSION);
            connection
                .execute_batch(
                    "INSERT INTO runs(id, kind, started_at, status) \
                     VALUES ('kept', 'test', '2026-01-01T00:00:00Z', 'pass'); \
                     INSERT INTO artifacts(id, run_id, kind, path, created_at) \
                     VALUES ('artifact', 'kept', 'log', '/kept', '2026-01-01T00:00:00Z');",
                )
                .expect("seed current store");
        }
        let before = store_fingerprint(&path);

        let connection = open_initialized_connection(&path).expect("open current store");
        let status = status_for_open_connection(&connection, path.clone(), true).expect("status");
        drop(connection);

        let historical_records = migrated_schema_fixture()["recorded_versions"]
            .as_array()
            .expect("recorded versions")
            .len() as i64;
        assert_eq!(status.schema_version, SCHEMA_VERSION);
        assert_eq!(status.migration_count, historical_records);
        assert_eq!(store_fingerprint(&path), before);
    }

    /// Reopening a store this binary created is equally a no-op.
    #[test]
    fn reopening_a_baseline_store_is_a_no_op() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("observations.sqlite");
        drop(open_initialized_connection(&path).expect("create"));
        let before = store_fingerprint(&path);

        drop(open_initialized_connection(&path).expect("reopen"));

        assert_eq!(store_fingerprint(&path), before);
    }

    /// Rebuild a store exactly as the historical chain left it: the captured
    /// object DDL verbatim, with `versions` recorded in `schema_migrations`.
    fn replay_migrated_schema(connection: &Connection, versions: std::ops::RangeInclusive<i64>) {
        let fixture = migrated_schema_fixture();
        let objects = fixture["objects"].as_array().expect("fixture objects");
        for kind in ["table", "index", "trigger", "view"] {
            for object in objects.iter().filter(|object| object["type"] == kind) {
                if let Some(sql) = object["sql"].as_str() {
                    connection
                        .execute_batch(sql)
                        .expect("replay fixture object");
                }
            }
        }
        for version in versions {
            connection
                .execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'fixture')",
                    [version],
                )
                .expect("record fixture version");
        }
    }

    /// Everything observable about a store: its schema, its version records,
    /// and every row of every table.
    fn store_fingerprint(path: &Path) -> serde_json::Value {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("inspect");
        let mut snapshot = schema_snapshot(&connection);
        let tables: Vec<String> = snapshot["tables"]
            .as_object()
            .expect("tables")
            .keys()
            .cloned()
            .collect();
        let mut contents = serde_json::Map::new();
        for table in tables {
            // Some tables are WITHOUT ROWID, so order rows by their content.
            let mut rows = query_rows(&connection, &format!("SELECT * FROM \"{table}\""), None);
            rows.sort_by_key(|row| row.to_string());
            contents.insert(table.clone(), rows.into());
        }
        snapshot["contents"] = contents.into();
        snapshot
    }

    fn recorded_versions(connection: &Connection) -> Vec<i64> {
        connection
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Collapse whitespace in every `sql` text; nothing else is normalized.
    fn normalize_snapshot(mut snapshot: serde_json::Value) -> serde_json::Value {
        for object in snapshot["objects"].as_array_mut().expect("objects") {
            if let Some(sql) = object["sql"].as_str() {
                object["sql"] = sql.split_whitespace().collect::<Vec<_>>().join(" ").into();
            }
        }
        snapshot
    }

    /// Structural snapshot of every schema object SQLite knows about. This is
    /// the same function that produced `MIGRATED_SCHEMA_FIXTURE`.
    fn schema_snapshot(connection: &Connection) -> serde_json::Value {
        let objects = query_rows(
            connection,
            "SELECT type, name, tbl_name, sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' OR type = 'index' ORDER BY type, name",
            None,
        );
        let mut tables = serde_json::Map::new();
        let mut indexes = serde_json::Map::new();
        for object in &objects {
            let name = object["name"].as_str().unwrap();
            match object["type"].as_str().unwrap() {
                "table" => {
                    tables.insert(
                        name.to_string(),
                        serde_json::json!({
                            "columns": query_rows(connection, "SELECT * FROM pragma_table_xinfo(?1) ORDER BY cid", Some(name)),
                            "foreign_keys": query_rows(connection, "SELECT * FROM pragma_foreign_key_list(?1) ORDER BY id, seq", Some(name)),
                            "indexes": query_rows(connection, "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1) ORDER BY name", Some(name)),
                        }),
                    );
                }
                "index" => {
                    indexes.insert(
                        name.to_string(),
                        serde_json::json!({
                            "columns": query_rows(connection, "SELECT * FROM pragma_index_xinfo(?1) ORDER BY seqno", Some(name)),
                        }),
                    );
                }
                _ => {}
            }
        }
        serde_json::json!({ "objects": objects, "tables": tables, "indexes": indexes })
    }

    fn query_rows(connection: &Connection, sql: &str, arg: Option<&str>) -> Vec<serde_json::Value> {
        let mut statement = connection.prepare(sql).unwrap();
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect();
        let map = |row: &rusqlite::Row<'_>| {
            let mut object = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                    rusqlite::types::ValueRef::Integer(value) => value.into(),
                    rusqlite::types::ValueRef::Real(value) => value.into(),
                    rusqlite::types::ValueRef::Text(value) => {
                        String::from_utf8_lossy(value).into_owned().into()
                    }
                    rusqlite::types::ValueRef::Blob(_) => "<blob>".into(),
                };
                object.insert(name.clone(), value);
            }
            Ok(serde_json::Value::Object(object))
        };
        match arg {
            Some(arg) => statement.query_map([arg], map),
            None => statement.query_map([], map),
        }
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    }
}
