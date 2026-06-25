use std::fs;

use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobEvent};
use crate::core::error::{Error, Result};
use crate::core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::redaction::redact_argv_display;

use super::super::execution::{canonical_daemon_body, daemon_api_get, result_event_data};
use super::super::{load, Runner};
use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
    remote_run_matches_job_window,
};
use super::tokens::is_retrievable_runner_artifact;
use super::util::{
    job_status_as_run_status, local_job_run_id, ms_to_rfc3339, result_summary, runner_metadata,
    source_snapshot_from_result,
};

#[derive(Debug)]
pub struct MirroredDaemonEvidence {
    pub run: RunRecord,
    pub patch: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RunnerJobLogSnapshot {
    pub job: Job,
    pub events: Vec<JobEvent>,
}

/// Fallback run-label token used when a command yields no usable domain token
/// (for example an all-flags argv). Kept deliberately generic so the
/// core-agnostic audit gate never sees an ecosystem literal.
pub(crate) const RUNNER_EXEC_DEFAULT_RUN_LABEL: &str = "exec";

/// Derive a domain-specific run label for a `runner exec` job.
///
/// When the caller supplies an explicit label it wins (after sanitizing); a
/// blank/whitespace-only explicit label is treated as absent. Otherwise the
/// label is derived from the command being executed \u2014 the basename of the
/// first meaningful (non flag-looking) token \u2014 so persisted runs reflect
/// the command domain instead of inheriting an unrelated workload name from an
/// adjacent invocation. The derivation is purely data-driven: it reads the
/// command argv the runner already received and never matches
/// ecosystem-specific literals, keeping homeboy core agnostic.
pub(crate) fn derive_runner_exec_run_label(command: &[String], explicit: Option<&str>) -> String {
    if let Some(label) = explicit
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(sanitize_run_label_token)
        .filter(|label| !label.is_empty())
    {
        return label;
    }

    command
        .iter()
        .map(|token| token.trim())
        .find(|token| !token.is_empty() && !token.starts_with('-'))
        .map(command_domain_token)
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| RUNNER_EXEC_DEFAULT_RUN_LABEL.to_string())
}

/// Reduce a command token to its domain stem: take the final path segment (so
/// `/usr/bin/foo` and `foo` collapse to `foo`), strip any trailing extension,
/// then sanitize into a slug-safe label.
fn command_domain_token(token: &str) -> String {
    let basename = token
        .rsplit(['/', '\\'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(token);
    let stem = basename
        .split_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(basename);
    sanitize_run_label_token(stem)
}

/// Collapse a token into a lowercase, slug-safe label: ASCII alphanumerics are
/// preserved, every other run of characters becomes a single `-`, and leading
/// and trailing separators are trimmed.
fn sanitize_run_label_token(token: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for ch in token.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.extend(ch.to_lowercase());
        } else {
            pending_separator = true;
        }
    }
    slug
}

pub fn mirror_daemon_evidence(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_label: Option<&str>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run =
        mirror_job_run(&store, runner, cwd, command, job, events, result, run_label)?;
    let remote_runs = mirror_remote_observation_runs(&store, runner, job, result)?;
    let patch = mirrored_patch_result(&store, runner, job, result.get("patch"))?;
    let primary_run = primary_mirrored_run(&remote_runs).unwrap_or(local_job_run);
    Ok(Some(MirroredDaemonEvidence {
        run: primary_run,
        patch,
    }))
}

pub fn mirror_reverse_broker_evidence(
    runner: &Runner,
    broker_url: &str,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_label: Option<&str>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let mut run = mirror_job_run(&store, runner, cwd, command, job, events, result, run_label)?;
    let artifacts = mirror_reverse_broker_artifacts(&store, runner, broker_url, &run.id, job)?;

    let mut metadata = run.metadata_json.clone();
    metadata["lab"]["reverse_broker"] = json!({
        "runner_id": runner.id.clone(),
        "job_id": job.id.to_string(),
        "broker_url": broker_url,
        "status": job.status,
        "events": events,
        "stdout": result.get("stdout").cloned().unwrap_or(Value::Null),
        "stderr": result.get("stderr").cloned().unwrap_or(Value::Null),
        "artifacts": artifacts,
    });
    run = store.update_run_metadata(&run.id, metadata)?;

    let patch = mirrored_reverse_patch_result(
        &store,
        &run.id,
        result
            .get("patch")
            .or_else(|| result.pointer("/data/patch")),
    )?;
    Ok(Some(MirroredDaemonEvidence { run, patch }))
}

pub fn mirror_daemon_job_progress(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    run_label: Option<&str>,
) -> Result<RunRecord> {
    let store = ObservationStore::open_initialized()?;
    mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        job,
        events,
        &json!({}),
        run_label,
    )
}

