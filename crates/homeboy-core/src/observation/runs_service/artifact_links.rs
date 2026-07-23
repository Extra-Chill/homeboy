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

/// Read a run's Lab remote job id from either metadata shape.
///
/// Records write the same identity two ways: the nested `lab.remote_job.id`
/// (runner-exec mirrors) and the flat `lab.remote_job_id` (bench/trace sibling
/// runs). Correlating a runner-exec run with its downstream siblings requires
/// tolerating both, otherwise a job-id-only match silently finds nothing (#9783).
fn lab_remote_job_id(run: &RunRecord) -> Option<&str> {
    let lab = run.metadata_json.get("lab")?;
    lab.pointer("/remote_job/id")
        .or_else(|| lab.get("remote_job_id"))
        .and_then(Value::as_str)
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
    // Resolve the runner-exec run's Lab job id. Prefer authoritative runner
    // evidence; fall back to the persisted metadata in either shape.
    let Some(job_id) =
        runner_evidence::with_runner_evidence(|p| p.mirrored_runner_job_identity(run))
            .map(|(_runner_id, job_id)| job_id)
            .or_else(|| lab_remote_job_id(run).map(str::to_string))
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
        // Match siblings on the same Lab job id, tolerating both metadata shapes.
        if lab_remote_job_id(&candidate) != Some(job_id.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::lab_remote_job_id;
    use crate::observation::RunRecord;

    fn run_with_metadata(metadata: serde_json::Value) -> RunRecord {
        RunRecord {
            id: "r".to_string(),
            kind: "runner-exec".to_string(),
            component_id: None,
            started_at: "2026-07-23T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: None,
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: metadata,
        }
    }

    #[test]
    fn reads_nested_remote_job_id() {
        // runner-exec mirrors persist the nested shape.
        let run = run_with_metadata(serde_json::json!({
            "lab": { "runner": { "id": "lab" }, "remote_job": { "id": "job-123" } }
        }));
        assert_eq!(lab_remote_job_id(&run), Some("job-123"));
    }

    #[test]
    fn reads_flat_remote_job_id() {
        // bench/trace sibling runs persist the flat shape.
        let run = run_with_metadata(serde_json::json!({
            "lab": { "remote_job_id": "job-123" }
        }));
        assert_eq!(lab_remote_job_id(&run), Some("job-123"));
    }

    #[test]
    fn nested_and_flat_shapes_correlate_to_the_same_job() {
        // #9783: a runner-exec run (nested) and its sibling (flat) must resolve
        // to the same job id so artifact linking finds the sibling.
        let runner = run_with_metadata(serde_json::json!({
            "lab": { "runner": { "id": "lab" }, "remote_job": { "id": "job-9783" } }
        }));
        let sibling = run_with_metadata(serde_json::json!({
            "lab": { "remote_job_id": "job-9783" }
        }));
        assert_eq!(lab_remote_job_id(&runner), lab_remote_job_id(&sibling));
    }

    #[test]
    fn missing_lineage_returns_none() {
        assert_eq!(
            lab_remote_job_id(&run_with_metadata(serde_json::json!({}))),
            None
        );
        assert_eq!(
            lab_remote_job_id(&run_with_metadata(serde_json::json!({ "lab": {} }))),
            None
        );
    }
}
