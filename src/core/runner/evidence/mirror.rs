use std::fs;

use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobEvent};
use crate::core::error::{Error, Result};
use crate::core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::redaction::redact_argv_display;
use crate::core::runner::agent_task_lifecycle_event::{
    agent_task_run_plan_lifecycle_event_from_job_events,
    agent_task_run_plan_lifecycle_event_from_value,
};

use super::super::execution::{canonical_daemon_body, daemon_api_get, result_event_data};
use super::super::{load, Runner};
use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
    remote_run_matches_job_window,
};
use super::tokens::is_retrievable_runner_artifact;
use super::util::{
    job_status_as_run_status, local_job_run_id, ms_to_rfc3339, result_summary,
    runner_exec_run_label, runner_metadata, source_snapshot_from_result,
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

pub fn mirror_daemon_evidence(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_id: Option<&str>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run = mirror_job_run(&store, runner, cwd, command, job, events, result, run_id)?;
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
    run_id: Option<&str>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let mut run = mirror_job_run(&store, runner, cwd, command, job, events, result, run_id)?;
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
    run_id: Option<&str>,
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
        run_id,
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
    run_id: Option<&str>,
) -> Result<RunRecord> {
    let inferred_label = runner_exec_run_label(command);
    let agent_task_lifecycle_event = agent_task_run_plan_lifecycle_event_from_value(result)
        .or_else(|| agent_task_run_plan_lifecycle_event_from_job_events(Some(events)))
        .and_then(|event| serde_json::to_value(event).ok());
    let mut lab = json!({
        "runner": runner_metadata(runner),
        "remote_job": job,
        "remote_events": events,
        "result_summary": result_summary(result),
        "source_snapshot": source_snapshot_from_result(result),
        "run_label": inferred_label,
        "explicit_run_id": run_id,
    });
    if let Some(event) = agent_task_lifecycle_event {
        lab["agent_task_lifecycle_event"] = event;
    }
    let run = RunRecord {
        id: run_id
            .map(str::to_string)
            .unwrap_or_else(|| local_job_run_id(&runner.id, &job.id.to_string(), &inferred_label)),
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
        metadata_json: json!({ "lab": lab }),
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
