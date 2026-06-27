//! Run/artifact observation service.
//!
//! Reusable lookup, enrichment, artifact retrieval, and mirrored-daemon
//! evidence refresh primitives extracted from `src/commands/runs.rs`. The
//! goals here are:
//!
//! * Keep CLI argument parsing and output enum serialization in the
//!   `commands::runs` adapter where it belongs.
//! * Expose run/artifact query and mutation primitives that other
//!   consumers (HTTP API, MCP, future automation) can reuse without
//!   going through the CLI output enum.
//!
//! Behavior here mirrors the previous `commands::runs` helpers byte-for-byte,
//! including the order of side effects (reconcile → refresh evidence → index
//! nested publication artifacts → list artifacts → enrich links). The
//! `commands::runs` callers are thin wrappers that map the returned data
//! into `RunsOutput` variants.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use super::{
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, ObservationStore,
    RunListFilter, RunRecord,
};
use crate::core::artifact_links::{cached_validated_viewer_links, public_artifact_url};
use crate::core::engine::temp::CleanupSizeTotals;
use crate::core::execution_contract::EXECUTION_CONTRACT;
use crate::core::runners::RunnerArtifactRef;
use crate::core::Error;
use crate::core::Result;

/// Output of a successful artifact byte retrieval (whether the bytes came
/// from a locally-recorded file or from a remote runner).
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactFetchOutcome {
    pub run_id: String,
    pub artifact_id: String,
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<RunnerArtifactRef>,
}

/// Outcome of `get_artifact_bytes` describing where the bytes were written.
pub enum ArtifactGetSource {
    /// Bytes copied from a locally-recorded file artifact.
    Local,
    /// Bytes fetched from a remote runner cache.
    Remote,
}

