use std::fs;

use rusqlite::{params, params_from_iter, OptionalExtension, ToSql};
use uuid::Uuid;

use super::*;

/// Rows returned when a caller does not ask for a specific page size.
pub const DEFAULT_RUN_PAGE_LIMIT: i64 = 100;

/// Largest page a single run query will materialize. A request above this is
/// reduced to it — but the reduction is reported through `RunPage::truncated`
/// and `RunPage::applied_limit` rather than being silent (#11177).
pub const MAX_RUN_PAGE_LIMIT: i64 = 1000;

/// Ceiling on an exhaustive walk, above which
/// [`ObservationStore::list_runs_all`] errors instead of returning a partial
/// answer. A wrong answer is worse than a loud one.
pub const MAX_EXHAUSTIVE_RUN_ROWS: i64 = 100_000;

/// Turn a probe read of `limit + 1` rows into a page plus its resume position.
///
/// The extra row is the whole mechanism: it is the difference between "the
/// page ended because the data ended" and "the page ended because the limit
/// did", which a bare `Vec` cannot express.
fn run_page_from_probe(mut runs: Vec<RunRecord>, limit: i64, offset: i64) -> RunPage {
    let truncated = runs.len() as i64 > limit;
    if truncated {
        runs.truncate(limit.max(0) as usize);
    }
    let next_cursor = if truncated {
        runs.last().map(RunCursor::from_run)
    } else {
        None
    };
    let next_offset = if truncated {
        Some(offset + limit)
    } else {
        None
    };
    RunPage {
        runs,
        truncated,
        applied_limit: limit,
        next_cursor,
        next_offset,
    }
}

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
        let store = Self {
            connection,
            path,
            readonly: false,
        };
        store.reconcile_unfinished_artifact_publications()?;
        Ok(store)
    }

    /// Open an existing observation database without any initialization work.
    /// Metadata readers use this so they never contend for the global writer
    /// lock merely to inspect a persisted run.
    pub fn open_readonly() -> Result<Self> {
        Self::open_readonly_at(database_path()?)
    }

    pub fn open_readonly_at(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.exists() {
            // Preserve the normal "run not found" contract without creating a
            // database merely because a metadata reader was invoked first.
            let connection = rusqlite::Connection::open_in_memory()
                .map_err(sqlite_error("open empty read-only observation store"))?;
            connection
                .execute_batch(
                    "CREATE TABLE runs (id TEXT PRIMARY KEY, kind TEXT NOT NULL, component_id TEXT, started_at TEXT NOT NULL, finished_at TEXT, status TEXT NOT NULL, command TEXT, cwd TEXT, homeboy_version TEXT, git_sha TEXT, rig_id TEXT, metadata_json TEXT NOT NULL DEFAULT '{}');",
                )
                .map_err(sqlite_error("create empty read-only observation store"))?;
            return Ok(Self {
                connection,
                path,
                readonly: true,
            });
        }
        let connection = schema::open_readonly_connection(&path)?;
        Ok(Self {
            connection,
            path,
            readonly: true,
        })
    }

    pub fn status(&self) -> Result<ObservationDbStatus> {
        schema::status_for_open_connection(&self.connection, self.path.clone(), true)
    }

    pub fn start_run(&self, run: NewRunRecord) -> Result<RunRecord> {
        let context = run
            .run_context
            .clone()
            .with_missing_from(RunContext::subprocess_compatibility_from_env());
        self.start_run_with_context_and_id(run, context, None)
    }

    /// Start a run under a caller-reserved identifier for a lifecycle that must
    /// correlate durable records across subsystems.
    pub fn start_run_with_id(&self, run: NewRunRecord, id: String) -> Result<RunRecord> {
        let context = run
            .run_context
            .clone()
            .with_missing_from(RunContext::subprocess_compatibility_from_env());
        self.start_run_with_context_and_id(run, context, Some(id))
    }

    pub fn start_run_with_context(
        &self,
        run: NewRunRecord,
        context: RunContext,
    ) -> Result<RunRecord> {
        self.start_run_with_context_and_id(run, context, None)
    }

    fn start_run_with_context_and_id(
        &self,
        mut run: NewRunRecord,
        context: RunContext,
        requested_id: Option<String>,
    ) -> Result<RunRecord> {
        if let Some(route) = crate::notification_route::current() {
            route.insert_into_metadata(&mut run.metadata_json);
        }
        validate_required("kind", &run.kind)?;
        let id = requested_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        validate_required("id", &id)?;
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
            Some(mut metadata_json) => {
                // Terminal reconciliation often supplies fresh evidence metadata.
                // Keep the route that was durably bound before execution unless a
                // caller explicitly supplies a replacement route.
                if crate::notification_route::NotificationRoute::from_metadata(&metadata_json)
                    .is_none()
                {
                    if let Some(existing) = self.get_run(run_id)? {
                        if let Some(route) =
                            crate::notification_route::NotificationRoute::from_metadata(
                                &existing.metadata_json,
                            )
                        {
                            route.insert_into_metadata(&mut metadata_json);
                        }
                    }
                }
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

    /// Finish a run only while it is still active. This prevents concurrent
    /// lifecycle owners from replacing an already-recorded terminal outcome.
    pub fn finish_running_run(
        &self,
        run_id: &str,
        status: RunStatus,
        metadata_json: Option<serde_json::Value>,
    ) -> Result<Option<RunRecord>> {
        validate_required("run_id", run_id)?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let rows = match metadata_json {
            Some(mut metadata_json) => {
                if crate::notification_route::NotificationRoute::from_metadata(&metadata_json)
                    .is_none()
                {
                    if let Some(existing) = self.get_run(run_id)? {
                        if let Some(route) =
                            crate::notification_route::NotificationRoute::from_metadata(
                                &existing.metadata_json,
                            )
                        {
                            route.insert_into_metadata(&mut metadata_json);
                        }
                    }
                }
                let serialized = serialize_metadata(&metadata_json)?;
                execute_with_retry("finish running run record with metadata", || {
                    self.connection.execute(
                        r#"
                        UPDATE runs
                        SET finished_at = ?1, status = ?2, metadata_json = ?3
                        WHERE id = ?4 AND status = ?5
                        "#,
                        params![
                            finished_at,
                            status.as_str(),
                            serialized,
                            run_id,
                            RunStatus::Running.as_str(),
                        ],
                    )
                })?
            }
            None => execute_with_retry("finish running run record", || {
                self.connection.execute(
                    r#"
                    UPDATE runs
                    SET finished_at = ?1, status = ?2
                    WHERE id = ?3 AND status = ?4
                    "#,
                    params![
                        finished_at,
                        status.as_str(),
                        run_id,
                        RunStatus::Running.as_str(),
                    ],
                )
            })?,
        };

        if rows == 0 {
            return Ok(None);
        }

        self.get_run(run_id)
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
            .map_err(|error| self.read_error("read run record", error))
    }

    pub(crate) fn read_error(&self, operation: &'static str, error: rusqlite::Error) -> Error {
        if self.readonly && is_transient_lock_error(&error) {
            Error::observation_store_busy(self.path.to_string_lossy(), operation, 750)
        } else {
            sqlite_error(operation)(error)
        }
    }

    /// List runs, discarding any signal that the page was cut short.
    ///
    /// The returned `Vec` is indistinguishable from a complete answer even when
    /// more rows matched the filter, so this is a **display-path** accessor.
    /// Any caller whose logic treats "absent from this list" as "does not
    /// exist" must use [`ObservationStore::list_runs_page`] and honour
    /// `RunPage::truncated`, or [`ObservationStore::list_runs_all`] to walk
    /// every matching row. See #11177 (and #11116, the outage it caused).
    pub fn list_runs(&self, filter: RunListFilter) -> Result<Vec<RunRecord>> {
        Ok(self.list_runs_page(filter)?.runs)
    }

    /// List a page of runs together with an explicit truncation signal and the
    /// position to resume from.
    ///
    /// The applied page size is clamped into `[1, MAX_RUN_PAGE_LIMIT]`; a
    /// request above that ceiling is reduced, but the reduction is now
    /// observable through `RunPage::applied_limit` and `RunPage::truncated`
    /// instead of being silent (#11177). Truncation is detected by reading one
    /// row past the page, so a result that exactly fills the limit is not
    /// mistaken for a truncated one.
    pub fn list_runs_page(&self, filter: RunListFilter) -> Result<RunPage> {
        let limit = filter
            .limit
            .unwrap_or(DEFAULT_RUN_PAGE_LIMIT)
            .clamp(1, MAX_RUN_PAGE_LIMIT);
        let offset = filter.offset.unwrap_or(0).max(0);
        // Read one row past the page so "ended on the boundary" and "there is
        // more" are distinguishable rather than conflated.
        let probe = limit + 1;
        let mut predicates = Vec::new();
        let mut values: Vec<&dyn ToSql> = Vec::new();
        if let Some(kind) = filter.kind.as_ref() {
            predicates.push("kind = ?");
            values.push(kind);
        }
        if let Some(component_id) = filter.component_id.as_ref() {
            predicates.push("component_id = ?");
            values.push(component_id);
        }
        if let Some(status) = filter.status.as_ref() {
            predicates.push("status = ?");
            values.push(status);
        }
        if let Some(rig_id) = filter.rig_id.as_ref() {
            predicates.push("rig_id = ?");
            values.push(rig_id);
        }
        if let Some(cursor) = filter.after.as_ref() {
            // Keyset resume in the canonical `started_at DESC, id DESC` order.
            // Expanded rather than written as a row-value comparison so the
            // predicate does not depend on the linked SQLite version.
            predicates.push("(started_at < ? OR (started_at = ? AND id < ?))");
            values.push(&cursor.started_at);
            values.push(&cursor.started_at);
            values.push(&cursor.id);
        }
        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        values.push(&probe);
        values.push(&offset);
        let mut statement = self
            .connection
            .prepare(&format!(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                {where_clause}
                ORDER BY started_at DESC, id DESC
                LIMIT ? OFFSET ?
                "#,
            ))
            .map_err(sqlite_error("prepare list run records"))?;
        let rows = statement
            .query_map(params_from_iter(values), row_to_run_record)
            .map_err(sqlite_error("list run records"))?;

        let runs = collect_rows(rows, "collect run records")?;
        Ok(run_page_from_probe(runs, limit, offset))
    }

    /// Walk every run matching `filter`, paginating internally so the answer is
    /// complete rather than silently cut at the page ceiling.
    ///
    /// `filter.limit` is ignored — completeness is the point. Pagination uses a
    /// keyset cursor, so rows inserted mid-walk cannot shift the window and
    /// cause a row to be skipped. If the result would exceed
    /// `MAX_EXHAUSTIVE_RUN_ROWS` the call **fails loudly** instead of returning
    /// a quietly partial set (#11177).
    pub fn list_runs_all(&self, filter: RunListFilter) -> Result<Vec<RunRecord>> {
        let mut page_filter = RunListFilter {
            limit: Some(MAX_RUN_PAGE_LIMIT),
            offset: None,
            ..filter
        };
        let mut collected: Vec<RunRecord> = Vec::new();
        loop {
            let page = self.list_runs_page(page_filter.clone())?;
            collected.extend(page.runs);
            if !page.truncated {
                return Ok(collected);
            }
            if collected.len() as i64 >= MAX_EXHAUSTIVE_RUN_ROWS {
                return Err(Error::internal_unexpected(format!(
                    "run listing exceeded the {MAX_EXHAUSTIVE_RUN_ROWS} row exhaustive-walk ceiling; \
                     narrow the filter or paginate explicitly with list_runs_page"
                )));
            }
            // `truncated` implies a non-empty page, so a missing cursor would be
            // a store bug; stop rather than loop forever on it.
            let Some(cursor) = page.next_cursor else {
                return Ok(collected);
            };
            page_filter.after = Some(cursor);
        }
    }

    /// Return a bounded set of runs linked to the given retry predecessor.
    /// The literal JSON path matches the `idx_runs_metadata_retry_of` index.
    ///
    /// Truncation is invisible in the returned `Vec`. A caller reasoning about
    /// retry *lineage* — where a missing sibling is a wrong answer, not a slow
    /// one — must use [`ObservationStore::list_runs_by_retry_of_page`] (#11177).
    pub fn list_runs_by_retry_of(
        &self,
        kind: &str,
        retry_of: &str,
        limit: usize,
    ) -> Result<Vec<RunRecord>> {
        Ok(self.list_runs_by_retry_of_page(kind, retry_of, limit)?.runs)
    }

    /// Retry-lineage siblings with an explicit truncation signal.
    pub fn list_runs_by_retry_of_page(
        &self,
        kind: &str,
        retry_of: &str,
        limit: usize,
    ) -> Result<RunPage> {
        let limit = i64::try_from(limit.clamp(1, MAX_RUN_PAGE_LIMIT as usize))
            .expect("bounded run query limit");
        let probe = limit + 1;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                WHERE kind = ?1
                  AND json_extract(metadata_json, '$.agent_task_run.metadata.retry_of') = ?2
                ORDER BY started_at DESC, id DESC
                LIMIT ?3
                "#,
            )
            .map_err(sqlite_error("prepare metadata-indexed run query"))?;
        let rows = statement
            .query_map(params![kind, retry_of, probe], row_to_run_record)
            .map_err(sqlite_error("query metadata-indexed run records"))?;
        let runs = collect_rows(rows, "collect metadata-indexed run records")?;
        Ok(run_page_from_probe(runs, limit, 0))
    }

    /// List every currently running run so callers can retain active work when
    /// applying a separate display limit to recent history.
    pub fn list_active_runs(&self) -> Result<Vec<RunRecord>> {
        self.list_active_runs_bounded(i64::MAX)
    }

    /// List the newest active runs without scanning an unbounded stale corpus.
    pub fn list_active_runs_bounded(&self, limit: i64) -> Result<Vec<RunRecord>> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, kind, component_id, started_at, finished_at, status, command, cwd,
                       homeboy_version, git_sha, rig_id, metadata_json
                FROM runs
                WHERE status = ?1
                ORDER BY started_at DESC, id DESC
                LIMIT ?2
                "#,
            )
            .map_err(sqlite_error("prepare list active run records"))?;
        let rows = statement
            .query_map(
                params![RunStatus::Running.as_str(), limit.max(1)],
                row_to_run_record,
            )
            .map_err(sqlite_error("list active run records"))?;

        collect_rows(rows, "collect active run records")
    }

    pub fn latest_run(&self, mut filter: RunListFilter) -> Result<Option<RunRecord>> {
        filter.limit = Some(1);
        Ok(self.list_runs(filter)?.into_iter().next())
    }

    /// Return a bounded page of terminal rows older than `finished_before`.
    /// Unknown statuses are deliberately retained rather than guessed terminal.
    pub fn terminal_run_ids_before(
        &self,
        finished_before: &str,
        limit: i64,
    ) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM runs WHERE finished_at < ?1 AND status IN ('pass', 'fail', 'error', 'skipped', 'stale') ORDER BY finished_at ASC, rowid ASC LIMIT ?2",
        ).map_err(sqlite_error("prepare terminal run retention candidates"))?;
        let rows = statement
            .query_map(params![finished_before, limit.clamp(1, 1000)], |row| {
                row.get(0)
            })
            .map_err(sqlite_error("list terminal run retention candidates"))?;
        collect_rows(rows, "collect terminal run retention candidates")
    }

    /// Delete run-owned records explicitly so older databases that did not
    /// enable SQLite foreign keys retain referential integrity as well.
    pub fn delete_terminal_runs(&mut self, ids: &[String]) -> Result<()> {
        let tx = self
            .connection
            .transaction()
            .map_err(sqlite_error("begin terminal run retention"))?;
        for id in ids {
            for table in RUN_OWNED_CHILD_TABLES {
                tx.execute(&format!("DELETE FROM {table} WHERE run_id = ?1"), [id])
                    .map_err(sqlite_error(format!("delete terminal run {table}")))?;
            }
            tx.execute("DELETE FROM runs WHERE id = ?1 AND status IN ('pass', 'fail', 'error', 'skipped', 'stale')", [id])
                .map_err(sqlite_error("delete terminal run"))?;
        }
        tx.commit()
            .map_err(sqlite_error("commit terminal run retention"))
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

    /// Atomically mark a run's notification as delivered.
    ///
    /// Sets `notification_delivered` in the run's `metadata_json` **only if**
    /// it is not already present. Returns `Ok(true)` if the marker was newly
    /// set (this caller won the exactly-once race) or `Ok(false)` if the run
    /// was already marked (another caller dispatched first).
    ///
    /// This is the single coordination point that prevents duplicate
    /// notifications when both the runner-direct path and the controller
    /// backstop observe a terminal run.
    pub fn mark_notification_delivered(&self, run_id: &str, delivered_by: &str) -> Result<bool> {
        validate_required("run_id", run_id)?;
        let marker = serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "by": delivered_by,
        });
        let marker_str = serde_json::to_string(&marker).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("serialize notification marker".to_string()),
            )
        })?;
        let rows = execute_with_retry("mark notification delivered", || {
            self.connection.execute(
                r#"
                UPDATE runs
                SET metadata_json = json_set(
                    COALESCE(metadata_json, '{}'),
                    '$.notification_delivered',
                    json(?1)
                )
                WHERE id = ?2
                  AND json_extract(COALESCE(metadata_json, '{}'), '$.notification_delivered') IS NULL
                "#,
                params![marker_str, run_id],
            )
        })?;
        Ok(rows > 0)
    }

    /// Check whether a run's notification has already been delivered.
    pub fn is_notification_delivered(&self, run_id: &str) -> Result<bool> {
        validate_required("run_id", run_id)?;
        let result: bool = self
            .connection
            .query_row(
                r#"
                SELECT json_extract(COALESCE(metadata_json, '{}'), '$.notification_delivered') IS NOT NULL
                FROM runs
                WHERE id = ?1
                "#,
                [run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error("check notification delivered"))?
            .unwrap_or(false);
        Ok(result)
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

    /// Insert a synthetic controller run exactly once. The caller that wins the
    /// insert owns `publication_token`; concurrent readers must not inherit it.
    pub fn import_synthetic_run(&self, run: &RunRecord, publication_token: &str) -> Result<bool> {
        validate_required("run.id", &run.id)?;
        let token = run
            .metadata_json
            .pointer("/lab/synthetic_publication_token")
            .and_then(serde_json::Value::as_str);
        if token != Some(publication_token) {
            return Err(Error::internal_unexpected(
                "synthetic run publication token does not match its metadata",
            ));
        }
        let metadata_json = serialize_metadata(&run.metadata_json)?;
        execute_with_retry("import synthetic run record", || {
            self.connection.execute(
                r#"
                INSERT OR IGNORE INTO runs(
                    id, kind, component_id, started_at, finished_at, status,
                    command, cwd, homeboy_version, git_sha, rig_id, metadata_json
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
        })
        .map(|inserted| inserted == 1)
    }

    pub fn upsert_imported_run(&self, run: &RunRecord) -> Result<()> {
        self.upsert_imported_run_with_terminal_guard(run, false)
    }

    /// Upsert an imported projection without allowing a stale in-flight writer
    /// to replace a settled observation.
    pub fn upsert_imported_run_preserving_terminal(&self, run: &RunRecord) -> Result<()> {
        self.upsert_imported_run_with_terminal_guard(run, true)
    }

    fn upsert_imported_run_with_terminal_guard(
        &self,
        run: &RunRecord,
        preserve_terminal: bool,
    ) -> Result<()> {
        validate_required("run.id", &run.id)?;
        let mut run = run.clone();
        if crate::notification_route::NotificationRoute::from_metadata(&run.metadata_json).is_none()
        {
            if let Some(existing) = self.get_run(&run.id)? {
                if let Some(route) = crate::notification_route::NotificationRoute::from_metadata(
                    &existing.metadata_json,
                ) {
                    route.insert_into_metadata(&mut run.metadata_json);
                }
            }
        }
        let metadata_json = serialize_metadata(&run.metadata_json)?;
        let terminal_guard = if preserve_terminal {
            " WHERE runs.status = 'running' OR ?6 != 'running'"
        } else {
            ""
        };
        execute_with_retry("upsert imported run record", || {
            self.connection.execute(
                &format!(
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
                {terminal_guard}
                "#
                ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    /// Seed rows with explicit ids and timestamps so the canonical
    /// `started_at DESC, id DESC` order is deterministic rather than dependent
    /// on how fast the inserts happened to run. Returns the ids in the order
    /// queries hand them back: newest first.
    fn seed_runs(store: &ObservationStore, kind: &str, count: usize) -> Vec<String> {
        let mut ids: Vec<String> = (0..count)
            .map(|index| {
                let id = format!("{kind}-{index:03}");
                seed_run(
                    store,
                    kind,
                    &id,
                    &format!("2026-01-01T00:{index:02}:00+00:00"),
                    None,
                );
                id
            })
            .collect();
        ids.reverse();
        ids
    }

    fn seed_run(
        store: &ObservationStore,
        kind: &str,
        id: &str,
        started_at: &str,
        retry_of: Option<&str>,
    ) {
        let metadata_json = match retry_of {
            Some(retry_of) => serde_json::json!({
                "agent_task_run": { "metadata": { "retry_of": retry_of } }
            }),
            None => serde_json::json!({}),
        };
        store
            .import_run(&RunRecord {
                id: id.to_string(),
                kind: kind.to_string(),
                started_at: started_at.to_string(),
                status: RunStatus::Running.as_str().to_string(),
                metadata_json,
                ..RunRecord::default()
            })
            .expect("seed run");
    }

    fn ids(runs: &[RunRecord]) -> Vec<String> {
        runs.iter().map(|run| run.id.clone()).collect()
    }

    /// The whole defect is that a cut-short page looked exactly like a complete
    /// one. A page that merely ends on its limit boundary must not claim
    /// truncation, or the signal is noise and callers learn to ignore it.
    #[test]
    fn a_page_that_ends_because_the_data_ended_is_not_truncated() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            seed_runs(&store, "page-exact", 3);

            let page = store
                .list_runs_page(RunListFilter {
                    kind: Some("page-exact".to_string()),
                    limit: Some(3),
                    ..RunListFilter::default()
                })
                .expect("page");

            assert_eq!(page.runs.len(), 3);
            assert!(!page.truncated, "an exact fit is a complete answer");
            assert_eq!(page.next_cursor, None);
            assert_eq!(page.next_offset, None);
        });
    }

    /// And a page that ended because the limit did must say so, and must say
    /// where to resume — otherwise the caller can detect the boundary but not
    /// cross it.
    #[test]
    fn a_page_cut_short_by_its_limit_reports_truncation_and_a_resume_point() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            let seeded = seed_runs(&store, "page-cut", 5);

            let page = store
                .list_runs_page(RunListFilter {
                    kind: Some("page-cut".to_string()),
                    limit: Some(2),
                    ..RunListFilter::default()
                })
                .expect("page");

            assert_eq!(ids(&page.runs), seeded[..2].to_vec());
            assert!(page.truncated);
            assert_eq!(page.applied_limit, 2);
            assert_eq!(page.next_offset, Some(2));
            assert_eq!(
                page.next_cursor,
                page.runs.last().map(RunCursor::from_run),
                "the resume point is the last row handed out"
            );
        });
    }

    /// A request above the page ceiling is still reduced — but the reduction is
    /// now reported instead of silent, which is the #11177 contract.
    #[test]
    fn a_request_above_the_page_ceiling_reports_the_limit_it_actually_applied() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            seed_runs(&store, "page-ceiling", 2);

            let page = store
                .list_runs_page(RunListFilter {
                    kind: Some("page-ceiling".to_string()),
                    limit: Some(5_000),
                    ..RunListFilter::default()
                })
                .expect("page");

            assert_eq!(page.applied_limit, MAX_RUN_PAGE_LIMIT);
            assert!(!page.truncated);
            assert_eq!(page.runs.len(), 2);
        });
    }

    /// Offset pagination has to actually reach past the first page, or the
    /// truncation signal is a dead end.
    #[test]
    fn offset_pagination_reaches_the_rows_past_the_first_page() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            let seeded = seed_runs(&store, "page-offset", 5);
            let filter = |offset: Option<i64>| RunListFilter {
                kind: Some("page-offset".to_string()),
                limit: Some(2),
                offset,
                ..RunListFilter::default()
            };

            let first = store.list_runs_page(filter(None)).expect("first page");
            let second = store
                .list_runs_page(filter(first.next_offset))
                .expect("second page");
            let third = store
                .list_runs_page(filter(second.next_offset))
                .expect("third page");

            let walked: Vec<String> = ids(&first.runs)
                .into_iter()
                .chain(ids(&second.runs))
                .chain(ids(&third.runs))
                .collect();

            assert_eq!(walked, seeded, "an offset walk must visit every row once");
            assert!(!third.truncated, "the walk must terminate");
        });
    }

    /// Cursor pagination is the one that stays correct while rows are being
    /// inserted, so it must cover the same ground as the offset walk.
    #[test]
    fn cursor_pagination_visits_every_row_exactly_once() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            let seeded = seed_runs(&store, "page-cursor", 5);

            let mut walked = Vec::new();
            let mut after = None;
            loop {
                let page = store
                    .list_runs_page(RunListFilter {
                        kind: Some("page-cursor".to_string()),
                        limit: Some(2),
                        after: after.clone(),
                        ..RunListFilter::default()
                    })
                    .expect("cursor page");
                walked.extend(ids(&page.runs));
                if !page.truncated {
                    break;
                }
                after = page.next_cursor;
            }

            assert_eq!(walked, seeded);
        });
    }

    /// A row inserted mid-walk shifts every offset. That is the case an offset
    /// walk gets wrong and a keyset cursor walk does not.
    #[test]
    fn a_cursor_walk_survives_an_insert_that_would_shift_an_offset_walk() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            let seeded = seed_runs(&store, "page-shift", 4);
            let filter = |after: Option<RunCursor>, offset: Option<i64>| RunListFilter {
                kind: Some("page-shift".to_string()),
                limit: Some(2),
                offset,
                after,
                ..RunListFilter::default()
            };

            let first = store
                .list_runs_page(filter(None, None))
                .expect("first page");
            // A newer row sorts ahead of everything the first page returned.
            seed_run(
                &store,
                "page-shift",
                "page-shift-newer",
                "2026-01-01T23:00:00+00:00",
                None,
            );

            let by_cursor = store
                .list_runs_page(filter(first.next_cursor.clone(), None))
                .expect("cursor page");
            let by_offset = store
                .list_runs_page(filter(None, first.next_offset))
                .expect("offset page");

            let cursor_walk: Vec<String> = ids(&first.runs)
                .into_iter()
                .chain(ids(&by_cursor.runs))
                .collect();
            assert_eq!(
                cursor_walk, seeded,
                "a cursor walk must not skip a row because a newer one arrived"
            );
            assert_ne!(
                ids(&by_offset.runs),
                ids(&by_cursor.runs),
                "the offset walk is expected to be the one that shifts; if it stops \
                 shifting this test no longer proves anything"
            );
        });
    }

    /// The exhaustive accessor exists so a correctness-sensitive caller can ask
    /// for completeness explicitly; a display limit must not silently cap it.
    #[test]
    fn the_exhaustive_walk_ignores_the_display_limit_and_returns_every_row() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            let seeded = seed_runs(&store, "page-all", 7);
            seed_runs(&store, "page-all-other", 3);

            let runs = store
                .list_runs_all(RunListFilter {
                    kind: Some("page-all".to_string()),
                    limit: Some(2),
                    ..RunListFilter::default()
                })
                .expect("exhaustive walk");

            assert_eq!(
                ids(&runs),
                seeded,
                "the filter still applies; the limit does not"
            );
        });
    }

    /// The bounded accessor keeps its historical shape so the existing call
    /// sites are unaffected: same rows, signal dropped.
    #[test]
    fn the_bounded_accessor_returns_the_same_rows_as_the_page() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            seed_runs(&store, "page-parity", 4);
            let filter = RunListFilter {
                kind: Some("page-parity".to_string()),
                limit: Some(3),
                ..RunListFilter::default()
            };

            let rows = store.list_runs(filter.clone()).expect("rows");
            let page = store.list_runs_page(filter).expect("page");

            assert_eq!(rows, page.runs);
            assert_eq!(rows.len(), 3);
            assert!(page.truncated);
        });
    }

    /// The default page size is unchanged for callers that ask for nothing.
    #[test]
    fn an_unspecified_limit_still_applies_the_historical_default() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            seed_runs(&store, "page-default", 2);

            let page = store
                .list_runs_page(RunListFilter {
                    kind: Some("page-default".to_string()),
                    ..RunListFilter::default()
                })
                .expect("page");

            assert_eq!(page.applied_limit, DEFAULT_RUN_PAGE_LIMIT);
        });
    }

    /// Retry lineage carried the identical unsignalled clamp. A truncated
    /// lineage is a wrong answer, not a slow one.
    #[test]
    fn a_truncated_retry_lineage_reports_truncation() {
        with_isolated_home(|_home| {
            let store = ObservationStore::open_initialized().expect("store");
            for index in 0..3 {
                seed_run(
                    &store,
                    "agent-task",
                    &format!("retry-{index:03}"),
                    &format!("2026-01-01T00:{index:02}:00+00:00"),
                    Some("source-run"),
                );
            }

            let cut = store
                .list_runs_by_retry_of_page("agent-task", "source-run", 2)
                .expect("lineage page");
            assert_eq!(cut.runs.len(), 2);
            assert!(cut.truncated);
            assert_eq!(cut.applied_limit, 2);

            let complete = store
                .list_runs_by_retry_of_page("agent-task", "source-run", 8)
                .expect("lineage page");
            assert_eq!(complete.runs.len(), 3);
            assert!(!complete.truncated);
            assert_eq!(
                store
                    .list_runs_by_retry_of("agent-task", "source-run", 8)
                    .expect("lineage rows"),
                complete.runs,
                "the bounded accessor keeps returning the same rows"
            );
        });
    }
}
