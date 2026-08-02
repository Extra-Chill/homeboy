use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::{sqlite_error, ObservationDbStatus};
use crate::{paths, Result};

struct Migration {
    version: i64,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runs (
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
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );

        CREATE TABLE IF NOT EXISTS artifacts (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            sha256 TEXT,
            size_bytes INTEGER,
            mime TEXT,
            created_at TEXT NOT NULL,
            FOREIGN KEY(run_id) REFERENCES runs(id)
        );
    "#,
    },
    Migration {
        version: 2,
        sql: r#"
        CREATE TABLE IF NOT EXISTS trace_runs (
            run_id TEXT PRIMARY KEY,
            component_id TEXT NOT NULL,
            rig_id TEXT,
            scenario_id TEXT NOT NULL,
            status TEXT NOT NULL,
            baseline_status TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            FOREIGN KEY(run_id) REFERENCES runs(id)
        );

        CREATE TABLE IF NOT EXISTS trace_spans (
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

        CREATE INDEX IF NOT EXISTS idx_trace_runs_component_scenario
            ON trace_runs(component_id, scenario_id);
        CREATE INDEX IF NOT EXISTS idx_trace_runs_rig
            ON trace_runs(rig_id);
        CREATE INDEX IF NOT EXISTS idx_trace_spans_run
            ON trace_spans(run_id);
    "#,
    },
    Migration {
        version: 3,
        sql: r#"
        CREATE TABLE IF NOT EXISTS findings (
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

        CREATE INDEX IF NOT EXISTS idx_findings_run
            ON findings(run_id);
        CREATE INDEX IF NOT EXISTS idx_findings_tool_file
            ON findings(tool, file);
        CREATE INDEX IF NOT EXISTS idx_findings_fingerprint
            ON findings(fingerprint);
    "#,
    },
    Migration {
        version: 4,
        sql: "",
    },
    Migration {
        version: 5,
        sql: r#"
        CREATE TABLE IF NOT EXISTS triage_items (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            repo_owner TEXT NOT NULL,
            repo_name TEXT NOT NULL,
            item_type TEXT NOT NULL,
            number INTEGER NOT NULL,
            state TEXT NOT NULL,
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            checks TEXT,
            review_decision TEXT,
            merge_state TEXT,
            next_action TEXT,
            comments_count INTEGER,
            reviews_count INTEGER,
            last_comment_at TEXT,
            last_review_at TEXT,
            updated_at TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            observed_at TEXT NOT NULL,
            FOREIGN KEY(run_id) REFERENCES runs(id)
        );

        CREATE INDEX IF NOT EXISTS idx_triage_items_run
            ON triage_items(run_id);
        CREATE INDEX IF NOT EXISTS idx_triage_items_repo_item
            ON triage_items(provider, repo_owner, repo_name, item_type, number);
    "#,
    },
    Migration {
        version: 6,
        sql: "",
    },
    Migration {
        version: 7,
        sql: "",
    },
    Migration {
        version: 8,
        sql: r#"
        -- Equality filters followed by started_at let latest/list queries walk
        -- a bounded index instead of sorting the full observation history.
        CREATE INDEX IF NOT EXISTS idx_runs_started_at_desc
            ON runs(started_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_runs_status_started_at_desc
            ON runs(status, started_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_runs_kind_component_started_at_desc
            ON runs(kind, component_id, started_at DESC, id DESC);
        -- Terminal retention selects known statuses before its finished-at
        -- cutoff, so this avoids scanning the full history on each cleanup.
        CREATE INDEX IF NOT EXISTS idx_runs_status_finished_at
            ON runs(status, finished_at ASC, id ASC);
        "#,
    },
    Migration {
        version: 9,
        sql: r#"
        -- Artifact reads are ordered within a run, while retention walks the
        -- global creation order. Keep both traversals index-backed.
        CREATE INDEX IF NOT EXISTS idx_artifacts_run_created
            ON artifacts(run_id, created_at ASC, id ASC);
        CREATE INDEX IF NOT EXISTS idx_artifacts_created_at
            ON artifacts(created_at ASC, id ASC);
        "#,
    },
    Migration {
        version: 10,
        sql: r#"
        -- Retry reconciliation starts from its durable predecessor identity.
        -- Keep this JSON projection indexed so it never walks run history.
        CREATE INDEX IF NOT EXISTS idx_runs_metadata_retry_of
            ON runs(json_extract(metadata_json, '$.agent_task_run.metadata.retry_of'));
        "#,
    },
    Migration {
        version: 11,
        sql: r#"
        -- A publication is journaled before final paths are materialized. On
        -- startup unfinished intents are removed with their unowned files.
        CREATE TABLE IF NOT EXISTS artifact_publication_intents (
            publication_id TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            staging_path TEXT NOT NULL,
            final_path TEXT NOT NULL,
            PRIMARY KEY(publication_id, artifact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_artifact_publication_intents_publication
            ON artifact_publication_intents(publication_id);
        "#,
    },
    Migration {
        version: 12,
        sql: "",
    },
    Migration {
        version: 13,
        sql: r#"
        -- Every child table has declared FOREIGN KEY(run_id) REFERENCES runs(id)
        -- since it was created, but SQLite enforces foreign keys per connection
        -- and defaults them off, and the pragma was never set. The schema
        -- documented an invariant the database did not hold: deleting a run
        -- left its children behind, and an orphaned `artifacts` row keeps
        -- pointing at a path whose bytes retention has already reclaimed.
        --
        -- Enforcement only applies to statements issued after it is enabled, so
        -- rows that accumulated while it was off would survive indefinitely and
        -- keep contradicting the constraint. Reap them once, here, so the
        -- pragma is turned on against a database that actually satisfies it.
        --
        -- NOT EXISTS rather than NOT IN: `runs.id` is a TEXT primary key, which
        -- SQLite permits to be NULL, and a single NULL would make every NOT IN
        -- comparison NULL and silently reap nothing.
        DELETE FROM artifacts
            WHERE NOT EXISTS (SELECT 1 FROM runs WHERE runs.id = artifacts.run_id);
        DELETE FROM findings
            WHERE NOT EXISTS (SELECT 1 FROM runs WHERE runs.id = findings.run_id);
        DELETE FROM triage_items
            WHERE NOT EXISTS (SELECT 1 FROM runs WHERE runs.id = triage_items.run_id);
        DELETE FROM trace_spans
            WHERE NOT EXISTS (SELECT 1 FROM runs WHERE runs.id = trace_spans.run_id);
        DELETE FROM trace_runs
            WHERE NOT EXISTS (SELECT 1 FROM runs WHERE runs.id = trace_runs.run_id);
        "#,
    },
];

/// The schema version a freshly initialized store lands on.
///
/// Derived from `MIGRATIONS` rather than written down separately. A
/// hand-maintained copy drifted: migration 12 was added and the constant stayed
/// at 11, so a correctly migrated store reported a version its own code did not
/// recognise as current. Five store-initialization tests had been failing on
/// that for as long as the drift existed, unseen because member-crate lib tests
/// were compiled and never executed (#10477).
/// How many migrations a fully initialized store has applied.
///
/// Derived for the same reason as [`LATEST_MIGRATION_VERSION`]: the test suite
/// asserted a hand-written `11` and adding migration 12 silently invalidated it.
pub(crate) const MIGRATION_COUNT: i64 = MIGRATIONS.len() as i64;

pub(crate) const LATEST_MIGRATION_VERSION: i64 = {
    assert!(!MIGRATIONS.is_empty(), "the store must declare a schema");
    MIGRATIONS[MIGRATIONS.len() - 1].version
};

static MIGRATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

    let connection = open_connection(&path)?;
    status_for_open_connection(&connection, path, true)
}

pub(crate) fn apply_migrations(connection: &Connection) -> Result<()> {
    let _guard = MIGRATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| crate::Error::internal_unexpected("observation migration lock poisoned"))?;

    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
        "#,
        )
        .map_err(sqlite_error("create schema_migrations"))?;

    for migration in MIGRATIONS {
        if migration_applied(connection, migration.version)? {
            continue;
        }

        let tx = connection
            .unchecked_transaction()
            .map_err(sqlite_error("begin observation migration"))?;
        if migration_applied(&tx, migration.version)? {
            tx.commit().map_err(sqlite_error(format!(
                "commit migration {}",
                migration.version
            )))?;
            continue;
        }
        apply_migration_sql(&tx, migration)?;
        tx.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![migration.version, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(sqlite_error(format!(
            "record migration {}",
            migration.version
        )))?;
        tx.commit().map_err(sqlite_error(format!(
            "commit migration {}",
            migration.version
        )))?;
    }

    Ok(())
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
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sqlite_error("configure observation store busy timeout"))?;
    // WAL journaling lets readers and a single writer proceed concurrently,
    // which sharply reduces transient "database is locked" contention when
    // multiple homeboy processes touch the observation store at once.
    // pragma_update may itself momentarily contend on the lock, so it stays
    // best effort: the busy_timeout above plus the write-retry wrapper still
    // apply.
    //
    // Best effort is not the same as silent. A discarded failure leaves the
    // store running in rollback-journal mode with materially different
    // concurrency behaviour and nobody told, so the next "database is locked"
    // report has no way to reach its own cause.
    for warning in apply_journal_pragmas(&connection) {
        warn_pragma_once(&warning);
    }
    enforce_foreign_keys(&connection)?;
    Ok(connection)
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
             deleting a run would orphan its artifacts, findings, triage items and trace rows",
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

/// Apply the concurrency pragmas, returning a warning per pragma that did not
/// take. Pure so every branch is reachable without a contended database.
fn apply_journal_pragmas(connection: &Connection) -> Vec<String> {
    [("journal_mode", "WAL"), ("synchronous", "NORMAL")]
        .into_iter()
        .filter_map(|(pragma, value)| {
            connection
                .pragma_update(None, pragma, value)
                .err()
                .map(|error| pragma_warning(pragma, value, &error.to_string()))
        })
        .collect()
}

fn pragma_warning(pragma: &str, value: &str, error: &str) -> String {
    format!(
        "observation store could not set PRAGMA {pragma}={value}: {error}. The store keeps \
         working with different concurrency behaviour; expect more transient \
         `observation_store.busy` errors."
    )
}

/// The condition is a property of the database, not of one open, and
/// `open_connection` runs on nearly every command. Warning once per process
/// keeps it visible without turning it into noise that gets filtered out.
fn warn_pragma_once(warning: &str) {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("Warning: {warning}");
    });
}

/// Open an existing observation store for a bounded metadata read.
///
/// This deliberately skips migrations and journal-mode configuration: both can
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

fn read_store_error(path: &Path, operation: &str, error: rusqlite::Error) -> crate::Error {
    if super::is_transient_lock_error(&error) {
        return crate::Error::observation_store_busy(path.to_string_lossy(), operation, 750);
    }
    sqlite_error(format!("{operation} {}", path.display()))(error)
}

fn apply_migration_sql(connection: &Connection, migration: &Migration) -> Result<()> {
    if migration.version == 4 {
        if !column_exists(connection, "artifacts", "artifact_type")? {
            connection
                .execute_batch(
                    r#"
                    ALTER TABLE artifacts
                        ADD COLUMN artifact_type TEXT NOT NULL DEFAULT 'file';
                    "#,
                )
                .map_err(sqlite_error("apply migration 4"))?;
        }
        return Ok(());
    }

    if migration.version == 6 {
        if !column_exists(connection, "artifacts", "metadata_json")? {
            connection
                .execute_batch(
                    r#"
                    ALTER TABLE artifacts
                        ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';
                    "#,
                )
                .map_err(sqlite_error("apply migration 6"))?;
        }
        return Ok(());
    }

    if migration.version == 7 {
        for (column, definition) in [
            ("url", "TEXT"),
            ("public_url", "TEXT"),
            ("viewer_url", "TEXT"),
            ("viewer_links_json", "TEXT NOT NULL DEFAULT '[]'"),
        ] {
            if !column_exists(connection, "artifacts", column)? {
                connection
                    .execute_batch(&format!(
                        "ALTER TABLE artifacts ADD COLUMN {column} {definition};"
                    ))
                    .map_err(sqlite_error("apply migration 7"))?;
            }
        }
        return Ok(());
    }

    if migration.version == 12 {
        for (column, definition) in [("owner_token", "TEXT"), ("lease_expires_at_ms", "INTEGER")] {
            if !column_exists(connection, "artifact_publication_intents", column)? {
                connection
                    .execute_batch(&format!(
                        "ALTER TABLE artifact_publication_intents ADD COLUMN {column} {definition};"
                    ))
                    .map_err(sqlite_error("apply migration 12"))?;
            }
        }
        connection
            .execute_batch(
                r#"
                CREATE INDEX IF NOT EXISTS idx_artifact_publication_intents_lease
                    ON artifact_publication_intents(lease_expires_at_ms);
                "#,
            )
            .map_err(sqlite_error("apply migration 12"))?;
        return Ok(());
    }

    connection
        .execute_batch(migration.sql)
        .map_err(sqlite_error(format!(
            "apply migration {}",
            migration.version
        )))
}

fn migration_applied(connection: &Connection, version: i64) -> Result<bool> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
            [version],
            |row| row.get(0),
        )
        .map_err(sqlite_error(format!("check migration {}", version)))?;
    Ok(count > 0)
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

fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_error(format!("inspect table {table}")))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error(format!("list columns for {table}")))?;

    for row in rows {
        if row.map_err(sqlite_error(format!("read column for {table}")))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real open must take both pragmas. Failure used to be discarded with
    /// `let _ =`, so the store could silently fall back to rollback-journal
    /// mode — different concurrency behaviour, nobody told, and the next
    /// "database is locked" report unable to reach its own cause (#11127).
    #[test]
    fn a_healthy_connection_applies_both_concurrency_pragmas_without_warning() {
        let directory = tempfile::tempdir().expect("temp dir");
        let connection =
            open_connection(&directory.path().join("observations.sqlite")).expect("open");

        assert!(
            apply_journal_pragmas(&connection).is_empty(),
            "a healthy store must take both pragmas"
        );
        let mode: String = connection
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    /// The point of the change: a failure becomes words, not silence. The
    /// warning has to name the pragma, the value, and the consequence.
    #[test]
    fn a_discarded_pragma_failure_becomes_an_actionable_warning() {
        let warning = pragma_warning("journal_mode", "WAL", "database is locked");

        assert!(warning.contains("journal_mode=WAL"), "{warning}");
        assert!(warning.contains("database is locked"), "{warning}");
        assert!(
            warning.contains("observation_store.busy"),
            "the warning must name what the operator will see instead: {warning}"
        );
    }

    /// Every pragma that does not take produces exactly one warning, so a
    /// second silent fallback cannot be introduced without a test noticing.
    #[test]
    fn one_warning_is_produced_per_pragma_that_did_not_take() {
        let warnings: Vec<_> = [("journal_mode", "WAL"), ("synchronous", "NORMAL")]
            .into_iter()
            .map(|(pragma, value)| pragma_warning(pragma, value, "disk I/O error"))
            .collect();

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|warning| warning.contains("PRAGMA")));
    }

    #[test]
    fn migration_12_adds_both_missing_columns() {
        assert_migration_12_completes(false, false);
    }

    #[test]
    fn migration_12_preserves_preexisting_owner_token() {
        assert_migration_12_completes(true, false);
    }

    #[test]
    fn migration_12_preserves_preexisting_lease_expiry() {
        assert_migration_12_completes(false, true);
    }

    #[test]
    fn migration_12_accepts_both_preexisting_columns() {
        assert_migration_12_completes(true, true);
    }

    fn assert_migration_12_completes(has_owner_token: bool, has_lease_expiry: bool) {
        let connection = schema_through_migration_11();
        connection
            .execute_batch(
                "INSERT INTO artifact_publication_intents \
                 (publication_id, artifact_id, staging_path, final_path) \
                 VALUES ('publication', 'artifact', '/staging', '/final');",
            )
            .unwrap();

        if has_owner_token {
            connection
                .execute_batch(
                    "ALTER TABLE artifact_publication_intents ADD COLUMN owner_token TEXT; \
                     UPDATE artifact_publication_intents SET owner_token = 'existing-owner';",
                )
                .unwrap();
        }
        if has_lease_expiry {
            connection
                .execute_batch(
                    "ALTER TABLE artifact_publication_intents \
                         ADD COLUMN lease_expires_at_ms INTEGER; \
                     UPDATE artifact_publication_intents SET lease_expires_at_ms = 4242;",
                )
                .unwrap();
        }
        if has_owner_token && has_lease_expiry {
            connection
                .execute_batch(
                    "CREATE INDEX idx_artifact_publication_intents_lease \
                     ON artifact_publication_intents(lease_expires_at_ms);",
                )
                .unwrap();
        }

        apply_migrations(&connection).unwrap();

        assert!(column_exists(&connection, "artifact_publication_intents", "owner_token").unwrap());
        assert!(column_exists(
            &connection,
            "artifact_publication_intents",
            "lease_expires_at_ms"
        )
        .unwrap());
        let values: (Option<String>, Option<i64>) = connection
            .query_row(
                "SELECT owner_token, lease_expires_at_ms \
                 FROM artifact_publication_intents \
                 WHERE publication_id = 'publication' AND artifact_id = 'artifact'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            values,
            (
                has_owner_token.then(|| "existing-owner".to_owned()),
                has_lease_expiry.then_some(4242),
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'index' \
                       AND name = 'idx_artifact_publication_intents_lease'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE version = 12",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
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

    /// Rows that accumulated while enforcement was off are invisible to the
    /// pragma -- it only constrains statements issued after it is enabled -- so
    /// they would contradict the constraint forever. Migration 13 reaps them.
    #[test]
    fn migration_13_reaps_rows_orphaned_while_enforcement_was_off() {
        let connection = schema_through_migration(12);
        seed_owned_and_orphaned_children(&connection);

        apply_migrations(&connection).unwrap();

        for table in [
            "artifacts",
            "findings",
            "triage_items",
            "trace_spans",
            "trace_runs",
        ] {
            let surviving: Vec<String> = surviving_run_ids(&connection, table);
            assert_eq!(
                surviving,
                vec!["live".to_string()],
                "{table} must keep its owned row and lose its orphan"
            );
        }
    }

    /// A `NULL` id is legal in a TEXT primary key, and one of them turns every
    /// `run_id NOT IN (SELECT id FROM runs)` comparison into NULL -- reaping
    /// nothing, silently. The migration uses NOT EXISTS for exactly this.
    #[test]
    fn a_null_run_id_does_not_disarm_the_orphan_reap() {
        let connection = schema_through_migration(12);
        seed_owned_and_orphaned_children(&connection);
        connection
            .execute_batch(
                "INSERT INTO runs(id, kind, started_at, status) \
                 VALUES (NULL, 'test', 'now', 'pass');",
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM artifacts \
                     WHERE run_id NOT IN (SELECT id FROM runs)",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "the NOT IN formulation must be shown to be disarmed by the NULL"
        );

        apply_migrations(&connection).unwrap();

        assert_eq!(surviving_run_ids(&connection, "artifacts"), vec!["live"]);
    }

    /// Owned rows are not collateral. The reap is scoped to rows whose parent
    /// is genuinely absent.
    #[test]
    fn the_orphan_reap_leaves_owned_rows_untouched() {
        let connection = schema_through_migration(12);
        seed_owned_and_orphaned_children(&connection);

        apply_migrations(&connection).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1,
            "the reap must not touch the runs table"
        );
        assert_eq!(
            connection
                .query_row("SELECT id FROM artifacts", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "artifact-live"
        );
    }

    fn seed_owned_and_orphaned_children(connection: &Connection) {
        connection
            .execute_batch(
                r#"
                INSERT INTO runs(id, kind, started_at, status)
                    VALUES ('live', 'test', 'now', 'pass');

                INSERT INTO artifacts(id, run_id, kind, path, created_at)
                    VALUES ('artifact-live', 'live', 'log', '/live', 'now'),
                           ('artifact-orphan', 'reaped', 'log', '/orphan', 'now');

                INSERT INTO findings(id, run_id, tool, message, created_at)
                    VALUES ('finding-live', 'live', 'clippy', 'kept', 'now'),
                           ('finding-orphan', 'reaped', 'clippy', 'orphaned', 'now');

                INSERT INTO triage_items(
                    id, run_id, provider, repo_owner, repo_name, item_type, number,
                    state, title, url, observed_at)
                    VALUES ('triage-live', 'live', 'github', 'o', 'r', 'issue', 1,
                            'open', 't', 'u', 'now'),
                           ('triage-orphan', 'reaped', 'github', 'o', 'r', 'issue', 2,
                            'open', 't', 'u', 'now');

                INSERT INTO trace_spans(id, run_id, span_id, status)
                    VALUES ('span-live', 'live', 's1', 'pass'),
                           ('span-orphan', 'reaped', 's2', 'pass');

                INSERT INTO trace_runs(run_id, component_id, scenario_id, status)
                    VALUES ('live', 'c', 'sc', 'pass'),
                           ('reaped', 'c', 'sc', 'pass');
                "#,
            )
            .unwrap();
    }

    fn surviving_run_ids(connection: &Connection, table: &str) -> Vec<String> {
        let mut statement = connection
            .prepare(&format!("SELECT run_id FROM {table} ORDER BY run_id"))
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    fn schema_through_migration_11() -> Connection {
        schema_through_migration(11)
    }

    fn schema_through_migration(max_version: i64) -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );
                "#,
            )
            .unwrap();

        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= max_version)
        {
            let tx = connection.transaction().unwrap();
            apply_migration_sql(&tx, migration).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'test')",
                [migration.version],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        connection
    }
}