#[derive(Debug, Clone, Default)]
pub struct RunnerDownloadCleanupOptions {
    pub apply: bool,
    pub runner: Option<String>,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDownloadCleanupOutcome {
    pub dry_run: bool,
    pub root: PathBuf,
    pub removed: bool,
    pub file_count: usize,
    pub directory_count: usize,
    pub size_bytes: u64,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PersistedArtifactCleanupOptions {
    pub apply: bool,
    pub older_than_days: i64,
    pub run_id: Option<String>,
    pub kind: Option<String>,
    pub artifact_type: Option<String>,
    pub run_kind: Option<String>,
    pub component_id: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistedArtifactCleanupOutcome {
    pub dry_run: bool,
    pub artifact_root: PathBuf,
    pub older_than_days: i64,
    #[serde(flatten)]
    pub totals: CleanupSizeTotals,
    pub planned_record_count: usize,
    pub planned_file_count: usize,
    pub planned_directory_count: usize,
    pub removed_record_count: usize,
    pub removed_file_count: usize,
    pub removed_directory_count: usize,
    pub skipped_count: usize,
    pub rows: Vec<PersistedArtifactCleanupRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistedArtifactCleanupRow {
    pub artifact_id: String,
    pub run_id: String,
    pub run_kind: String,
    pub component_id: Option<String>,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub created_at: String,
    pub exists: bool,
    pub action: String,
    pub reason: String,
    pub size_bytes: u64,
}

#[derive(Debug, Default)]
struct RunnerDownloadCleanupPreview {
    file_count: usize,
    directory_count: usize,
    size_bytes: u64,
    paths: Vec<String>,
}

/// Storage classes recognized by [`classify_artifact_storage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStorage {
    LocalFile,
    Remote,
    MetadataOnly,
    Other,
}

mod run_lookup {
    use super::*;

    /// Look up a run and surface a stable validation error when it doesn't
    /// exist. Used by every observation command that takes a `run_id`.
    pub fn require_run(store: &ObservationStore, run_id: &str) -> Result<RunRecord> {
        if let Some(run) = store.get_run(run_id)? {
            return Ok(run);
        }
        if let Ok(Some(run)) = crate::core::runners::mirror_connected_runner_run(run_id) {
            return Ok(run);
        }
        Err(missing_run_error(run_id))
    }

    fn missing_run_error(run_id: &str) -> Error {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            Some(missing_run_guidance(run_id)),
        )
    }

    fn missing_run_guidance(run_id: &str) -> Vec<String> {
        let mut hints = Vec::new();
        let connected = crate::core::runners::statuses()
            .unwrap_or_default()
            .into_iter()
            .filter(|report| report.connected)
            .map(|report| report.runner_id)
            .collect::<Vec<_>>();

        if connected.is_empty() {
            hints.push(
                "No connected runner daemon is available for controller-side lookup; connect the offload runner or inspect it with `homeboy runner exec <runner-id> -- homeboy runs list --limit 100`.".to_string(),
            );
            return hints;
        }

        missing_run_guidance_for_runner_ids(run_id, connected)
    }

    pub(crate) fn missing_run_guidance_for_runner_ids(
        run_id: &str,
        runner_ids: Vec<String>,
    ) -> Vec<String> {
        let mut hints = Vec::new();
        for runner_id in runner_ids {
            hints.push(format!(
                "Check runner `{runner_id}` from the controller: `homeboy runs list --runner {runner_id} --limit 100`."
            ));
            hints.push(format!(
                "Inspect run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs show {run_id}`."
            ));
            hints.push(format!(
                "List artifacts for run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs artifacts {run_id}`."
            ));
            hints.push(format!(
                "Export run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs export --run {run_id} --output <dir>`."
            ));
        }
        hints
    }

    /// Best-effort refresh of mirrored Lab runner evidence for a run.
    ///
    /// The previous CLI helper printed a warning to stderr and swallowed the
    /// error. Callers that want richer logging can use
    /// [`refresh_mirrored_daemon_evidence`] directly. This helper preserves the
    /// historical CLI behavior so the `runs show` / `runs artifacts` commands
    /// keep emitting the same stderr text on failures.
    pub fn refresh_mirrored_daemon_evidence_best_effort(run_id: &str) {
        if let Err(err) = crate::core::runners::refresh_mirrored_daemon_evidence(run_id) {
            eprintln!(
                "Warning: could not refresh mirrored Lab runner evidence for `{run_id}`: {}",
                err.message
            );
        }
    }
}
pub use run_lookup::*;

mod artifact_links {
    use super::*;

    /// Enrich a single artifact record with public/viewer link metadata.
    ///
    /// Mirrors the original CLI helper exactly: derive a public URL (from
    /// stored artifact metadata or by treating the artifact path as the URL
    /// for `url`-typed artifacts), then resolve any cached viewer links.
    pub(crate) fn enrich_artifact_link(mut artifact: ArtifactRecord) -> ArtifactRecord {
        let public_url =
            public_artifact_url(&artifact).or_else(|| public_url_for_url_artifact(&artifact));
        if let Some(url) = public_url.clone() {
            artifact.public_url = Some(url.clone());
            artifact.viewer_links = cached_validated_viewer_links(&artifact, &url);
            artifact.viewer_url = artifact.viewer_links.first().map(|link| link.url.clone());
        }
        artifact
    }

    /// Enrich a list of artifacts, preserving order.
    pub fn enrich_artifact_links(artifacts: Vec<ArtifactRecord>) -> Vec<ArtifactRecord> {
        artifacts.into_iter().map(enrich_artifact_link).collect()
    }

    fn public_url_for_url_artifact(artifact: &ArtifactRecord) -> Option<String> {
        (artifact.artifact_type == "url")
            .then(|| artifact.url.clone().or_else(|| Some(artifact.path.clone())))
            .flatten()
    }

    /// Collect artifacts belonging to remote bench/trace runs that share the
    /// same Lab `remote_job_id` with the supplied runner-exec run. Used so
    /// `runs artifacts <runner-job-run>` surfaces the downstream bench/trace
    /// artifacts produced inside the same Lab job.
    pub fn related_lab_artifacts_for_runner_job(
        store: &ObservationStore,
        run: &RunRecord,
    ) -> Result<Vec<ArtifactRecord>> {
        if run.kind != "runner-exec" {
            return Ok(Vec::new());
        }
        let Some((_runner_id, job_id)) = crate::core::runners::mirrored_runner_job_identity(run)
        else {
            return Ok(Vec::new());
        };
        let mut artifacts = Vec::new();
        for candidate in store.list_runs(RunListFilter {
            kind: None,
            component_id: None,
            status: None,
            rig_id: None,
            limit: Some(1000),
        })? {
            if candidate.id == run.id {
                continue;
            }
            if candidate
                .metadata_json
                .pointer("/lab/remote_job_id")
                .and_then(Value::as_str)
                != Some(job_id.as_str())
            {
                continue;
            }
            artifacts.extend(store.list_artifacts(&candidate.id)?);
        }
        Ok(artifacts)
    }

    /// List the enriched artifact records for a run, including downstream
    /// Lab job artifacts.
    ///
    /// Side-effect ordering matches the CLI: refresh mirrored daemon evidence,
    /// then index nested publication artifact refs, then list and enrich.
    pub fn list_artifacts_for_run(
        store: &ObservationStore,
        run_id: &str,
    ) -> Result<Vec<ArtifactRecord>> {
        let run = require_run(store, run_id)?;
        refresh_mirrored_daemon_evidence_best_effort(run_id);
        crate::core::artifacts::index_remote_published_artifact_refs_for_run(store, run_id)?;
        let mut artifacts = store.list_artifacts(run_id)?;
        artifacts.extend(related_lab_artifacts_for_runner_job(store, &run)?);
        Ok(enrich_artifact_links(artifacts))
    }
}
pub use artifact_links::*;

mod artifact_resolve {
    use super::*;

    /// Resolve an artifact record by run/artifact token, validating that the
    /// recorded `run_id` matches the requested run.
    ///
    /// The previous CLI helper indexed nested publication artifact refs before
    /// looking up the artifact; this helper preserves that order.
    pub fn resolve_artifact_for_run(
        store: &ObservationStore,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<ArtifactRecord> {
        require_run(store, run_id)?;
        crate::core::artifacts::index_remote_published_artifact_refs_for_run(store, run_id)?;
        let artifact = store
            .get_artifact_for_run_token(run_id, artifact_id)?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "artifact_id",
                    format!("artifact record not found: {artifact_id}"),
                    Some(artifact_id.to_string()),
                    None,
                )
            })?;

        if artifact.run_id != run_id {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                "artifact does not belong to requested run",
                Some(artifact_id.to_string()),
                None,
            ));
        }
        Ok(artifact)
    }

    /// Copy a recorded file artifact's bytes to `output`.
    ///
    /// Returns a stable `ArtifactFetchOutcome` so callers can present the
    /// summary in their preferred format. Validates that the artifact is a
    /// local file (callers should detect remote/metadata-only artifacts and
    /// dispatch separately).
    pub fn copy_local_file_artifact(
        artifact: ArtifactRecord,
        output: Option<PathBuf>,
    ) -> Result<ArtifactFetchOutcome> {
        if artifact.artifact_type != "file" {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "artifact {} is {}, not a downloadable file",
                    artifact.id, artifact.artifact_type
                ),
                Some(artifact.id),
                None,
            ));
        }

        let source = PathBuf::from(&artifact.path);
        if !source.is_file() {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "artifact {} file is missing or unreadable at {}; rerun the source command or import a bundle that includes artifact bytes",
                    artifact.id,
                    source.display()
                ),
                Some(artifact.id),
                None,
            ));
        }
        let file_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&artifact.id)
            .to_string();
        let output = output.unwrap_or_else(|| PathBuf::from(file_name));
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
            })?;
        }

        let mut reader = File::open(&source).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("open artifact {}", source.display())),
            )
        })?;
        let mut writer = File::create(&output).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", output.display())))
        })?;
        io::copy(&mut reader, &mut writer).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "copy artifact {} to {}",
                    artifact.id,
                    output.display()
                )),
            )
        })?;

        Ok(ArtifactFetchOutcome {
            run_id: artifact.run_id,
            artifact_id: artifact.id,
            output_path: output,
            content_type: artifact.mime,
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256,
            artifact_ref: None,
        })
    }

    /// Download a remote runner artifact and report the same normalized fetch
    /// outcome used by local artifact copies.
    pub fn download_remote_artifact(
        artifact: ArtifactRecord,
        output: Option<PathBuf>,
    ) -> Result<ArtifactFetchOutcome> {
        let download = crate::core::runners::download_remote_artifact(&artifact.path, output)?;
        Ok(ArtifactFetchOutcome {
            run_id: artifact.run_id,
            artifact_id: artifact.id,
            output_path: download.output_path,
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
            artifact_ref: Some(download.artifact_ref),
        })
    }

    /// Classify an artifact's storage so callers can decide between local
    /// copy, remote download, or a metadata-only error.
    pub fn classify_artifact_storage(artifact: &ArtifactRecord) -> ArtifactStorage {
        if artifact.artifact_type == "file" {
            return ArtifactStorage::LocalFile;
        }
        if crate::core::runners::is_remote_runner_artifact_path(&artifact.path)
            || artifact.artifact_type == "remote_file"
        {
            return ArtifactStorage::Remote;
        }
        if artifact.artifact_type == "metadata-only" {
            return ArtifactStorage::MetadataOnly;
        }
        ArtifactStorage::Other
    }
}
pub use artifact_resolve::*;

