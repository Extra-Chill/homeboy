use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

mod findings;
mod schema;
mod triage_items;

use super::context::RunContext;
pub use super::context::{
    LAB_OFFLOAD_METADATA_ENV, PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV,
    SOURCE_SNAPSHOT_METADATA_ENV,
};
use super::records::{
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, FindingListFilter,
    FindingRecord, NewFindingRecord, NewRunRecord, NewTraceRunRecord, NewTraceSpanRecord,
    NewTriageItemRecord, RunListFilter, RunRecord, RunStatus, TraceRunRecord, TraceSpanRecord,
    TriageItemRecord, TriagePullRequestSignals,
};
use crate::core::{paths, Error, Result};

pub const CURRENT_SCHEMA_VERSION: i64 = 6;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObservationDbStatus {
    pub path: String,
    pub exists: bool,
    pub schema_version: i64,
    pub migration_count: i64,
    pub table_count: i64,
}

pub struct ObservationStore {
    connection: Connection,
    path: PathBuf,
}

impl ObservationStore {
    /// Open and lazily initialize the local observed-state database.
    pub fn open_initialized() -> Result<Self> {
        let path = database_path()?;
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

        self.connection
            .execute(
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
            .map_err(sqlite_error("insert run record"))?;

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
                self.connection
                    .execute(
                        r#"
                        UPDATE runs
                        SET finished_at = ?1, status = ?2, metadata_json = ?3
                        WHERE id = ?4
                        "#,
                        params![finished_at, status.as_str(), serialized, run_id],
                    )
                    .map_err(sqlite_error("finish run record with metadata"))?
            }
            None => self
                .connection
                .execute(
                    r#"
                    UPDATE runs
                    SET finished_at = ?1, status = ?2
                    WHERE id = ?3
                    "#,
                    params![finished_at, status.as_str(), run_id],
                )
                .map_err(sqlite_error("finish run record"))?,
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
        let rows = self
            .connection
            .execute(
                r#"
                UPDATE runs
                SET metadata_json = ?1
                WHERE id = ?2
                "#,
                params![serialized, run_id],
            )
            .map_err(sqlite_error("update run metadata"))?;

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
        let inserted = self
            .connection
            .execute(
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
            .map_err(sqlite_error("import run record"))?;
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
        self.connection
            .execute(
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
            .map_err(sqlite_error("upsert imported run record"))?;
        Ok(())
    }

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
        self.connection
            .execute(
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
                    serialize_metadata(&artifact.metadata_json)?,
                    artifact.created_at,
                ],
            )
            .map_err(sqlite_error("import artifact record"))?;
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

        self.connection
            .execute(
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
                    serialize_metadata(&artifact.metadata_json)?,
                    created_at,
                ],
            )
            .map_err(sqlite_error("insert artifact record"))?;

        let artifact = self
            .list_artifacts(run_id)?
            .into_iter()
            .find(|artifact| artifact.id == id)
            .ok_or_else(|| {
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

        self.connection
            .execute(
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
                    serialize_metadata(&metadata_json)?,
                    created_at,
                ],
            )
            .map_err(sqlite_error("insert directory artifact record"))?;

        self.list_artifacts(run_id)?
            .into_iter()
            .find(|artifact| artifact.id == id)
            .ok_or_else(|| {
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

        self.connection
            .execute(
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
                    serialize_metadata(&metadata_json)?,
                    created_at,
                ],
            )
            .map_err(sqlite_error("insert URL artifact record"))?;

        self.list_artifacts(run_id)?
            .into_iter()
            .find(|artifact| artifact.id == id)
            .ok_or_else(|| {
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
        for artifact in self.list_artifacts(run_id)? {
            if artifact.id == artifact_token
                || artifact.kind == artifact_token
                || artifact
                    .metadata_json
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value == artifact_token)
                || artifact
                    .metadata_json
                    .get("original_manifest_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value == artifact_token)
            {
                return Ok(Some(artifact));
            }
        }
        Ok(None)
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
        let rows = self
            .connection
            .execute("DELETE FROM artifacts WHERE id = ?1", [artifact_id])
            .map_err(sqlite_error("delete artifact record"))?;
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

        self.connection
            .execute(
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
            .map_err(sqlite_error("insert trace run record"))?;

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

        self.connection
            .execute(
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
            .map_err(sqlite_error("insert trace span record"))?;

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
        self.connection
            .execute(
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
            .map_err(sqlite_error("import trace span record"))?;
        Ok(())
    }
}

pub fn database_path() -> Result<PathBuf> {
    schema::database_path()
}

/// Read local observation-store status without creating the database.
pub fn status() -> Result<ObservationDbStatus> {
    schema::status()
}

fn validate_required(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "value cannot be empty",
            None,
            None,
        ));
    }
    Ok(())
}

fn ensure_identical<T: PartialEq>(kind: &str, id: &str, existing: &T, incoming: &T) -> Result<()> {
    if existing == incoming {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        format!("{kind}.id"),
        format!("existing {kind} record conflicts with imported bundle record: {id}"),
        Some(id.to_string()),
        None,
    ))
}

fn serialize_metadata(metadata_json: &serde_json::Value) -> Result<String> {
    serde_json::to_string(metadata_json).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize run metadata".to_string()))
    })
}

