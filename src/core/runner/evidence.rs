use std::fs;
use std::path::PathBuf;

use base64::Engine;
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobEvent, JobStatus};
use crate::core::error::{Error, Result};
use crate::core::execution_contract::{
    decode_uri_component, encode_uri_component, EXECUTION_CONTRACT,
};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::paths;

use super::execution::{canonical_daemon_body, daemon_api_get, result_event_data};
use super::{load, Runner};

pub fn is_remote_runner_artifact_path(path: &str) -> bool {
    EXECUTION_CONTRACT.artifacts.is_runner_artifact_ref(path)
}

pub fn runner_artifact_store_token(runner_id: &str, run_id: &str, locator: &str) -> String {
    let encoded_locator = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(locator);
    runner_artifact_token(
        runner_id,
        run_id,
        &format!("artifact-store:{encoded_locator}"),
    )
}

pub(crate) fn artifact_store_locator_from_runner_artifact_id(artifact_id: &str) -> Option<String> {
    let encoded_locator = artifact_id.strip_prefix("artifact-store:")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_locator)
        .ok()?;
    String::from_utf8(bytes).ok()
}

pub fn is_retrievable_runner_artifact(path: &str) -> bool {
    RemoteArtifactToken::parse(path).is_ok()
}

pub fn is_reportable_artifact_evidence_path(path: &str) -> bool {
    is_retrievable_runner_artifact(path)
        || EXECUTION_CONTRACT.artifacts.is_metadata_only_ref(path)
        || !std::path::Path::new(path).is_absolute()
        || fs::metadata(path)
            .map(|metadata| metadata.is_file() || metadata.is_dir())
            .unwrap_or(false)
}

pub fn reportable_artifact_evidence_path(path: Option<&String>) -> Option<String> {
    path.filter(|path| is_reportable_artifact_evidence_path(path))
        .cloned()
}

pub fn download_remote_artifact(
    path: &str,
    output: Option<PathBuf>,
) -> Result<RemoteArtifactDownload> {
    let token = RemoteArtifactToken::parse(path)?;
    let data = daemon_api_get(
        &token.runner_id,
        &format!(
            "/runs/{}/artifacts/{}/content",
            encode_uri_component(&token.run_id),
            encode_uri_component(&token.artifact_id)
        ),
    )?;
    let body = canonical_daemon_body(&data, "runner artifact response")?;
    let content_base64 = body
        .get("content_base64")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::internal_unexpected("runner artifact response missing content"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64)
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("decode runner artifact content".to_string()),
            )
        })?;
    let file_name = body
        .get("filename")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&token.artifact_id);
    let output_path = output.unwrap_or_else(|| {
        paths::artifact_root()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("runner")
            .join(&token.runner_id)
            .join(&token.run_id)
            .join(file_name)
    });
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    fs::write(&output_path, bytes).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("write runner artifact {}", output_path.display())),
        )
    })?;
    Ok(RemoteArtifactDownload {
        output_path,
        content_type: body.get("mime").and_then(Value::as_str).map(str::to_string),
        size_bytes: body.get("size_bytes").and_then(Value::as_i64),
        sha256: body
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[derive(Debug)]
pub struct RemoteArtifactDownload {
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
}

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

#[derive(Debug, Clone)]
struct RemoteArtifactToken {
    runner_id: String,
    run_id: String,
    artifact_id: String,
}

impl RemoteArtifactToken {
    fn parse(path: &str) -> Result<Self> {
        let token = EXECUTION_CONTRACT
            .artifacts
            .strip_runner_artifact_scheme(path)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "artifact_id",
                    "artifact is not a runner artifact token",
                    Some(path.to_string()),
                    None,
                )
            })?;
        let mut parts = token.split('/');
        let runner_id = parts.next().unwrap_or_default();
        let run_id = parts.next().unwrap_or_default();
        let artifact_id = parts.next().unwrap_or_default();
        if runner_id.is_empty()
            || run_id.is_empty()
            || artifact_id.is_empty()
            || parts.next().is_some()
        {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "runner artifact token must be {}<runner>/<run>/<artifact>",
                    EXECUTION_CONTRACT.artifacts.runner_artifact_scheme
                ),
                Some(path.to_string()),
                None,
            ));
        }
        Ok(Self {
            runner_id: decode_uri_component(runner_id),
            run_id: decode_uri_component(run_id),
            artifact_id: decode_uri_component(artifact_id),
        })
    }
}