mod persisted_cleanup {
    use super::*;

    pub fn cleanup_persisted_artifacts(
        options: PersistedArtifactCleanupOptions,
    ) -> Result<PersistedArtifactCleanupOutcome> {
        if options.older_than_days < 0 {
            return Err(Error::validation_invalid_argument(
                "older_than_days",
                "--older-than-days must be zero or greater",
                Some(options.older_than_days.to_string()),
                None,
            ));
        }

        let artifact_root = crate::core::artifacts::root()?;
        let created_before = (Utc::now() - Duration::days(options.older_than_days)).to_rfc3339();
        let store = ObservationStore::open_initialized()?;
        let candidates = store.list_artifact_cleanup_candidates(ArtifactCleanupFilter {
            created_before: Some(created_before),
            run_id: options.run_id.clone(),
            kind: options.kind.clone(),
            artifact_type: options.artifact_type.clone(),
            run_kind: options.run_kind.clone(),
            component_id: options.component_id.clone(),
            limit: Some(options.limit),
        })?;

        let mut rows = Vec::new();
        let mut planned_record_count = 0;
        let mut planned_file_count = 0;
        let mut planned_directory_count = 0;
        let mut planned_size_bytes = 0;
        let mut removed_record_count = 0;
        let mut removed_file_count = 0;
        let mut removed_directory_count = 0;
        let mut removed_size_bytes = 0;
        let mut skipped_count = 0;

        for candidate in candidates.iter() {
            let mut row = classify_persisted_artifact(candidate, &artifact_root)?;
            if row.action == "remove" {
                planned_record_count += 1;
                planned_size_bytes += row.size_bytes;
                match row.artifact_type.as_str() {
                    "directory" => planned_directory_count += 1,
                    _ => planned_file_count += usize::from(row.exists),
                }
                if options.apply {
                    apply_persisted_artifact_cleanup(&store, &candidate.artifact, &artifact_root)?;
                    removed_record_count += 1;
                    removed_size_bytes += row.size_bytes;
                    match row.artifact_type.as_str() {
                        "directory" => removed_directory_count += 1,
                        _ => removed_file_count += usize::from(row.exists),
                    }
                    row.action = "removed".to_string();
                }
            } else {
                skipped_count += 1;
            }
            rows.push(row);
        }

        Ok(PersistedArtifactCleanupOutcome {
            dry_run: !options.apply,
            artifact_root,
            older_than_days: options.older_than_days,
            totals: CleanupSizeTotals {
                inspected_count: candidates.len(),
                planned_size_bytes,
                removed_size_bytes,
            },
            planned_record_count,
            planned_file_count,
            planned_directory_count,
            removed_record_count,
            removed_file_count,
            removed_directory_count,
            skipped_count,
            rows,
        })
    }

