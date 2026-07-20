use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::*;

impl ObservationStore {
    pub fn import_artifact(&self, artifact: &ArtifactRecord) -> Result<()> {
        let artifact = artifact_with_link_metadata(artifact);
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
            if artifact.artifact_type == "remote_file"
                && remote_projection_identity_matches(&existing, &artifact)
            {
                // A controller can crash after inserting this row and before it
                // records the projection marker. Preserve the original lifecycle
                // timestamp while accepting the identical retry.
                return Ok(());
            }
            return ensure_identical("artifact", &artifact.id, &existing, &artifact);
        }
        let metadata_json = serialize_metadata(&artifact.metadata_json)?;
        let viewer_links_json = serialize_metadata(&serde_json::json!(artifact.viewer_links))?;
        execute_with_retry("import artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                "#,
                params![
                    artifact.id,
                    artifact.run_id,
                    artifact.kind,
                    artifact.artifact_type,
                    artifact.path,
                    artifact.url,
                    artifact.public_url,
                    artifact.viewer_url,
                    viewer_links_json,
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
        self.record_artifact_with_id_and_metadata(run_id, kind, path, None, metadata_json)
    }

    /// Record verified file bytes under a caller-provided stable logical id.
    /// This is used when an owning lifecycle already defines artifact identity.
    pub fn record_artifact_with_id(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
        artifact_id: &str,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        self.record_artifact_with_id_and_metadata(
            run_id,
            kind,
            path,
            Some(artifact_id),
            metadata_json,
        )
    }

    /// Import bytes whose stable identity and integrity metadata were published
    /// by another durable store. Validate before publishing so a mirror never
    /// turns an advertised artifact into controller-owned corrupt evidence.
    pub fn record_verified_artifact_with_id(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
        artifact_id: &str,
        expected_size_bytes: Option<i64>,
        expected_sha256: Option<&str>,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read artifact metadata {}", path.display())),
            )
        })?;
        let actual_size_bytes = i64::try_from(metadata.len()).ok();
        if expected_size_bytes.is_some() && expected_size_bytes != actual_size_bytes {
            return Err(Error::validation_invalid_argument(
                "artifact.size_bytes",
                format!(
                    "artifact `{artifact_id}` size does not match the published durable metadata"
                ),
                actual_size_bytes.map(|value| value.to_string()),
                None,
            ));
        }
        if let Some(expected_sha256) = expected_sha256 {
            let actual_sha256 = crate::artifact_metadata::sha256_file(path)?;
            if actual_sha256 != expected_sha256 {
                return Err(Error::validation_invalid_argument(
                    "artifact.sha256",
                    format!(
                        "artifact `{artifact_id}` SHA-256 does not match the published durable metadata"
                    ),
                    Some(actual_sha256),
                    None,
                ));
            }
        }
        self.record_artifact_with_id(run_id, kind, path, artifact_id, metadata_json)
    }

    fn record_artifact_with_id_and_metadata(
        &self,
        run_id: &str,
        kind: &str,
        path: impl AsRef<Path>,
        artifact_id: Option<&str>,
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

        let id = artifact_id
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let stored_path = persisted_artifact_path(run_id, &id, path)?;
        let staged_path = staged_artifact_path(&stored_path, Uuid::new_v4());
        copy_artifact_file(path, &staged_path)?;
        let staged_metadata = fs::metadata(&staged_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("inspect staged artifact bytes".to_string()),
            )
        })?;
        let size_bytes = i64::try_from(staged_metadata.len()).ok();
        let sha256 = Some(crate::artifact_metadata::sha256_file(&staged_path)?);
        if let Some(existing) = self.get_artifact(&id)? {
            let existing_path = Path::new(&existing.path);
            let existing_matches = existing.run_id == run_id
                && existing.kind == kind
                && existing.size_bytes == size_bytes
                && existing.sha256 == sha256
                && fs::metadata(existing_path)
                    .map(|value| value.is_file())
                    .unwrap_or(false)
                && crate::artifact_metadata::sha256_file(existing_path).ok() == sha256;
            fs::remove_file(&staged_path).ok();
            if existing_matches {
                return Ok(existing);
            }
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!("stable artifact id '{id}' already records different content or ownership"),
                Some(id),
                None,
            ));
        }
        let published_new_file = match fs::hard_link(&staged_path, &stored_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing_matches = fs::metadata(&stored_path)
                    .map(|value| value.is_file() && i64::try_from(value.len()).ok() == size_bytes)
                    .unwrap_or(false)
                    && crate::artifact_metadata::sha256_file(&stored_path).ok() == sha256;
                fs::remove_file(&staged_path).ok();
                if !existing_matches {
                    return Err(Error::validation_invalid_argument(
                        "artifact_id",
                        format!("stable artifact id '{id}' already publishes different bytes"),
                        Some(id),
                        None,
                    ));
                }
                false
            }
            Err(error) => {
                fs::remove_file(&staged_path).ok();
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("publish persisted artifact".to_string()),
                ));
            }
        };
        if published_new_file {
            fs::remove_file(&staged_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("remove persisted artifact staging file".to_string()),
                )
            })?;
        }
        let created_at = chrono::Utc::now().to_rfc3339();
        let mime = crate::artifact_metadata::content_type_from_path(path);
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
        crate::artifact_links::annotate_public_artifact_url_validation(&mut artifact);

        let metadata_json_str = serialize_metadata(&artifact.metadata_json)?;
        let viewer_links_json = serialize_metadata(&serde_json::json!(artifact.viewer_links))?;
        if let Err(error) = execute_with_retry("insert artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "file",
                    path_string,
                    Option::<String>::None,
                    artifact.public_url,
                    artifact.viewer_url,
                    viewer_links_json,
                    sha256,
                    size_bytes,
                    mime,
                    metadata_json_str,
                    created_at,
                ],
            )
        }) {
            if published_new_file {
                fs::remove_file(&stored_path).ok();
            }
            return Err(error);
        }

        let artifact = self.get_artifact(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted artifact record {id} but could not read it back"
            ))
        })?;
        // Nested publication artifacts referenced by a manifest are authored
        // relative to the *original* manifest path, not the UUID-addressed
        // stored copy. Pass both so the materializer can locate a nested
        // artifact-store ref next to the manifest as recorded.
        crate::publication_artifacts::index_published_artifact_refs(
            self,
            &artifact,
            &[stored_path.as_path(), path],
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
        let viewer_links_json = serialize_metadata(&serde_json::json!([]))?;
        execute_with_retry("insert directory artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "directory",
                    path_string,
                    Option::<String>::None,
                    Option::<String>::None,
                    Option::<String>::None,
                    viewer_links_json,
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
        let viewer_links_json = serialize_metadata(&serde_json::json!([]))?;
        execute_with_retry("insert URL artifact record", || {
            self.connection.execute(
                r#"
                INSERT INTO artifacts(id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                "#,
                params![
                    id,
                    run_id,
                    kind,
                    "url",
                    url,
                    url,
                    Option::<String>::None,
                    Option::<String>::None,
                    viewer_links_json,
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
                SELECT id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at
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

    pub fn update_artifact_metadata(
        &self,
        artifact_id: &str,
        metadata_json: serde_json::Value,
    ) -> Result<ArtifactRecord> {
        validate_required("artifact_id", artifact_id)?;
        let serialized = serialize_metadata(&metadata_json)?;
        let rows = execute_with_retry("update artifact metadata", || {
            self.connection.execute(
                r#"
                UPDATE artifacts
                SET metadata_json = ?1
                WHERE id = ?2
                "#,
                params![serialized, artifact_id],
            )
        })?;
        if rows == 0 {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!("artifact record not found: {artifact_id}"),
                Some(artifact_id.to_string()),
                None,
            ));
        }
        self.get_artifact(artifact_id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Updated artifact record {artifact_id} but could not read it back"
            ))
        })
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
                SELECT id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at
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
                            a.id, a.run_id, a.kind, a.artifact_type, a.path, a.url,
                            a.public_url, a.viewer_url, a.viewer_links_json, a.sha256,
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
                        a.id, a.run_id, a.kind, a.artifact_type, a.path, a.url,
                        a.public_url, a.viewer_url, a.viewer_links_json, a.sha256,
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
                SELECT id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at
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
                SELECT id, run_id, kind, artifact_type, path, url, public_url, viewer_url, viewer_links_json, sha256, size_bytes, mime, metadata_json, created_at
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
                SELECT a.id, a.run_id, a.kind, a.artifact_type, a.path, a.url,
                       a.public_url, a.viewer_url, a.viewer_links_json, a.sha256,
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

fn remote_projection_identity_matches(
    existing: &ArtifactRecord,
    incoming: &ArtifactRecord,
) -> bool {
    existing.id == incoming.id
        && existing.run_id == incoming.run_id
        && existing.kind == incoming.kind
        && existing.artifact_type == "remote_file"
        && existing.path == incoming.path
        && existing.url == incoming.url
        && existing.public_url == incoming.public_url
        && existing.viewer_url == incoming.viewer_url
        && existing.viewer_links == incoming.viewer_links
        && existing.sha256 == incoming.sha256
        && existing.size_bytes == incoming.size_bytes
        && existing.mime == incoming.mime
        && existing.metadata_json == incoming.metadata_json
}

fn staged_artifact_path(stored_path: &Path, staging_id: Uuid) -> std::path::PathBuf {
    // Keep sibling staging names well below common NAME_MAX limits regardless of
    // the content-addressed final artifact name.
    stored_path.with_file_name(format!(".artifact-{staging_id}.staging"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{ArtifactViewerLink, NewRunRecord};
    use crate::test_support::with_isolated_home;
    use std::collections::BTreeSet;

    #[test]
    fn stable_artifact_id_publishes_copied_bytes_and_rejects_conflicts() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("test").cwd_path(home.path()).build())
                .expect("run");
            let source = home.path().join("source.patch");
            fs::write(&source, b"first bytes").expect("write source");

            let first = store
                .record_artifact_with_id(
                    &run.id,
                    "patch",
                    &source,
                    "stable-patch",
                    serde_json::json!({}),
                )
                .expect("first publication");
            assert_eq!(first.size_bytes, Some(11));
            assert_eq!(
                first.sha256,
                Some(crate::artifact_metadata::sha256_file(&source).expect("source hash"))
            );
            assert_eq!(
                fs::read(&first.path).expect("persisted bytes"),
                b"first bytes"
            );

            let replay = store
                .record_artifact_with_id(
                    &run.id,
                    "patch",
                    &source,
                    "stable-patch",
                    serde_json::json!({}),
                )
                .expect("identical replay");
            assert_eq!(replay.id, first.id);

            fs::write(&source, b"different bytes").expect("rewrite source");
            assert!(store
                .record_artifact_with_id(
                    &run.id,
                    "patch",
                    &source,
                    "stable-patch",
                    serde_json::json!({}),
                )
                .is_err());
            assert_eq!(
                fs::read(&first.path).expect("original persisted bytes"),
                b"first bytes"
            );
            let stored_directory = Path::new(&first.path).parent().expect("stored directory");
            assert!(fs::read_dir(stored_directory)
                .expect("stored directory entries")
                .all(|entry| !entry
                    .expect("stored directory entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".artifact-")));
        });
    }

    #[test]
    fn staging_paths_are_bounded_and_unique_for_long_final_names() {
        let root = Path::new("/artifacts/run");
        for final_name in ["a".repeat(240), "é".repeat(120)] {
            let stored_path = root.join(final_name);
            let staging_paths = [Uuid::from_u128(1), Uuid::from_u128(2)]
                .into_iter()
                .map(|staging_id| staged_artifact_path(&stored_path, staging_id))
                .collect::<Vec<_>>();
            let staging_names = staging_paths
                .iter()
                .map(|path| path.file_name().expect("staging file name").to_owned())
                .collect::<BTreeSet<_>>();

            assert_eq!(staging_names.len(), 2);
            for staging_path in staging_paths {
                assert_eq!(staging_path.parent(), Some(root));
                assert!(staging_path
                    .file_name()
                    .expect("staging file name")
                    .to_string_lossy()
                    .starts_with(".artifact-"));
                assert!(
                    staging_path
                        .file_name()
                        .expect("staging file name")
                        .to_string_lossy()
                        .len()
                        < 64
                );
            }
        }
    }

    #[test]
    fn long_artifact_names_persist_without_expanding_staging_names() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("test").cwd_path(home.path()).build())
                .expect("run");

            for (index, name) in ["a".repeat(200), "é".repeat(100)].into_iter().enumerate() {
                let source = home.path().join(name);
                fs::write(&source, b"long filename artifact").expect("write source");
                let artifact = store
                    .record_artifact_with_id(
                        &run.id,
                        "evidence",
                        &source,
                        &format!("long-name-{index}"),
                        serde_json::json!({}),
                    )
                    .expect("persist long-name artifact");

                assert!(Path::new(&artifact.path).is_file());
                assert_eq!(
                    fs::read(&artifact.path).expect("persisted bytes"),
                    b"long filename artifact"
                );
            }
        });
    }

    #[test]
    fn verified_stable_fuzz_artifact_rejects_published_integrity_mismatch_before_copying() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("fuzz").cwd_path(home.path()).build())
                .expect("run");
            let source = home.path().join("fuzz-results.json");
            fs::write(&source, b"durable fuzz bytes").expect("write source");
            let size = i64::try_from(fs::metadata(&source).expect("metadata").len())
                .expect("test size fits i64");
            let sha256 = crate::artifact_metadata::sha256_file(&source).expect("source hash");

            let artifact = store
                .record_verified_artifact_with_id(
                    &run.id,
                    "fuzz_results",
                    &source,
                    "remote-fuzz-results",
                    Some(size),
                    Some(&sha256),
                    serde_json::json!({ "owner": "controller" }),
                )
                .expect("verified publication");
            assert_eq!(
                fs::read(&artifact.path).expect("controller bytes"),
                b"durable fuzz bytes"
            );

            let error = store
                .record_verified_artifact_with_id(
                    &run.id,
                    "fuzz_results",
                    &source,
                    "remote-fuzz-results-mismatch",
                    Some(size + 1),
                    Some(&sha256),
                    serde_json::json!({}),
                )
                .expect_err("published size mismatch must fail closed");
            assert_eq!(error.code, crate::ErrorCode::ValidationInvalidArgument);
            assert!(store
                .get_artifact("remote-fuzz-results-mismatch")
                .expect("lookup")
                .is_none());
        });
    }

    #[test]
    fn controller_owned_verified_fuzz_artifacts_survive_source_cleanup_and_remain_retrievable() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("fuzz").cwd_path(home.path()).build())
                .expect("run");
            let run_dir = crate::engine::run_dir::RunDir::create().expect("transient run dir");
            let fixtures = [
                ("fuzz-results", "fuzz_results", b"results".as_slice()),
                (
                    "execution-request",
                    "fuzz_execution_request",
                    b"request".as_slice(),
                ),
                (
                    "result-envelope",
                    "fuzz_result_envelope",
                    b"envelope".as_slice(),
                ),
                ("coverage", "fuzz_coverage", b"coverage".as_slice()),
            ];
            let mut expected = Vec::new();
            for (id, kind, bytes) in fixtures {
                let source = run_dir.step_file(id);
                fs::write(&source, bytes).expect("write runner artifact");
                let size = i64::try_from(bytes.len()).expect("test size fits i64");
                let sha256 = crate::artifact_metadata::sha256_file(&source).expect("source hash");
                store
                    .record_verified_artifact_with_id(
                        &run.id,
                        kind,
                        &source,
                        id,
                        Some(size),
                        Some(&sha256),
                        serde_json::json!({ "owner": "controller" }),
                    )
                    .expect("controller mirror records bytes");
                expected.push((id, size, sha256, bytes.to_vec()));
            }

            run_dir.cleanup();
            for (id, size, sha256, bytes) in expected {
                let artifact =
                    crate::observation::runs_service::resolve_artifact_for_run(&store, &run.id, id)
                        .expect("controller mirror resolves artifact after runner cleanup");
                let output = home.path().join("retrieved").join(id);
                let retrieved = crate::observation::runs_service::copy_local_file_artifact(
                    artifact,
                    Some(output.clone()),
                )
                .expect("controller mirror retrieves artifact while runner is unavailable");
                assert_eq!(retrieved.size_bytes, Some(size));
                assert_eq!(retrieved.sha256.as_deref(), Some(sha256.as_str()));
                assert_eq!(fs::read(output).expect("retrieved bytes"), bytes);
            }
        });
    }

    #[test]
    fn import_artifact_round_trips_public_link_metadata() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("fuzz")
                        .command("homeboy fuzz run component-a")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");

            store
                .import_artifact(&ArtifactRecord {
                    id: "artifact-1".to_string(),
                    run_id: run.id.clone(),
                    kind: "fuzz_result_envelope".to_string(),
                    artifact_type: "remote_file".to_string(),
                    path: "/runner/private/fuzz-result-envelope.json".to_string(),
                    url: Some("https://example.com/raw/fuzz-result-envelope.json".to_string()),
                    public_url: Some("https://example.com/fuzz-result-envelope.json".to_string()),
                    viewer_url: Some("https://viewer.example.com/fuzz-result-envelope".to_string()),
                    viewer_links: vec![ArtifactViewerLink {
                        kind: "json".to_string(),
                        url: "https://viewer.example.com/fuzz-result-envelope".to_string(),
                        replay: None,
                    }],
                    sha256: None,
                    size_bytes: None,
                    mime: Some("application/json".to_string()),
                    metadata_json: serde_json::json!({ "schema": "test" }),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("import artifact");

            let artifact = store
                .get_artifact("artifact-1")
                .expect("get artifact")
                .expect("artifact exists");

            assert_eq!(
                artifact.url.as_deref(),
                Some("https://example.com/raw/fuzz-result-envelope.json")
            );
            assert_eq!(
                artifact.public_url.as_deref(),
                Some("https://example.com/fuzz-result-envelope.json")
            );
            assert_eq!(
                artifact.viewer_url.as_deref(),
                Some("https://viewer.example.com/fuzz-result-envelope")
            );
            assert_eq!(artifact.viewer_links.len(), 1);
            assert_eq!(artifact.viewer_links[0].kind, "json");
        });
    }

    #[test]
    fn remote_projection_retry_preserves_inserted_row_lifecycle_time() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("test").cwd_path(home.path()).build())
                .expect("run");
            let mut artifact = ArtifactRecord {
                id: "remote-projection".to_string(),
                run_id: run.id,
                kind: "patch".to_string(),
                artifact_type: "remote_file".to_string(),
                path: "runner-artifact://runner%2Fa/run%20b/patch".to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some("abc".to_string()),
                size_bytes: Some(3),
                mime: Some("text/x-patch".to_string()),
                metadata_json: serde_json::json!({ "name": "patch" }),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            };
            store.import_artifact(&artifact).expect("first projection");
            artifact.created_at = "2026-01-02T00:00:00Z".to_string();
            store
                .import_artifact(&artifact)
                .expect("retry after row insertion");
            assert_eq!(
                store
                    .get_artifact("remote-projection")
                    .expect("read projection")
                    .expect("projection")
                    .created_at,
                "2026-01-01T00:00:00Z"
            );
            artifact.path = "runner-artifact://runner%2Fa/run%20b/other".to_string();
            assert!(store.import_artifact(&artifact).is_err());
        });
    }
}

fn artifact_with_link_metadata(artifact: &ArtifactRecord) -> ArtifactRecord {
    let mut artifact = artifact.clone();
    if artifact.metadata_json.is_null() {
        artifact.metadata_json = serde_json::json!({});
    }
    if let Some(metadata) = artifact.metadata_json.as_object_mut() {
        if let Some(url) = artifact.url.as_ref() {
            metadata
                .entry("url".to_string())
                .or_insert_with(|| serde_json::Value::String(url.clone()));
        }
        if let Some(public_url) = artifact.public_url.as_ref() {
            metadata
                .entry("public_url".to_string())
                .or_insert_with(|| serde_json::Value::String(public_url.clone()));
        }
        if let Some(viewer_url) = artifact.viewer_url.as_ref() {
            metadata
                .entry("viewer_url".to_string())
                .or_insert_with(|| serde_json::Value::String(viewer_url.clone()));
        }
    }
    artifact
}