pub fn refresh_mirrored_daemon_evidence(run_id: &str) -> Result<Option<Vec<RunRecord>>> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    let Some((runner_id, job_id)) = mirrored_runner_job_identity(&run) else {
        return Ok(None);
    };
    let runner = load(&runner_id)?;
    let job = fetch_daemon_job(&runner_id, &job_id)?;
    let events = fetch_daemon_events(&runner_id, &job_id)?;
    let result = result_event_data(&events).unwrap_or_else(|| json!({}));
    let cwd = run.cwd.as_deref().unwrap_or("");
    let command = run
        .command
        .as_ref()
        .map(|command| vec![command.clone()])
        .unwrap_or_default();
    mirror_job_run(&store, &runner, cwd, &command, &job, &events, &result, None)?;
    Ok(Some(mirror_remote_observation_runs(
        &store, &runner, &job, &result,
    )?))
}

pub fn mirror_connected_runner_run(run_id: &str) -> Result<Option<RunRecord>> {
    let store = ObservationStore::open_initialized()?;
    for report in super::super::connection::statuses()? {
        if !report.connected {
            continue;
        }
        let runner_id = report.runner_id;
        let runner = load(&runner_id)?;
        let Ok(data) = daemon_api_get(
            &runner_id,
            &format!("/runs/{}", encode_uri_component(run_id)),
        ) else {
            continue;
        };
        let body = canonical_daemon_body(&data, "runner run detail response")?;
        let Some(detail) = body.get("run") else {
            continue;
        };
        let run = remote_detail_to_run_record(detail, &runner, None)?;
        import_run_if_absent(&store, &run)?;
        for artifact in remote_detail_artifacts(detail, &runner, &run.id)? {
            import_artifact_if_absent(&store, &artifact)?;
        }
        return Ok(Some(run));
    }
    Ok(None)
}

pub fn runner_job_log_snapshot(runner_id: &str, job_id: &str) -> Result<RunnerJobLogSnapshot> {
    Ok(RunnerJobLogSnapshot {
        job: fetch_daemon_job(runner_id, job_id)?,
        events: fetch_daemon_events(runner_id, job_id)?,
    })
}

pub fn mirrored_runner_job_identity(run: &RunRecord) -> Option<(String, String)> {
    let lab = run.metadata_json.get("lab")?;
    let runner_id = lab
        .pointer("/runner/id")
        .or_else(|| lab.get("runner_id"))
        .and_then(Value::as_str)?;
    let job_id = lab
        .pointer("/remote_job/id")
        .or_else(|| lab.get("remote_job_id"))
        .and_then(Value::as_str)?;
    Some((runner_id.to_string(), job_id.to_string()))
}

fn fetch_daemon_job(runner_id: &str, job_id: &str) -> Result<Job> {
    let data = daemon_api_get(runner_id, &format!("/jobs/{job_id}"))?;
    let body = canonical_daemon_body(&data, "daemon job response")?;
    serde_json::from_value(body["job"].clone())
        .map_err(|err| Error::internal_json(err.to_string(), Some("parse daemon job".to_string())))
}

fn fetch_daemon_events(runner_id: &str, job_id: &str) -> Result<Vec<JobEvent>> {
    let data = daemon_api_get(runner_id, &format!("/jobs/{job_id}/events"))?;
    let body = canonical_daemon_body(&data, "daemon job events response")?;
    serde_json::from_value(body["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
}

pub(super) fn mirrored_patch_result(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    patch: Option<&Value>,
) -> Result<Option<Value>> {
    let Some(patch) = patch.filter(|patch| !patch.is_null()) else {
        return Ok(None);
    };
    let Some(artifact_id) = patch.get("patch_artifact_id").and_then(Value::as_str) else {
        return Ok(Some(patch.clone()));
    };
    if artifact_id.is_empty() {
        return Ok(Some(patch.clone()));
    }

    let expected_run_id = format!("runner-exec-{}", job.id);
    let artifact = store
        .get_artifact(artifact_id)?
        .filter(|artifact| artifact.run_id == expected_run_id)
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner capture-patch artifact {artifact_id} was reported by remote run {expected_run_id}, but no mirrored artifact record is available"
            ))
        })?;

    let accessible = is_retrievable_runner_artifact(&artifact.path)
        || fs::metadata(&artifact.path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false);
    if !accessible {
        return Err(Error::internal_unexpected(format!(
            "runner capture-patch artifact {artifact_id} was mirrored for runner {}, but its mirrored path is not accessible: {}",
            runner.id, artifact.path
        )));
    }

    let mut patched = patch.clone();
    if let Some(object) = patched.as_object_mut() {
        object.insert(
            "patch_artifact_path".to_string(),
            Value::String(artifact.path),
        );
    }
    Ok(Some(patched))
}