    fn classify_persisted_artifact(
        candidate: &ArtifactCleanupCandidateRecord,
        artifact_root: &Path,
    ) -> Result<PersistedArtifactCleanupRow> {
        let artifact = &candidate.artifact;
        let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
        let mut exists = false;
        let mut size_bytes = 0;
        let (action, reason) = if artifact.artifact_type == "url"
            || crate::core::runners::is_remote_runner_artifact_path(&artifact.path)
            || EXECUTION_CONTRACT
                .artifacts
                .is_metadata_only_ref(&artifact.path)
        {
            ("skip", "artifact is not local persisted bytes")
        } else if let Some(metadata) = symlink_metadata_if_exists(&path)? {
            exists = true;
            if !path_is_within_root(&path, artifact_root) {
                ("skip", "existing artifact path is outside artifact root")
            } else if metadata.file_type().is_symlink() {
                ("skip", "artifact path is a symlink")
            } else {
                size_bytes = path_size_bytes(&path, &metadata)?;
                ("remove", "artifact bytes and DB row are eligible")
            }
        } else {
            ("remove", "artifact bytes are missing; DB row is stale")
        };

        Ok(PersistedArtifactCleanupRow {
            artifact_id: artifact.id.clone(),
            run_id: artifact.run_id.clone(),
            run_kind: candidate.run_kind.clone(),
            component_id: candidate.component_id.clone(),
            kind: artifact.kind.clone(),
            artifact_type: artifact.artifact_type.clone(),
            path: artifact.path.clone(),
            created_at: artifact.created_at.clone(),
            exists,
            action: action.to_string(),
            reason: reason.to_string(),
            size_bytes,
        })
    }