fn with_run_context_metadata(
    mut metadata: serde_json::Value,
    context: &RunContext,
) -> serde_json::Value {
    let owner = serde_json::json!({
        "pid": std::process::id(),
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });

    let mut additions = vec![("homeboy_run_owner".to_string(), owner)];
    if let Some(source_snapshot) = &context.provenance.source_snapshot {
        additions.push(("source_snapshot".to_string(), source_snapshot.clone()));
    }
    if let Some(lab_offload) = &context.provenance.lab_offload {
        additions.push(("lab_offload".to_string(), lab_offload.clone()));
    }
    if let Some(preview) = &context.provenance.preview {
        additions.push(("preview".to_string(), preview.clone()));
    }
    if let Some(artifact_mirror) = &context.provenance.artifact_mirror {
        additions.push(("artifact_mirror".to_string(), artifact_mirror.clone()));
    }

    let target = if metadata.is_object() {
        &mut metadata
    } else {
        metadata = serde_json::json!({
            "homeboy_original_metadata": metadata,
        });
        &mut metadata
    };

    if let Some(object) = target.as_object_mut() {
        for (key, value) in additions {
            object.insert(key, value);
        }
    }

    metadata
}

fn parse_metadata(raw: String) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })
}

fn row_to_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        component_id: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status: row.get(5)?,
        command: row.get(6)?,
        cwd: row.get(7)?,
        homeboy_version: row.get(8)?,
        git_sha: row.get(9)?,
        rig_id: row.get(10)?,
        metadata_json: parse_metadata(row.get(11)?)?,
    })
}

fn row_to_artifact_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        artifact_type: row.get(3)?,
        path: row.get(4)?,
        url: if row.get_ref(3)?.as_str()? == "url" {
            Some(row.get(4)?)
        } else {
            None
        },
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: row.get(5)?,
        size_bytes: row.get(6)?,
        mime: row.get(7)?,
        metadata_json: parse_metadata(row.get(8)?)?,
        created_at: row.get(9)?,
    })
}

fn row_to_artifact_cleanup_candidate(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ArtifactCleanupCandidateRecord> {
    Ok(ArtifactCleanupCandidateRecord {
        artifact: row_to_artifact_record(row)?,
        run_kind: row.get(10)?,
        component_id: row.get(11)?,
        run_started_at: row.get(12)?,
        run_status: row.get(13)?,
    })
}

fn row_to_trace_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceRunRecord> {
    Ok(TraceRunRecord {
        run_id: row.get(0)?,
        component_id: row.get(1)?,
        rig_id: row.get(2)?,
        scenario_id: row.get(3)?,
        status: row.get(4)?,
        baseline_status: row.get(5)?,
        metadata_json: parse_metadata(row.get(6)?)?,
    })
}

fn row_to_trace_span_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceSpanRecord> {
    Ok(TraceSpanRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        span_id: row.get(2)?,
        status: row.get(3)?,
        duration_ms: row.get(4)?,
        from_event: row.get(5)?,
        to_event: row.get(6)?,
        metadata_json: parse_metadata(row.get(7)?)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    context: &'static str,
) -> Result<Vec<T>> {
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(sqlite_error(context))?);
    }
    Ok(records)
}

fn persisted_artifact_path(run_id: &str, artifact_id: &str, source: &Path) -> Result<PathBuf> {
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{artifact_id}-{name}"))
        .unwrap_or_else(|| artifact_id.to_string());
    Ok(paths::artifact_root()?.join(run_id).join(file_name))
}

fn copy_artifact_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("create artifact directory {}", parent.display())),
            )
        })?;
    }
    fs::copy(source, target).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "persist artifact {} to {}",
                source.display(),
                target.display()
            )),
        )
    })?;
    Ok(())
}

fn copy_artifact_directory(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create artifact directory {}", target.display())),
        )
    })?;
    for entry in fs::read_dir(source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read artifact directory {}", source.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact directory entry {}",
                    source.display()
                )),
            )
        })?;
        let entry_source = entry.path();
        let entry_target = target.join(entry.file_name());
        let entry_type = entry.file_type().map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact entry type {}",
                    entry_source.display()
                )),
            )
        })?;
        if entry_type.is_dir() {
            copy_artifact_directory(&entry_source, &entry_target)?;
        } else if entry_type.is_file() {
            copy_artifact_file(&entry_source, &entry_target)?;
        }
    }
    Ok(())
}

fn sqlite_error(context: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> Error {
    let context = context.into();
    move |error| {
        Error::internal_unexpected(format!(
            "SQLite observation store error: {context}: {error}"
        ))
    }
}

#[cfg(test)]
#[path = "../../../tests/core/observation/store_test.rs"]
mod store_test;
