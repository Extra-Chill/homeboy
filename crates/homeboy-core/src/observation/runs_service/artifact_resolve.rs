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
    let run = require_run(store, run_id)?;
    crate::artifacts::index_remote_published_artifact_refs_for_run(store, &run.id)?;
    let artifact = match store.get_artifact_for_run_token(&run.id, artifact_id)? {
        Some(artifact) => artifact,
        None => {
            return Err(unknown_artifact_error(store, &run.id, artifact_id));
        }
    };

    if artifact.run_id != run.id {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "artifact does not belong to requested run",
            Some(artifact_id.to_string()),
            None,
        ));
    }
    Ok(artifact)
}

/// Build a clear "artifact not found" error that lists the artifact names
/// (kinds and ids) actually recorded for the run, so callers fix the token
/// instead of guessing which name matches.
fn unknown_artifact_error(store: &ObservationStore, run_id: &str, artifact_id: &str) -> Error {
    let available = store.list_artifacts(run_id).unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    for artifact in &available {
        for token in [artifact.kind.as_str(), artifact.id.as_str()] {
            if !token.is_empty() && !names.iter().any(|name| name == token) {
                names.push(token.to_string());
            }
        }
    }
    let (problem, hints) = if names.is_empty() {
        (
            format!("artifact record not found: {artifact_id}; run `{run_id}` has no recorded artifacts yet"),
            vec!["Run `homeboy runs artifacts <run-id>` after the source command records artifacts.".to_string()],
        )
    } else {
        (
            format!(
                "artifact record not found: {artifact_id}; available artifact names for run `{run_id}`: {}",
                names.join(", ")
            ),
            vec![
                "Pass an artifact id, kind, or name from the available list.".to_string(),
                "Run `homeboy runs artifacts <run-id>` to inspect all recorded artifacts."
                    .to_string(),
            ],
        )
    };
    Error::validation_invalid_argument(
        "artifact_id",
        problem,
        Some(artifact_id.to_string()),
        Some(hints),
    )
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
    // Flush and fsync so the reported `output_path` is durably on disk
    // before we return success — never print fetch metadata for a write
    // that did not actually land.
    writer.flush().map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("flush {}", output.display())))
    })?;
    writer.sync_all().map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("sync {}", output.display())))
    })?;
    drop(writer);
    if !output.is_file() {
        return Err(Error::internal_io(
            format!(
                "artifact {} copy reported success but no file exists at {}",
                artifact.id,
                output.display()
            ),
            Some(format!("verify artifact output {}", output.display())),
        ));
    }

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
    let download = runner_evidence::with_runner_evidence(|p| {
        p.download_remote_artifact(&artifact.path, output)
    })?;
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
    if crate::execution_contract::is_remote_runner_artifact_path(&artifact.path)
        || artifact.artifact_type == "remote_file"
    {
        return ArtifactStorage::Remote;
    }
    if artifact.artifact_type == "metadata-only" {
        return ArtifactStorage::MetadataOnly;
    }
    ArtifactStorage::Other
}

/// Hydrate remote runner artifacts into local-file artifact records by
/// downloading their bytes, so JSON summarizers (e.g. matrix-artifacts)
/// can parse remote finding-packets / result packets instead of seeing
/// an opaque `remote_file` they cannot read.
///
/// `should_hydrate` selects which remote artifacts are worth a round-trip
/// (so we never pull unrelated binaries). `download` performs the actual
/// per-artifact runner round-trip — that is the live hop and is supplied
/// by [`hydrate_remote_artifacts_via_runner`] in production. Everything
/// around it (selection, record rewriting, diagnostics) is pure and unit
/// tested with a fake downloader.
///
/// Failures never abort the pass: the original (un-hydrated) record is kept
/// and a diagnostic is recorded so the operator sees exactly which packet
/// was unreachable and why.
pub fn hydrate_remote_artifacts<P, F>(
    artifacts: Vec<ArtifactRecord>,
    mut should_hydrate: P,
    mut download: F,
) -> (Vec<ArtifactRecord>, Vec<String>)
where
    P: FnMut(&ArtifactRecord) -> bool,
    F: FnMut(&ArtifactRecord) -> Result<ArtifactFetchOutcome>,
{
    let mut hydrated = Vec::with_capacity(artifacts.len());
    let mut diagnostics = Vec::new();
    for artifact in artifacts {
        let is_remote = classify_artifact_storage(&artifact) == ArtifactStorage::Remote;
        if !is_remote || !should_hydrate(&artifact) {
            hydrated.push(artifact);
            continue;
        }
        match download(&artifact) {
            Ok(outcome) => {
                let mut record = artifact;
                record.artifact_type = "file".to_string();
                record.path = outcome.output_path.display().to_string();
                if record.mime.is_none() {
                    record.mime = outcome.content_type;
                }
                if record.size_bytes.is_none() {
                    record.size_bytes = outcome.size_bytes;
                }
                if record.sha256.is_none() {
                    record.sha256 = outcome.sha256;
                }
                hydrated.push(record);
            }
            Err(err) => {
                diagnostics.push(format!(
                    "remote artifact {} could not be hydrated for summary: {}",
                    artifact.id, err.message
                ));
                hydrated.push(artifact);
            }
        }
    }
    (hydrated, diagnostics)
}

/// Hydrate remote artifacts using the live runner download path
/// (`download_remote_artifact`, the same mechanism `runs artifacts --pull`
/// uses). Convenience wrapper over [`hydrate_remote_artifacts`].
pub fn hydrate_remote_artifacts_via_runner<P>(
    artifacts: Vec<ArtifactRecord>,
    should_hydrate: P,
) -> (Vec<ArtifactRecord>, Vec<String>)
where
    P: FnMut(&ArtifactRecord) -> bool,
{
    hydrate_remote_artifacts(artifacts, should_hydrate, |artifact| {
        download_remote_artifact(artifact.clone(), None)
    })
}

/// Resolve a run id *or* a human run label to an observation run id.
///
pub fn resolve_run_id_or_label(store: &ObservationStore, run_id_or_label: &str) -> Result<String> {
    require_run(store, run_id_or_label).map(|run| run.id)
}

/// Find the first run whose human label matches `label`. A run matches when
/// its id equals the label, its command carries `--run-id <label>`, or a
/// known metadata pointer (lab label / proof provenance) equals the label.
pub fn match_run_label(runs: &[RunRecord], label: &str) -> Option<RunRecord> {
    runs.iter()
        .find(|run| super::run_lookup::run_matches_label(run, label))
        .cloned()
}