    fn apply_persisted_artifact_cleanup(
        store: &ObservationStore,
        artifact: &ArtifactRecord,
        artifact_root: &Path,
    ) -> Result<()> {
        let path = persisted_artifact_path_from_record(artifact_root, &artifact.path);
        if let Some(metadata) = symlink_metadata_if_exists(&path)? {
            if metadata.file_type().is_symlink() || !path_is_within_root(&path, artifact_root) {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "artifact path failed cleanup safety revalidation",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            if metadata.is_dir() {
                fs::remove_dir_all(&path)
                    .map_err(|err| persisted_artifact_remove_error(&path, err))?;
            } else {
                fs::remove_file(&path)
                    .map_err(|err| persisted_artifact_remove_error(&path, err))?;
            }
        }
        store.delete_artifact_record(&artifact.id)?;
        Ok(())
    }

    fn persisted_artifact_path_from_record(artifact_root: &Path, raw: &str) -> PathBuf {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            artifact_root.join(path)
        }
    }

    fn symlink_metadata_if_exists(path: &Path) -> Result<Option<fs::Metadata>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::internal_io(
                err.to_string(),
                Some(format!("read persisted artifact {}", path.display())),
            )),
        }
    }

    fn path_is_within_root(path: &Path, artifact_root: &Path) -> bool {
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        let root = fs::canonicalize(artifact_root).unwrap_or_else(|_| artifact_root.to_path_buf());
        let candidate = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        candidate.starts_with(root)
    }

    fn path_size_bytes(path: &Path, metadata: &fs::Metadata) -> Result<u64> {
        if metadata.is_dir() {
            let mut total = 0;
            for entry in
                fs::read_dir(path).map_err(|err| persisted_artifact_read_dir_error(path, err))?
            {
                let entry = entry.map_err(|err| persisted_artifact_read_dir_error(path, err))?;
                let entry_path = entry.path();
                let metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("read persisted artifact {}", entry_path.display())),
                    )
                })?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                total += path_size_bytes(&entry_path, &metadata)?;
            }
            Ok(total)
        } else {
            Ok(metadata.len())
        }
    }

    fn persisted_artifact_read_dir_error(path: &Path, err: io::Error) -> Error {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "read persisted artifact directory {}",
                path.display()
            )),
        )
    }

    fn persisted_artifact_remove_error(path: &Path, err: io::Error) -> Error {
        Error::internal_io(
            err.to_string(),
            Some(format!("remove persisted artifact {}", path.display())),
        )
    }
}
pub use persisted_cleanup::*;

