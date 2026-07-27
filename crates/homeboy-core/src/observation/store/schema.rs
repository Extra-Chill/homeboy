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
        sql: r#"
        ALTER TABLE artifact_publication_intents ADD COLUMN owner_token TEXT;
        ALTER TABLE artifact_publication_intents ADD COLUMN lease_expires_at_ms INTEGER;
        CREATE INDEX IF NOT EXISTS idx_artifact_publication_intents_lease
            ON artifact_publication_intents(lease_expires_at_ms);
        "#,
    },
];

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
    // pragma_update may itself momentarily contend on the lock, so it is best
    // effort: the busy_timeout above plus the write-retry wrapper still apply.
    let _ = connection.pragma_update(None, "journal_mode", "WAL");
    let _ = connection.pragma_update(None, "synchronous", "NORMAL");
    Ok(connection)
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