pub(super) fn mirror_job_run(
    store: &ObservationStore,
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_label: Option<&str>,
) -> Result<RunRecord> {
    let run_label = derive_runner_exec_run_label(command, run_label);
    let run = RunRecord {
        id: local_job_run_id(&runner.id, &job.id.to_string()),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: ms_to_rfc3339(job.started_at_ms.unwrap_or(job.created_at_ms)),
        finished_at: job.finished_at_ms.map(ms_to_rfc3339),
        status: job_status_as_run_status(job.status).to_string(),
        command: Some(redact_argv_display(command)),
        cwd: Some(cwd.to_string()),
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: json!({
            "run_label": run_label,
            "lab": {
                "runner": runner_metadata(runner),
                "remote_job": job,
                "remote_events": events,
                "result_summary": result_summary(result),
                "source_snapshot": source_snapshot_from_result(result),
            }
        }),
    };
    import_run_if_absent(store, &run)?;
    store.get_run(&run.id)?.ok_or_else(|| {
        Error::internal_unexpected(format!(
            "mirrored runner run {} but could not read it back",
            run.id
        ))
    })
}

fn mirror_remote_observation_runs(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    result: &Value,
) -> Result<Vec<RunRecord>> {
    let explicit_run_ids = explicit_observation_run_ids(result, job);
    if !explicit_run_ids.is_empty() {
        return mirror_remote_observation_runs_by_id(store, runner, job, &explicit_run_ids);
    }

    let data = daemon_api_get(&runner.id, "/runs?limit=100")?;
    let body = canonical_daemon_body(&data, "runner runs response")?;
    let Some(remote_runs) = body.get("runs").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut mirrored = Vec::new();
    for summary in remote_runs {
        let Some(run_id) = summary.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !remote_run_matches_job_window(summary, job) {
            continue;
        }
        let detail_data = daemon_api_get(
            &runner.id,
            &format!("/runs/{}", encode_uri_component(run_id)),
        )?;
        let detail_body = canonical_daemon_body(&detail_data, "runner run detail response")?;
        let Some(detail) = detail_body.get("run") else {
            continue;
        };
        let run = remote_detail_to_run_record(detail, runner, Some(job))?;
        import_run_if_absent(store, &run)?;
        for artifact in remote_detail_artifacts(detail, runner, &run.id)? {
            import_artifact_if_absent(store, &artifact)?;
        }
        mirrored.push(run);
    }
    Ok(mirrored)
}

fn mirror_remote_observation_runs_by_id(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run_ids: &[String],
) -> Result<Vec<RunRecord>> {
    let mut mirrored = Vec::new();
    for run_id in run_ids {
        let detail_data = daemon_api_get(
            &runner.id,
            &format!("/runs/{}", encode_uri_component(run_id)),
        )?;
        let detail_body = canonical_daemon_body(&detail_data, "runner run detail response")?;
        let Some(detail) = detail_body.get("run") else {
            continue;
        };
        let run = remote_detail_to_run_record(detail, runner, Some(job))?;
        import_run_if_absent(store, &run)?;
        for artifact in remote_detail_artifacts(detail, runner, &run.id)? {
            import_artifact_if_absent(store, &artifact)?;
        }
        mirrored.push(run);
    }
    Ok(mirrored)
}

fn import_run_if_absent(store: &ObservationStore, run: &RunRecord) -> Result<()> {
    store.upsert_imported_run(run)
}

fn import_artifact_if_absent(store: &ObservationStore, artifact: &ArtifactRecord) -> Result<()> {
    if store.get_artifact(&artifact.id)?.is_some() {
        return Ok(());
    }
    store.import_artifact(artifact)
}

pub(super) fn primary_mirrored_run(remote_runs: &[RunRecord]) -> Option<RunRecord> {
    remote_runs.iter().find(|run| run.kind == "fuzz").cloned()
}

fn mirror_reverse_broker_artifacts(
    store: &ObservationStore,
    runner: &Runner,
    broker_url: &str,
    run_id: &str,
    job: &Job,
) -> Result<Vec<ArtifactRecord>> {
    let mut mirrored = Vec::new();
    for artifact in &job.artifacts {
        let record = reverse_broker_artifact_record(runner, broker_url, run_id, job, artifact);
        import_artifact_if_absent(store, &record)?;
        mirrored.push(record);
    }
    Ok(mirrored)
}