pub fn mirror_daemon_evidence(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run = mirror_job_run(&store, runner, cwd, command, job, events, result)?;
    mirror_remote_observation_runs(&store, runner, job)?;
    let patch = mirrored_patch_result(&store, runner, job, result.get("patch"))?;
    Ok(Some(MirroredDaemonEvidence {
        run: local_job_run,
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
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let mut run = mirror_job_run(&store, runner, cwd, command, job, events, result)?;
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
) -> Result<RunRecord> {
    let store = ObservationStore::open_initialized()?;
    mirror_job_run(&store, runner, cwd, command, job, events, &json!({}))
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
    mirror_job_run(&store, &runner, cwd, &command, &job, &events, &result)?;
    Ok(Some(mirror_remote_observation_runs(&store, &runner, &job)?))
}

pub fn mirror_connected_runner_run(run_id: &str) -> Result<Option<RunRecord>> {
    let store = ObservationStore::open_initialized()?;
    for report in super::connection::statuses()? {
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

fn mirrored_patch_result(
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

fn mirror_job_run(
    store: &ObservationStore,
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
) -> Result<RunRecord> {
    let run = RunRecord {
        id: local_job_run_id(&runner.id, &job.id.to_string()),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: ms_to_rfc3339(job.started_at_ms.unwrap_or(job.created_at_ms)),
        finished_at: job.finished_at_ms.map(ms_to_rfc3339),
        status: job_status_as_run_status(job.status).to_string(),
        command: Some(command.join(" ")),
        cwd: Some(cwd.to_string()),
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: json!({
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
) -> Result<Vec<RunRecord>> {
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

fn import_run_if_absent(store: &ObservationStore, run: &RunRecord) -> Result<()> {
    store.upsert_imported_run(run)
}

fn import_artifact_if_absent(store: &ObservationStore, artifact: &ArtifactRecord) -> Result<()> {
    if store.get_artifact(&artifact.id)?.is_some() {
        return Ok(());
    }
    store.import_artifact(artifact)
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
            "broker_artifact_retrieval": {
                "mode": "metadata_only",
                "content_available": false,
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

fn remote_detail_to_run_record(
    detail: &Value,
    runner: &Runner,
    job: Option<&Job>,
) -> Result<RunRecord> {
    let id = required_str(detail, "id")?.to_string();
    let metadata = detail.get("metadata").cloned().unwrap_or_else(|| json!({}));
    Ok(RunRecord {
        id,
        kind: required_str(detail, "kind")?.to_string(),
        component_id: detail
            .get("component_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        started_at: required_str(detail, "started_at")?.to_string(),
        finished_at: detail
            .get("finished_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: required_str(detail, "status")?.to_string(),
        command: detail
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string),
        cwd: detail
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string),
        homeboy_version: detail
            .get("homeboy_version")
            .and_then(Value::as_str)
            .map(str::to_string),
        git_sha: detail
            .get("git_sha")
            .and_then(Value::as_str)
            .map(str::to_string),
        rig_id: detail
            .get("rig_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata_json: merge_lab_metadata(metadata, runner, job, detail.get("artifacts").cloned()),
    })
}

fn remote_detail_artifacts(
    detail: &Value,
    runner: &Runner,
    run_id: &str,
) -> Result<Vec<ArtifactRecord>> {
    let Some(artifacts) = detail.get("artifacts").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut imported = Vec::new();
    for artifact in artifacts {
        let id = required_str(artifact, "id")?.to_string();
        let artifact_type = artifact
            .get("type")
            .or_else(|| artifact.get("artifact_type"))
            .and_then(Value::as_str)
            .unwrap_or("file");
        let mut mirrored_type = artifact_type.to_string();
        let path = if artifact_type == "file" {
            mirrored_type = "remote_file".to_string();
            runner_artifact_token(&runner.id, run_id, &id)
        } else {
            artifact
                .get("url")
                .or_else(|| artifact.get("path"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string()
        };
        imported.push(ArtifactRecord {
            id,
            run_id: run_id.to_string(),
            kind: artifact
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("artifact")
                .to_string(),
            artifact_type: mirrored_type,
            path,
            url: artifact
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string),
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: artifact
                .get("sha256")
                .and_then(Value::as_str)
                .map(str::to_string),
            size_bytes: artifact.get("size_bytes").and_then(Value::as_i64),
            mime: artifact
                .get("mime")
                .and_then(Value::as_str)
                .map(str::to_string),
            metadata_json: artifact
                .get("metadata_json")
                .cloned()
                .unwrap_or_else(|| json!({})),
            created_at: artifact
                .get("created_at")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        });
    }
    Ok(imported)
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        Error::internal_json(
            format!("remote run detail missing {field}"),
            Some("mirror runner evidence".to_string()),
        )
    })
}

fn merge_lab_metadata(
    metadata: Value,
    runner: &Runner,
    job: Option<&Job>,
    artifact_manifest: Option<Value>,
) -> Value {
    let mut object = metadata.as_object().cloned().unwrap_or_default();
    let mut lab = json!({
        "runner": runner_metadata(runner),
        "source_snapshot": source_snapshot_from_result(&metadata),
        "remote_artifact_manifest": artifact_manifest,
    });
    if let Some(job) = job {
        lab["remote_job_id"] = Value::String(job.id.to_string());
        lab["remote_job_status"] = json!(job.status);
    }
    object.insert("lab".to_string(), lab);
    Value::Object(object)
}

fn runner_metadata(runner: &Runner) -> Value {
    json!({
        "id": runner.id,
        "kind": runner.kind,
        "server_id": runner.server_id,
        "workspace_root": runner.workspace_root,
        "homeboy_path": runner.settings.homeboy_path,
        "daemon": runner.settings.daemon,
        "artifact_policy": runner.settings.artifact_policy,
    })
}

fn result_summary(result: &Value) -> Value {
    json!({
        "command": result.get("command").cloned(),
        "exit_code": result.get("exit_code").cloned(),
        "output_command": result.pointer("/output/command").cloned(),
        "output_status": result.pointer("/output/status").cloned(),
    })
}

fn source_snapshot_from_result(value: &Value) -> Option<Value> {
    [
        "/source_snapshot",
        "/source",
        "/metadata/source_snapshot",
        "/metadata/source",
        "/output/source_snapshot",
        "/output/source",
        "/output/metadata/source_snapshot",
        "/output/metadata/source",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).cloned())
}

fn remote_run_matches_job_window(summary: &Value, job: &Job) -> bool {
    let Some(started_at) = summary.get("started_at").and_then(Value::as_str) else {
        return false;
    };
    let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return false;
    };
    let started_ms = started_at.timestamp_millis();
    let job_start = i64::try_from(job.started_at_ms.unwrap_or(job.created_at_ms)).unwrap_or(0);
    let job_finish =
        i64::try_from(job.finished_at_ms.unwrap_or(job.updated_at_ms)).unwrap_or(i64::MAX);
    started_ms >= job_start.saturating_sub(5_000) && started_ms <= job_finish.saturating_add(5_000)
}

fn job_status_as_run_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued | JobStatus::Running => "running",
        JobStatus::Succeeded => "pass",
        JobStatus::Failed => "fail",
        JobStatus::Cancelled => "skipped",
    }
}

fn local_job_run_id(runner_id: &str, job_id: &str) -> String {
    format!("runner-exec-{}-{}", sanitize_id_segment(runner_id), job_id)
}

fn runner_artifact_token(runner_id: &str, run_id: &str, artifact_id: &str) -> String {
    EXECUTION_CONTRACT
        .artifacts
        .runner_artifact_ref(runner_id, run_id, artifact_id)
}

fn sanitize_id_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn ms_to_rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(0))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::RunnerKind;
    use crate::core::server::{RunnerPolicy, RunnerSettings};
    use uuid::Uuid;

    fn ssh_runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("srv".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings {
                daemon: true,
                ..Default::default()
            },
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        }
    }

    #[test]
    fn test_download_remote_artifact_rejects_non_runner_token() {
        let err = download_remote_artifact("/tmp/raw-file", None).expect_err("reject raw path");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn test_runner_artifact_token_round_trips_escaped_segments() {
        let token = runner_artifact_token("runner/a", "run b", "artifact:c");
        assert_eq!(token, "runner-artifact://runner%2Fa/run%20b/artifact%3Ac");
        let parsed = RemoteArtifactToken::parse(&token).expect("parse token");
        assert_eq!(parsed.runner_id, "runner/a");
        assert_eq!(parsed.run_id, "run b");
        assert_eq!(parsed.artifact_id, "artifact:c");
    }

    #[test]
    fn test_reportable_artifact_evidence_requires_local_or_retrievable_path() {
        crate::test_support::with_isolated_home(|home| {
            let local = home.path().join("artifact.json");
            fs::write(&local, b"{}").expect("artifact");

            assert!(is_reportable_artifact_evidence_path(
                &local.to_string_lossy()
            ));
            assert!(is_reportable_artifact_evidence_path(
                "runner-artifact://lab/run-1/artifact-1"
            ));
            assert!(is_reportable_artifact_evidence_path(
                "metadata-only:trace.zip"
            ));
            assert!(is_reportable_artifact_evidence_path(
                "artifacts/relative-trace.zip"
            ));
            assert!(!is_reportable_artifact_evidence_path(
                "/srv/remote-only/trace.zip"
            ));
            assert!(!is_retrievable_runner_artifact(
                "runner-artifact://missing-segments"
            ));
        });
    }

    #[test]
    fn test_mirror_daemon_evidence_persists_runner_exec_observation() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let job_id = Uuid::new_v4();
            let job = Job {
                id: job_id,
                operation: "exec".to_string(),
                status: JobStatus::Succeeded,
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_001_000,
                started_at_ms: Some(1_700_000_000_000),
                finished_at_ms: Some(1_700_000_001_000),
                event_count: 0,
                source_snapshot: None,
                stale_reason: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
            };
            let run = mirror_job_run(
                &store,
                &ssh_runner(),
                "/srv/homeboy/project",
                &["homeboy".to_string(), "bench".to_string()],
                &job,
                &[],
                &json!({"exit_code":0,"output":{"command":"bench"}}),
            )
            .expect("mirror job");
            assert_eq!(run.kind, "runner-exec");
            assert_eq!(run.status, "pass");
            assert_eq!(run.cwd.as_deref(), Some("/srv/homeboy/project"));
            assert_eq!(
                run.metadata_json["lab"]["runner"]["id"].as_str(),
                Some("lab")
            );
            assert_eq!(
                run.metadata_json["lab"]["remote_job"]["id"].as_str(),
                Some(job_id.to_string().as_str())
            );
        });
    }

    #[test]
    fn test_mirrored_patch_result_reports_accessible_artifact_token() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let runner = ssh_runner();
            let job_id = Uuid::new_v4();
            let job = Job {
                id: job_id,
                operation: "exec".to_string(),
                status: JobStatus::Succeeded,
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_001_000,
                started_at_ms: Some(1_700_000_000_000),
                finished_at_ms: Some(1_700_000_001_000),
                event_count: 0,
                source_snapshot: None,
                stale_reason: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
            };
            let run_id = format!("runner-exec-{job_id}");
            let artifact_id = format!("runner-fix-patch-{job_id}");
            store
                .import_run(&RunRecord {
                    id: run_id.clone(),
                    kind: "runner-exec".to_string(),
                    component_id: None,
                    started_at: "2026-05-16T00:00:00Z".to_string(),
                    finished_at: Some("2026-05-16T00:00:01Z".to_string()),
                    status: "pass".to_string(),
                    command: Some("homeboy runner exec".to_string()),
                    cwd: Some("/srv/project".to_string()),
                    homeboy_version: None,
                    git_sha: None,
                    rig_id: None,
                    metadata_json: json!({}),
                })
                .expect("import run");
            let token = runner_artifact_token(&runner.id, &run_id, &artifact_id);
            store
                .import_artifact(&ArtifactRecord {
                    id: artifact_id.clone(),
                    run_id: run_id.clone(),
                    kind: "lab_fix_patch".to_string(),
                    artifact_type: "remote_file".to_string(),
                    path: token.clone(),
                    url: None,
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: Some("abc".to_string()),
                    size_bytes: Some(12),
                    mime: Some("text/x-diff".to_string()),
                    metadata_json: json!({}),
                    created_at: "2026-05-16T00:00:01Z".to_string(),
                })
                .expect("import artifact");

            let patch = json!({
                "patch_artifact_id": artifact_id,
                "patch_artifact_path": "/srv/homeboy/.homeboy/artifacts/remote.diff",
            });

            let mirrored = mirrored_patch_result(&store, &runner, &job, Some(&patch))
                .expect("mirror patch")
                .expect("patch");

            assert_eq!(mirrored["patch_artifact_path"], token);
        });
    }

    #[test]
    fn test_mirrored_patch_result_fails_when_patch_artifact_was_not_mirrored() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let runner = ssh_runner();
            let job_id = Uuid::new_v4();
            let job = Job {
                id: job_id,
                operation: "exec".to_string(),
                status: JobStatus::Succeeded,
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_001_000,
                started_at_ms: Some(1_700_000_000_000),
                finished_at_ms: Some(1_700_000_001_000),
                event_count: 0,
                source_snapshot: None,
                stale_reason: None,
                target_runner_id: None,
                target_project_id: None,
                claim_id: None,
                claimed_by_runner_id: None,
                claimed_at_ms: None,
                claim_expires_at_ms: None,
                artifacts: Vec::new(),
            };
            let artifact_id = format!("runner-fix-patch-{job_id}");
            let patch = json!({
                "patch_artifact_id": artifact_id,
                "patch_artifact_path": "/srv/homeboy/.homeboy/artifacts/remote.diff",
            });

            let err = mirrored_patch_result(&store, &runner, &job, Some(&patch))
                .expect_err("missing mirror should fail");

            assert!(err
                .message
                .contains("no mirrored artifact record is available"));
        });
    }

    #[test]
    fn test_remote_file_artifacts_are_indexed_as_runner_tokens() {
        let detail = json!({
            "artifacts": [{
                "id": "artifact-1",
                "kind": "trace",
                "type": "file",
                "path": "/srv/private/trace.zip",
                "sha256": "abc",
                "size_bytes": 12,
                "mime": "application/zip",
                "created_at": "2026-05-16T00:00:00Z"
            }]
        });
        let artifacts =
            remote_detail_artifacts(&detail, &ssh_runner(), "run-1").expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, "artifact-1");
        assert_eq!(artifacts[0].artifact_type, "remote_file");
        assert_eq!(artifacts[0].path, "runner-artifact://lab/run-1/artifact-1");
    }
}
