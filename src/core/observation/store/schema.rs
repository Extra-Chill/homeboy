use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::Connection;

use super::{sqlite_error, ObservationDbStatus};
use crate::core::{paths, Result};

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
        .map_err(|_| {
            crate::core::Error::internal_unexpected("observation migration lock poisoned")
        })?;

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
    Ok(connection)
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