fn reverse_broker_artifact_record(
    runner: &Runner,
    broker_url: &str,
    run_id: &str,
    job: &Job,
    artifact: &JobArtifactMetadata,
) -> ArtifactRecord {
    ArtifactRecord {
        id: artifact.id.clone(),
        run_id: run_id.to_string(),
        kind: artifact
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("reverse_broker_artifact")
            .to_string(),
        artifact_type: "metadata".to_string(),
        path: EXECUTION_CONTRACT.artifacts.metadata_only_ref(&artifact.id),
        url: artifact.url.clone(),
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: artifact.sha256.clone(),
        size_bytes: artifact
            .size_bytes
            .and_then(|size| i64::try_from(size).ok()),
        mime: artifact.mime.clone(),
        metadata_json: json!({
            "runner_id": runner.id.clone(),
            "job_id": job.id.to_string(),
            "broker_url": broker_url,
            "name": artifact.name.clone(),
            "remote_path": artifact.path.clone(),
            "url": artifact.url.clone(),
            "broker_artifact_metadata_path": format!(
                "/runner/jobs/{}/artifacts/{}",
                job.id,
                encode_uri_component(&artifact.id)
            ),
            "content_available": false,
            "broker_artifact_retrieval": {
                "mode": "metadata_only",
                "content_available": false,
                "content_url": null,
                "fetch_command": null,
                "hint": "Reverse broker artifact mirroring preserves metadata only; broker byte retrieval is a future content-proxy slice."
            },
            "metadata": artifact.metadata.clone(),
        }),
        created_at: ms_to_rfc3339(job.finished_at_ms.unwrap_or(job.updated_at_ms)),
    }
}

fn mirrored_reverse_patch_result(
    store: &ObservationStore,
    run_id: &str,
    patch: Option<&Value>,
) -> Result<Option<Value>> {
    let Some(patch) = patch.filter(|patch| !patch.is_null()) else {
        return Ok(None);
    };
    let Some(artifact_id) = patch.get("patch_artifact_id").and_then(Value::as_str) else {
        return Ok(Some(patch.clone()));
    };
    if artifact_id.is_empty() {
        return Ok(Some(patch.clone()));
    }
    let Some(artifact) = store
        .get_artifact(artifact_id)?
        .filter(|artifact| artifact.run_id == run_id)
    else {
        return Ok(Some(patch.clone()));
    };
    let mut patched = patch.clone();
    if let Some(object) = patched.as_object_mut() {
        object.insert(
            "patch_artifact_path".to_string(),
            Value::String(artifact.path),
        );
    }
    Ok(Some(patched))
}

#[cfg(test)]
mod run_label_tests {
    use super::{derive_runner_exec_run_label, RUNNER_EXEC_DEFAULT_RUN_LABEL};

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|token| token.to_string()).collect()
    }

    #[test]
    fn derives_run_label_from_command_domain_when_no_explicit_label() {
        let command = argv(&["psql", "--no-align", "-c", "select 1"]);
        assert_eq!(derive_runner_exec_run_label(&command, None), "psql");
    }

    #[test]
    fn derives_run_label_from_executable_basename_and_strips_extension() {
        let command = argv(&["/usr/local/bin/profile.sh", "--matrix"]);
        assert_eq!(derive_runner_exec_run_label(&command, None), "profile");
    }

    #[test]
    fn skips_leading_flags_when_deriving_run_label() {
        // Leading flag-looking tokens are not a command domain; the first
        // meaningful token wins so evidence reflects what actually ran.
        let command = argv(&["--login", "diagnose", "system"]);
        assert_eq!(derive_runner_exec_run_label(&command, None), "diagnose");
    }

    #[test]
    fn explicit_label_is_used_and_sanitized() {
        let command = argv(&["psql", "-c", "select 1"]);
        assert_eq!(
            derive_runner_exec_run_label(&command, Some("DB Query Profile")),
            "db-query-profile"
        );
    }

    #[test]
    fn blank_explicit_label_falls_back_to_command_derivation() {
        let command = argv(&["bench", "--iterations", "10"]);
        assert_eq!(derive_runner_exec_run_label(&command, Some("   ")), "bench");
    }

    #[test]
    fn unrelated_prior_workload_name_is_not_reused_as_a_generic_fallback() {
        // A stale workload-family name from an adjacent invocation must never be
        // inherited: with no explicit label the run label comes from the command
        // itself, and an all-flags command falls back to a generic token rather
        // than any workload-specific name (#6362).
        let stale_workload = "woo-db-api-rest-query-profile-20240101";
        let command = argv(&["status"]);
        let derived = derive_runner_exec_run_label(&command, None);
        assert_eq!(derived, "status");
        assert_ne!(derived, stale_workload);

        let all_flags = argv(&["--verbose", "--json"]);
        assert_eq!(
            derive_runner_exec_run_label(&all_flags, None),
            RUNNER_EXEC_DEFAULT_RUN_LABEL
        );
        assert_ne!(
            derive_runner_exec_run_label(&all_flags, None),
            stale_workload
        );
    }

    #[test]
    fn empty_command_falls_back_to_generic_default_label() {
        assert_eq!(
            derive_runner_exec_run_label(&[], None),
            RUNNER_EXEC_DEFAULT_RUN_LABEL
        );
    }
}