mod runner_downloads {
    use super::*;

    pub fn cleanup_runner_downloads(
        options: RunnerDownloadCleanupOptions,
    ) -> Result<RunnerDownloadCleanupOutcome> {
        if options.run_id.is_some() && options.runner.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "--run-id requires --runner so cleanup stays inside one runner cache",
                options.run_id,
                None,
            ));
        }

        let root = runner_download_root(options.runner.as_deref(), options.run_id.as_deref())?;
        let plan = plan_runner_download_cleanup(&root)?;
        if options.apply && root.exists() {
            remove_runner_download_root(&root)?;
        }

        Ok(RunnerDownloadCleanupOutcome {
            dry_run: !options.apply,
            removed: options.apply && !root.exists(),
            root,
            file_count: plan.file_count,
            directory_count: plan.directory_count,
            size_bytes: plan.size_bytes,
            paths: plan.paths,
        })
    }

    fn runner_download_root(runner: Option<&str>, run_id: Option<&str>) -> Result<PathBuf> {
        let mut root = crate::core::artifacts::root()?.join("runner");
        if let Some(runner) = cleanup_path_component("runner", runner)? {
            root = root.join(runner);
        }
        if let Some(run_id) = cleanup_path_component("run_id", run_id)? {
            root = root.join(run_id);
        }
        Ok(root)
    }

    fn cleanup_path_component(name: &str, value: Option<&str>) -> Result<Option<String>> {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let path = Path::new(value);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(Error::validation_invalid_argument(
                name,
                format!("{name} must be a single path component"),
                Some(value.to_string()),
                None,
            ));
        }
        Ok(Some(value.to_string()))
    }

    fn plan_runner_download_cleanup(root: &Path) -> Result<RunnerDownloadCleanupPreview> {
        let mut plan = RunnerDownloadCleanupPreview::default();
        if !root.exists() {
            return Ok(plan);
        }

        let metadata = fs::symlink_metadata(root).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("read runner artifact cache {}", root.display())),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::validation_invalid_argument(
                "artifact_root",
                format!(
                    "runner artifact cache root must be a real directory: {}",
                    root.display()
                ),
                Some(root.display().to_string()),
                None,
            ));
        }

        collect_runner_download_cleanup(root, root, &mut plan)?;
        plan.paths.sort();
        Ok(plan)
    }

    fn collect_runner_download_cleanup(
        root: &Path,
        path: &Path,
        plan: &mut RunnerDownloadCleanupPreview,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "read runner artifact cache entry {}",
                    path.display()
                )),
            )
        })?;

        if path != root {
            plan.paths.push(relative_cleanup_path(root, path));
        }

        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            plan.directory_count += usize::from(path != root);
            for entry in
                fs::read_dir(path).map_err(|err| runner_cache_directory_error(path, err))?
            {
                let entry = entry.map_err(|err| runner_cache_directory_error(path, err))?;
                collect_runner_download_cleanup(root, &entry.path(), plan)?;
            }
        } else {
            plan.file_count += 1;
            plan.size_bytes += metadata.len();
        }

        Ok(())
    }

    fn runner_cache_directory_error(path: &Path, err: io::Error) -> Error {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "read runner artifact cache directory {}",
                path.display()
            )),
        )
    }

    fn remove_runner_download_root(root: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(root).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("read runner artifact cache {}", root.display())),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::validation_invalid_argument(
                "artifact_root",
                format!(
                    "runner artifact cache root must be a real directory: {}",
                    root.display()
                ),
                Some(root.display().to_string()),
                None,
            ));
        }
        fs::remove_dir_all(root).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("remove runner artifact cache {}", root.display())),
            )
        })
    }

    fn relative_cleanup_path(root: &Path, path: &Path) -> String {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string()
    }
}
pub use runner_downloads::*;

