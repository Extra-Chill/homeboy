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
    let Some(job_id) =
        runner_evidence::with_runner_evidence(|p| p.mirrored_runner_job_identity(run))
            .map(|(_runner_id, job_id)| job_id)
            .or_else(|| {
                run.metadata_json
                    .pointer("/lab/remote_job_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
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

/// List the enriched artifact records attached to a run.
///
/// Side-effect ordering matches the CLI: refresh mirrored daemon evidence,
/// then index nested publication artifact refs, then list and enrich.
pub fn list_artifacts_for_run(
    store: &ObservationStore,
    run_id: &str,
) -> Result<Vec<ArtifactRecord>> {
    let run = require_run(store, run_id)?;
    refresh_mirrored_daemon_evidence_best_effort(&run.id);
    crate::artifacts::index_remote_published_artifact_refs_for_run(store, &run.id)?;
    let artifacts = store.list_artifacts(&run.id)?;
    Ok(enrich_artifact_links(artifacts))
}
