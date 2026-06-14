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

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{ArtifactRecord, ObservationStore, RunListFilter, RunRecord};
use crate::core::artifact_links::{cached_validated_viewer_links, public_artifact_url};
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
}

/// Look up a run and surface a stable validation error when it doesn't
/// exist. Used by every observation command that takes a `run_id`.
pub fn require_run(store: &ObservationStore, run_id: &str) -> Result<RunRecord> {
    store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })
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

/// Enrich a single artifact record with public/viewer link metadata.
///
/// Mirrors the original CLI helper exactly: derive a public URL (from
/// stored artifact metadata or by treating the artifact path as the URL
/// for `url`-typed artifacts), then resolve any cached viewer links.
pub fn enrich_artifact_link(mut artifact: ArtifactRecord) -> ArtifactRecord {
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
    let Some((_runner_id, job_id)) = crate::core::runners::mirrored_runner_job_identity(run) else {
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

/// Outcome of `get_artifact_bytes` describing where the bytes were written.
pub enum ArtifactGetSource {
    /// Bytes copied from a locally-recorded file artifact.
    Local,
    /// Bytes fetched from a remote runner cache.
    Remote,
}

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

/// Storage classes recognized by [`classify_artifact_storage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStorage {
    LocalFile,
    Remote,
    MetadataOnly,
    Other,
}

/// Convenience accessor: returns the resolved local path used to display
/// `homeboy runs artifacts` rows. Kept so other consumers don't need to
/// reach into `ArtifactRecord` for path formatting.
#[allow(dead_code)]
pub fn artifact_display_path(artifact: &ArtifactRecord) -> &Path {
    Path::new(&artifact.path)
}

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
