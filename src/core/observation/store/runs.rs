use std::fs;

use rusqlite::params;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::*;

impl ObservationStore {
    /// Open and lazily initialize the local observed-state database.
    pub fn open_initialized() -> Result<Self> {
        Self::open_initialized_at(database_path()?)
    }

    pub fn open_initialized_at(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("create observation store dir {}", parent.display())),
                )
            })?;
        }

        let connection = schema::open_connection(&path)?;
        schema::apply_migrations(&connection)?;
        Ok(Self { connection, path })
    }

    pub fn status(&self) -> Result<ObservationDbStatus> {
        schema::status_for_open_connection(&self.connection, self.path.clone(), true)
    }

    pub fn start_run(&self, run: NewRunRecord) -> Result<RunRecord> {
        let context = run
            .run_context
            .clone()
            .with_missing_from(RunContext::subprocess_compatibility_from_env());
        self.start_run_with_context(run, context)
    }

    pub fn start_run_with_context(
        &self,
        run: NewRunRecord,
        context: RunContext,
    ) -> Result<RunRecord> {
        validate_required("kind", &run.kind)?;
        let id = Uuid::new_v4().to_string();
        let started_at = chrono::Utc::now().to_rfc3339();
        let metadata_json =
            serialize_metadata(&with_run_context_metadata(run.metadata_json, &context))?;

        execute_with_retry("insert run record", || {
            self.connection.execute(
                r#"
                INSERT INTO runs(
                    id,
                    kind,
                    component_id,
                    started_at,
                    status,
                    command,
                    cwd,
                    homeboy_version,
                    git_sha,
                    rig_id,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    id,
                    run.kind,
                    run.component_id,
                    started_at,
                    RunStatus::Running.as_str(),
                    run.command,
                    run.cwd,
                    run.homeboy_version,
                    run.git_sha,
                    run.rig_id,
                    metadata_json,
                ],
            )
        })?;

        self.get_run(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted run record {id} but could not read it back"
            ))
        })
    }

    pub fn finish_run(
        &self,
        run_id: &str,
        status: RunStatus,
        metadata_json: Option<serde_json::Value>,
    ) -> Result<RunRecord> {
        validate_required("run_id", run_id)?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let rows = match metadata_json {
            Some(metadata_json) => {
                let serialized = serialize_metadata(&metadata_json)?;
                execute_with_retry("finish run record with metadata", || {
                    self.connection.execute(
                        r#"
                        UPDATE runs
                        SET finished_at = ?1, status = ?2, metadata_json = ?3
                        WHERE id = ?4
                        "#,
                        params![finished_at, status.as_str(), serialized, run_id],
                    )
                })?
            }
            None => execute_with_retry("finish run record", || {
                self.connection.execute(
                    r#"
                    UPDATE runs
                    SET finished_at = ?1, status = ?2
                    WHERE id = ?3
                    "#,
                    params![finished_at, status.as_str(), run_id],
                )
            })?,
        };

        if rows == 0 {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            ));
        }

        self.get_run(run_id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Finished run record {run_id} but could not read it back"
            ))
        })
    }

    pub fn update_run_metadata(
        &self,
        run_id: &str,
        metadata_json: serde_json::Value,
    ) -> Result<RunRecord> {
        validate_required("run_id", run_id)?;
        let serialized = serialize_metadata(&metadata_json)?;
        let rows = execute_with_retry("update run metadata", || {
            self.connection.execute(
                r#"
                UPDATE runs
                SET metadata_json = ?1
                WHERE id = ?2
                "#,
                params![serialized, run_id],
            )
        })?;

        if rows == 0 {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            ));
        }

        self.get_run(run_id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Updated run record {run_id} but could not read it back"
            ))
        })
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<RunRecord>> {
        validate_required("run_id", run_id)?;
        self.connection
            .query_row(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                WHERE id = ?1
                "#,
                [run_id],
                row_to_run_record,
            )
            .optional()
            .map_err(sqlite_error("read run record"))
    }

    pub fn list_runs(&self, filter: RunListFilter) -> Result<Vec<RunRecord>> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1000);
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                WHERE (?1 IS NULL OR kind = ?1)
                  AND (?2 IS NULL OR component_id = ?2)
                  AND (?3 IS NULL OR status = ?3)
                  AND (?4 IS NULL OR rig_id = ?4)
                ORDER BY started_at DESC, rowid DESC
                LIMIT ?5
                "#,
            )
            .map_err(sqlite_error("prepare list run records"))?;
        let rows = statement
            .query_map(
                params![
                    filter.kind.as_deref(),
                    filter.component_id.as_deref(),
                    filter.status.as_deref(),
                    filter.rig_id.as_deref(),
                    limit,
                ],
                row_to_run_record,
            )
            .map_err(sqlite_error("list run records"))?;

        collect_rows(rows, "collect run records")
    }

    pub fn latest_run(&self, mut filter: RunListFilter) -> Result<Option<RunRecord>> {
        filter.limit = Some(1);
        Ok(self.list_runs(filter)?.into_iter().next())
    }

    pub fn list_runs_started_since(&self, started_at: &str) -> Result<Vec<RunRecord>> {
        validate_required("started_at", started_at)?;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                WHERE started_at >= ?1
                ORDER BY started_at DESC
                "#,
            )
            .map_err(sqlite_error("prepare list recent run records"))?;
        let rows = statement
            .query_map([started_at], row_to_run_record)
            .map_err(sqlite_error("list recent run records"))?;

        collect_rows(rows, "collect recent run records")
    }

    pub fn import_run(&self, run: &RunRecord) -> Result<()> {
        validate_required("run.id", &run.id)?;
        let metadata_json = serialize_metadata(&run.metadata_json)?;
        let inserted = execute_with_retry("import run record", || {
            self.connection.execute(
                r#"
                INSERT OR IGNORE INTO runs(
                    id,
                    kind,
                    component_id,
                    started_at,
                    finished_at,
                    status,
                    command,
                    cwd,
                    homeboy_version,
                    git_sha,
                    rig_id,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    run.id,
                    run.kind,
                    run.component_id,
                    run.started_at,
                    run.finished_at,
                    run.status,
                    run.command,
                    run.cwd,
                    run.homeboy_version,
                    run.git_sha,
                    run.rig_id,
                    metadata_json,
                ],
            )
        })?;
        if inserted == 0 {
            let existing = self.get_run(&run.id)?.ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "run import for {} was ignored but no existing record was found",
                    run.id
                ))
            })?;
            ensure_identical("run", &run.id, &existing, run)?;
        }
        Ok(())
    }

    pub fn upsert_imported_run(&self, run: &RunRecord) -> Result<()> {
        validate_required("run.id", &run.id)?;
        let metadata_json = serialize_metadata(&run.metadata_json)?;
        execute_with_retry("upsert imported run record", || {
            self.connection.execute(
                r#"
                INSERT INTO runs(
                    id,
                    kind,
                    component_id,
                    started_at,
                    finished_at,
                    status,
                    command,
                    cwd,
                    homeboy_version,
                    git_sha,
                    rig_id,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    component_id = excluded.component_id,
                    started_at = excluded.started_at,
                    finished_at = excluded.finished_at,
                    status = excluded.status,
                    command = excluded.command,
                    cwd = excluded.cwd,
                    homeboy_version = excluded.homeboy_version,
                    git_sha = excluded.git_sha,
                    rig_id = excluded.rig_id,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    run.id,
                    run.kind,
                    run.component_id,
                    run.started_at,
                    run.finished_at,
                    run.status,
                    run.command,
                    run.cwd,
                    run.homeboy_version,
                    run.git_sha,
                    run.rig_id,
                    metadata_json,
                ],
            )
        })?;
        Ok(())
    }
}
