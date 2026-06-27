use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::*;

impl ObservationStore {
    pub fn import_artifact(&self, artifact: &ArtifactRecord) -> Result<()> {
        validate_required("artifact.id", &artifact.id)?;
        if self.get_run(&artifact.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "artifact.run_id",
                format!("referenced run record not found: {}", artifact.run_id),
                Some(artifact.run_id.clone()),
                None,
            ));
        }
        if let Some(existing) = self.get_artifact(&artifact.id)? {
            return ensure_identical("artifact", &artifact.id, &existing, artifact);
        }
        let metadata_json = serialize_metadata(&artifact.metadata_json)?;
        execute_with_retry("import artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    artifact.id,
                    artifact.run_id,
                    artifact.kind,
                    artifact.artifact_type,
                    artifact.path,
                    artifact.sha256,
                    artifact.size_bytes,
                    artifact.mime,
                    metadata_json,
                    artifact.created_at,
                ],
            )
        })?;
        Ok(())
    }

    pub fn record_artifact(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
    ) -> Result<ArtifactRecord> {
        self.record_artifact_with_metadata(run_id, kind, path, serde_json::json!({}))
    }

    pub fn record_artifact_with_metadata(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        validate_required("run_id", run_id)?;
        validate_required("kind", kind)?;
        if self.get_run(run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            ));
        }

        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Error::validation_invalid_argument(
                    "path",
                    format!("artifact file not found: {}", path.display()),
                    Some(path.to_string_lossy().to_string()),
                    None,
                );
            }
            Error::internal_io(
                e.to_string(),
                Some(format!("read artifact metadata {}", path.display())),
            )
        })?;
        if !metadata.is_file() {
            return Err(Error::validation_invalid_argument(
                "path",
                format!("artifact path is not a file: {}", path.display()),
                Some(path.to_string_lossy().to_string()),
                None,
            ));
        }

        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let size_bytes = i64::try_from(metadata.len()).ok();
        let sha256 = Some(crate::core::artifact_metadata::sha256_file(path)?);
        let mime = crate::core::artifact_metadata::content_type_from_path(path);
        let stored_path = persisted_artifact_path(run_id, &id, path)?;
        copy_artifact_file(path, &stored_path)?;
        let path_string = stored_path.to_string_lossy().to_string();
        let mut artifact = ArtifactRecord {
            id: id.clone(),
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            artifact_type: "file".to_string(),
            path: path_string.clone(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: sha256.clone(),
            size_bytes,
            mime: mime.clone(),
            metadata_json,
            created_at: created_at.clone(),
        };
        crate::core::artifact_links::annotate_public_artifact_url_validation(&mut artifact);

        let metadata_json_str = serialize_metadata(&artifact.metadata_json)?;
        execute_with_retry("insert artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "file",
                    path_string,
                    sha256,
                    size_bytes,
                    mime,
                    metadata_json_str,
                    created_at,
                ],
            )
        })?;

        let artifact = self.get_artifact(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted artifact record {id} but could not read it back"
            ))
        })?;
        crate::core::publication_artifacts::index_published_artifact_refs(
            self,
            &artifact,
            Some(path),
        )?;
        Ok(artifact)
    }

    pub fn record_directory_artifact(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
    ) -> Result<ArtifactRecord> {
        self.record_directory_artifact_with_metadata(run_id, kind, path, serde_json::json!({}))
    }

    pub fn record_directory_artifact_with_metadata(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        validate_required("run_id", run_id)?;
        validate_required("kind", kind)?;
        if self.get_run(run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            ));
        }

        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Error::validation_invalid_argument(
                    "path",
                    format!("artifact directory not found: {}", path.display()),
                    Some(path.to_string_lossy().to_string()),
                    None,
                );
            }
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact directory metadata {}",
                    path.display()
                )),
            )
        })?;
        if !metadata.is_dir() {
            return Err(Error::validation_invalid_argument(
                "path",
                format!("artifact path is not a directory: {}", path.display()),
                Some(path.to_string_lossy().to_string()),
                None,
            ));
        }

        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let stored_path = persisted_artifact_path(run_id, &id, path)?;
        copy_artifact_directory(path, &stored_path)?;
        let path_string = stored_path.to_string_lossy().to_string();

        let metadata_json_str = serialize_metadata(&metadata_json)?;
        execute_with_retry("insert directory artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "directory",
                    path_string,
                    Option::<String>::None,
                    Option::<i64>::None,
                    Option::<String>::None,
                    metadata_json_str,
                    created_at,
                ],
            )
        })?;

        self.get_artifact(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted directory artifact record {id} but could not read it back"
            ))
        })
    }

    pub fn record_url_artifact(
        &self,
        run_id: &str,
        kind: &str,
        url: &str,
    ) -> Result<ArtifactRecord> {
        self.record_url_artifact_with_metadata(run_id, kind, url, serde_json::json!({}))
    }

    pub fn record_url_artifact_with_metadata(
        &self,
        run_id: &str,
        kind: &str,
        url: &str,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        validate_required("run_id", run_id)?;
        validate_required("kind", kind)?;
        validate_required("url", url)?;
        if self.get_run(run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            ));
        }

        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        let metadata_json_str = serialize_metadata(&metadata_json)?;
        execute_with_retry("insert URL artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "url",
                    url,
                    Option::<String>::None,
                    Option::<i64>::None,
                    Option::<String>::None,
                    metadata_json_str,
                    created_at,
                ],
            )
        })?;

        self.get_artifact(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted artifact record {id} but could not read it back"
            ))
        })
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<ArtifactRecord>> {
        validate_required("run_id", run_id)?;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at
                FROM artifacts
                WHERE run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(sqlite_error("prepare list artifact records"))?;
        let rows = statement
            .query_map([run_id], row_to_artifact_record)
            .map_err(sqlite_error("list artifact records"))?;

        collect_rows(rows, "collect artifact records")
    }

    pub fn list_artifacts_for_runs(
        &self,
        run_ids: &[String],
    ) -> Result<BTreeMap<String, Vec<ArtifactRecord>>> {
        let mut artifacts_by_run = run_ids
            .iter()
            .map(|run_id| {
                validate_required("run_id", run_id)?;
                Ok((run_id.clone(), Vec::new()))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        if run_ids.is_empty() {
            return Ok(artifacts_by_run);
        }

        for chunk in run_ids.chunks(900) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                r#"
                SELECT id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at
                FROM artifacts
                WHERE run_id IN ({placeholders})
                ORDER BY run_id ASC, created_at ASC
                "#
            );
            let mut statement = self
                .connection
                .prepare(&sql)
                .map_err(sqlite_error("prepare batch list artifact records"))?;
            let rows = statement
                .query_map(
                    rusqlite::params_from_iter(chunk.iter().map(String::as_str)),
                    row_to_artifact_record,
                )
                .map_err(sqlite_error("batch list artifact records"))?;

            for row in rows {
                let artifact = row.map_err(sqlite_error("collect batch artifact records"))?;
                artifacts_by_run
                    .entry(artifact.run_id.clone())
                    .or_default()
                    .push(artifact);
            }
        }

        Ok(artifacts_by_run)
    }

    pub fn list_run_artifacts(
        &self,
        filter: RunListFilter,
        started_since: Option<&str>,
    ) -> Result<Vec<RunArtifactRecord>> {
        if let Some(started_since) = started_since {
            validate_required("started_since", started_since)?;
            let mut statement = self
                .connection
                .prepare(
                    r#"
                    SELECT r.id, r.kind, r.component_id, r.started_at, r.finished_at,
                           r.status, r.command, r.cwd, r.homeboy_version, r.git_sha,
                           r.rig_id, r.metadata_json,
                           a.id, a.run_id, a.kind, a.artifact_type, a.path, a.sha256,
                           a.size_bytes, a.mime, a.metadata_json, a.created_at
                    FROM runs r
                    INNER JOIN artifacts a ON a.run_id = r.id
                    WHERE r.started_at >= ?1
                      AND (?2 IS NULL OR r.kind = ?2)
                      AND (?3 IS NULL OR r.component_id = ?3)
                      AND (?4 IS NULL OR r.status = ?4)
                      AND (?5 IS NULL OR r.rig_id = ?5)
                    ORDER BY r.started_at DESC, a.created_at ASC
                    "#,
                )
                .map_err(sqlite_error("prepare joined run artifact records"))?;
            return query_run_artifact_records(
                &mut statement,
                params![
                    started_since,
                    filter.kind.as_deref(),
                    filter.component_id.as_deref(),
                    filter.status.as_deref(),
                    filter.rig_id.as_deref(),
                ],
            );
        }

        let limit = filter.limit.unwrap_or(100).clamp(1, 1000);
        let mut statement = self
            .connection
            .prepare(
                r#"
                WITH selected_runs AS (
                    SELECT rowid AS run_rowid, id, kind, component_id, started_at, finished_at,
                           status, command, cwd, homeboy_version, git_sha, rig_id, metadata_json
                    FROM runs
                    WHERE (?1 IS NULL OR kind = ?1)
                      AND (?2 IS NULL OR component_id = ?2)
                      AND (?3 IS NULL OR status = ?3)
                      AND (?4 IS NULL OR rig_id = ?4)
                    ORDER BY started_at DESC, rowid DESC
                    LIMIT ?5
                )
                SELECT r.id, r.kind, r.component_id, r.started_at, r.finished_at,
                       r.status, r.command, r.cwd, r.homeboy_version, r.git_sha,
                       r.rig_id, r.metadata_json,
                       a.id, a.run_id, a.kind, a.artifact_type, a.path, a.sha256,
                       a.size_bytes, a.mime, a.metadata_json, a.created_at
                FROM selected_runs r
                INNER JOIN artifacts a ON a.run_id = r.id
                ORDER BY r.started_at DESC, r.run_rowid DESC, a.created_at ASC
                "#,
            )
            .map_err(sqlite_error("prepare joined run artifact records"))?;
        query_run_artifact_records(
            &mut statement,
            params![
                filter.kind.as_deref(),
                filter.component_id.as_deref(),
                filter.status.as_deref(),
                filter.rig_id.as_deref(),
                limit,
            ],
        )
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Result<Option<ArtifactRecord>> {
        validate_required("artifact_id", artifact_id)?;
        self.connection
            .query_row(
                r#"
                SELECT id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at
                FROM artifacts
                WHERE id = ?1
                "#,
                [artifact_id],
                row_to_artifact_record,
            )
            .optional()
            .map_err(sqlite_error("read artifact record"))
    }

    pub fn get_artifact_for_run_token(
        &self,
        run_id: &str,
        artifact_token: &str,
    ) -> Result<Option<ArtifactRecord>> {
        validate_required("run_id", run_id)?;
        validate_required("artifact_token", artifact_token)?;
        self.connection
            .query_row(
                r#"
                SELECT id, run_id, kind, artifact_type, path, sha256, size_bytes, mime, metadata_json, created_at
                FROM artifacts
                WHERE run_id = ?1
                  AND (
                    id = ?2
                    OR kind = ?2
                    OR json_extract(metadata_json, '$.name') = ?2
                    OR json_extract(metadata_json, '$.original_manifest_id') = ?2
                  )
                ORDER BY created_at ASC
                LIMIT 1
                "#,
                params![run_id, artifact_token],
                row_to_artifact_record,
            )
            .optional()
            .map_err(sqlite_error("read artifact record for run token"))
    }

    pub fn list_artifact_cleanup_candidates(
        &self,
        filter: ArtifactCleanupFilter,
    ) -> Result<Vec<ArtifactCleanupCandidateRecord>> {
        let limit = filter.limit.unwrap_or(1000).clamp(1, 10_000);
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT a.id, a.run_id, a.kind, a.artifact_type, a.path, a.sha256,
                       a.size_bytes, a.mime, a.metadata_json, a.created_at,
                       r.kind, r.component_id, r.started_at, r.status
                FROM artifacts a
                INNER JOIN runs r ON r.id = a.run_id
                WHERE (?1 IS NULL OR a.created_at < ?1)
                  AND (?2 IS NULL OR a.run_id = ?2)
                  AND (?3 IS NULL OR a.kind = ?3)
                  AND (?4 IS NULL OR a.artifact_type = ?4)
                  AND (?5 IS NULL OR r.kind = ?5)
                  AND (?6 IS NULL OR r.component_id = ?6)
                ORDER BY a.created_at ASC, a.id ASC
                LIMIT ?7
                "#,
            )
            .map_err(sqlite_error("prepare artifact cleanup candidates"))?;
        let rows = statement
            .query_map(
                params![
                    filter.created_before.as_deref(),
                    filter.run_id.as_deref(),
                    filter.kind.as_deref(),
                    filter.artifact_type.as_deref(),
                    filter.run_kind.as_deref(),
                    filter.component_id.as_deref(),
                    limit,
                ],
                row_to_artifact_cleanup_candidate,
            )
            .map_err(sqlite_error("list artifact cleanup candidates"))?;

        collect_rows(rows, "collect artifact cleanup candidates")
    }

    pub fn delete_artifact_record(&self, artifact_id: &str) -> Result<bool> {
        validate_required("artifact_id", artifact_id)?;
        let rows = execute_with_retry("delete artifact record", || {
            self.connection
                .execute("DELETE FROM artifacts WHERE id = ?1", [artifact_id])
        })?;
        Ok(rows > 0)
    }

    pub fn record_trace_run(&self, record: NewTraceRunRecord) -> Result<TraceRunRecord> {
        let run_id = record.run_id.clone();
        validate_required("run_id", &record.run_id)?;
        validate_required("component_id", &record.component_id)?;
        validate_required("scenario_id", &record.scenario_id)?;
        validate_required("status", &record.status)?;
        if self.get_run(&record.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {}", record.run_id),
                Some(record.run_id),
                None,
            ));
        }
        let metadata_json = serialize_metadata(&record.metadata_json)?;

        execute_with_retry("insert trace run record", || {
            self.connection.execute(
                r#"
                INSERT INTO trace_runs(
                    run_id,
                    component_id,
                    rig_id,
                    scenario_id,
                    status,
                    baseline_status,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(run_id) DO UPDATE SET
                    component_id = excluded.component_id,
                    rig_id = excluded.rig_id,
                    scenario_id = excluded.scenario_id,
                    status = excluded.status,
                    baseline_status = excluded.baseline_status,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    record.run_id,
                    record.component_id,
                    record.rig_id,
                    record.scenario_id,
                    record.status,
                    record.baseline_status,
                    metadata_json,
                ],
            )
        })?;

        self.get_trace_run(&run_id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted trace run record {} but could not read it back",
                run_id
            ))
        })
    }

    pub fn get_trace_run(&self, run_id: &str) -> Result<Option<TraceRunRecord>> {
        validate_required("run_id", run_id)?;
        self.connection
            .query_row(
                r#"
                SELECT run_id, component_id, rig_id, scenario_id, status, baseline_status,
                       metadata_json
                FROM trace_runs
                WHERE run_id = ?1
                "#,
                [run_id],
                row_to_trace_run_record,
            )
            .optional()
            .map_err(sqlite_error("read trace run record"))
    }

    pub fn record_trace_span(&self, record: NewTraceSpanRecord) -> Result<TraceSpanRecord> {
        let run_id = record.run_id.clone();
        validate_required("run_id", &record.run_id)?;
        validate_required("span_id", &record.span_id)?;
        validate_required("status", &record.status)?;
        if self.get_run(&record.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                format!("run record not found: {}", record.run_id),
                Some(record.run_id),
                None,
            ));
        }
        let id = Uuid::new_v4().to_string();
        let metadata_json = serialize_metadata(&record.metadata_json)?;

        execute_with_retry("insert trace span record", || {
            self.connection.execute(
                r#"
                INSERT INTO trace_spans(
                    id,
                    run_id,
                    span_id,
                    status,
                    duration_ms,
                    from_event,
                    to_event,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    id,
                    record.run_id,
                    record.span_id,
                    record.status,
                    record.duration_ms,
                    record.from_event,
                    record.to_event,
                    metadata_json,
                ],
            )
        })?;

        self.list_trace_spans(&run_id)?
            .into_iter()
            .find(|span| span.id == id)
            .ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "Inserted trace span record {id} but could not read it back"
                ))
            })
    }

    pub fn list_trace_spans(&self, run_id: &str) -> Result<Vec<TraceSpanRecord>> {
        validate_required("run_id", run_id)?;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, span_id, status, duration_ms, from_event, to_event,
                       metadata_json
                FROM trace_spans
                WHERE run_id = ?1
                ORDER BY rowid ASC
                "#,
            )
            .map_err(sqlite_error("prepare list trace span records"))?;
        let rows = statement
            .query_map([run_id], row_to_trace_span_record)
            .map_err(sqlite_error("list trace span records"))?;

        collect_rows(rows, "collect trace span records")
    }

    fn get_trace_span(&self, trace_span_id: &str) -> Result<Option<TraceSpanRecord>> {
        validate_required("trace_span_id", trace_span_id)?;
        self.connection
            .query_row(
                r#"
                SELECT id, run_id, span_id, status, duration_ms, from_event, to_event,
                       metadata_json
                FROM trace_spans
                WHERE id = ?1
                "#,
                [trace_span_id],
                row_to_trace_span_record,
            )
            .optional()
            .map_err(sqlite_error("read trace span record"))
    }

    pub fn import_trace_span(&self, span: &TraceSpanRecord) -> Result<()> {
        validate_required("trace_span.id", &span.id)?;
        if self.get_run(&span.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "trace_span.run_id",
                format!("referenced run record not found: {}", span.run_id),
                Some(span.run_id.clone()),
                None,
            ));
        }
        if let Some(existing) = self.get_trace_span(&span.id)? {
            return ensure_identical("trace_span", &span.id, &existing, span);
        }
        let metadata_json = serialize_metadata(&span.metadata_json)?;
        execute_with_retry("import trace span record", || {
            self.connection.execute(
                r#"
                INSERT INTO trace_spans(
                    id,
                    run_id,
                    span_id,
                    status,
                    duration_ms,
                    from_event,
                    to_event,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    span.id,
                    span.run_id,
                    span.span_id,
                    span.status,
                    span.duration_ms,
                    span.from_event,
                    span.to_event,
                    metadata_json,
                ],
            )
        })?;
        Ok(())
    }
}