#[cfg(test)]
mod tests {
    //! Service-level coverage. The CLI adapter in `commands::runs` keeps the
    //! full integration coverage (JSON shape, markdown, error messages); here
    //! we exercise the standalone service surface so callers outside the CLI
    //! can rely on it without re-deriving guarantees from the command tests.

    use super::*;
    use crate::core::observation::NewRunRecord;
    use crate::test_support::with_isolated_home;
    use serde_json::Value;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn sample_run(kind: &str) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id("homeboy")
            .command(format!("homeboy {kind}"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .git_sha(Some("abc123".to_string()))
            .rig_id("studio")
            .metadata(Value::Null)
            .build()
    }

    #[test]
    fn require_run_returns_validation_error_for_missing_run() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let err = require_run(&store, "missing-run").expect_err("missing");
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("run record not found"));
        });
    }

    #[test]
    fn missing_run_guidance_prints_runner_routed_retrieval_commands() {
        let hints = missing_run_guidance_for_runner_ids("run-123", vec!["homeboy-lab".to_string()]);
        assert_eq!(
            hints,
            vec![
                "Check runner `homeboy-lab` from the controller: `homeboy runs list --runner homeboy-lab --limit 100`.",
                "Inspect run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs show run-123`.",
                "List artifacts for run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs artifacts run-123`.",
                "Export run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs export --run run-123 --output <dir>`.",
            ]
        );
    }

    #[test]
    fn list_artifacts_for_run_enriches_url_artifact_links() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(sample_run("bench")).expect("run");
            store
                .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
                .expect("record URL artifact");

            let artifacts = list_artifacts_for_run(&store, &run.id).expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].artifact_type, "url");
            // URL artifacts are enriched: public_url is filled in from the
            // recorded URL so downstream consumers don't need to re-derive it.
            assert_eq!(
                artifacts[0].public_url.as_deref(),
                Some("https://example.test/")
            );
        });
    }

    #[test]
    fn resolve_artifact_for_run_rejects_unknown_artifact_id() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(sample_run("bench")).expect("run");
            let err = resolve_artifact_for_run(&store, &run.id, "missing-artifact")
                .expect_err("missing artifact");
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("artifact record not found"));
        });
    }

    #[test]
    fn copy_local_file_artifact_writes_bytes_and_reports_metadata() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(sample_run("bench")).expect("run");
            let source = home.path().join("bench-results.json");
            std::fs::write(&source, br#"{"ok":true}"#).expect("source");
            let artifact = store
                .record_artifact(&run.id, "bench_results", &source)
                .expect("record");

            let dest = home.path().join("downloaded.json");
            let outcome =
                copy_local_file_artifact(artifact.clone(), Some(dest.clone())).expect("copy");
            assert_eq!(outcome.run_id, run.id);
            assert_eq!(outcome.artifact_id, artifact.id);
            assert_eq!(outcome.output_path, dest);
            assert_eq!(std::fs::read(&dest).expect("downloaded"), br#"{"ok":true}"#);
        });
    }

    #[test]
    fn classify_artifact_storage_recognizes_local_remote_and_metadata_only() {
        let mut artifact = ArtifactRecord {
            id: "a1".into(),
            run_id: "r1".into(),
            kind: "bench".into(),
            artifact_type: "file".into(),
            path: "/tmp/local".into(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: Value::Null,
            created_at: "2026-06-12T00:00:00Z".into(),
        };
        assert_eq!(
            classify_artifact_storage(&artifact),
            ArtifactStorage::LocalFile
        );
        artifact.artifact_type = "metadata-only".into();
        artifact.path = "metadata-only:trace.zip".into();
        assert_eq!(
            classify_artifact_storage(&artifact),
            ArtifactStorage::MetadataOnly
        );
        artifact.artifact_type = "remote_file".into();
        assert_eq!(
            classify_artifact_storage(&artifact),
            ArtifactStorage::Remote
        );
        artifact.artifact_type = "url".into();
        assert_eq!(classify_artifact_storage(&artifact), ArtifactStorage::Other);
    }
}
