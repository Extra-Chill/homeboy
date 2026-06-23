//! Conversion helpers that translate remote runner/job payloads into observation records.

use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobStatus};
use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, RunRecord};

use super::super::{Runner, RunnerArtifactRef};
use super::artifact::{runner_artifact_token, sanitize_id_segment, RemoteArtifactToken};

pub(super) fn remote_detail_to_run_record(
    detail: &Value,
    runner: &Runner,
    job: Option<&Job>,
) -> Result<RunRecord> {
    let remote_run_id = required_str(detail, "id")?;
    let kind = required_str(detail, "kind")?;
    let local_run_id = requested_fuzz_run_id(detail).unwrap_or(remote_run_id);
    let metadata = detail.get("metadata").cloned().unwrap_or_else(|| json!({}));
    Ok(RunRecord {
        id: local_run_id.to_string(),
        kind: kind.to_string(),
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
        metadata_json: merge_lab_metadata(
            metadata,
            runner,
            job,
            detail.get("artifacts").cloned(),
            remote_run_id,
            local_run_id,
            kind,
            detail.get("cwd").cloned(),
        ),
    })
}

pub(super) fn remote_detail_artifacts(
    detail: &Value,
    runner: &Runner,
    run_id: &str,
) -> Result<Vec<ArtifactRecord>> {
    let Some(artifacts) = detail.get("artifacts").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let remote_run_id = required_str(detail, "id")?;
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
            runner_artifact_token(&runner.id, remote_run_id, &id)
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
            metadata_json: lab_artifact_metadata(
                artifact
                    .get("metadata_json")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                runner,
                remote_run_id,
                run_id,
            ),
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
    remote_run_id: &str,
    local_run_id: &str,
    kind: &str,
    remote_workspace: Option<Value>,
) -> Value {
    let mut object = metadata.as_object().cloned().unwrap_or_default();
    let mut lab = json!({
        "runner": runner_metadata(runner),
        "source_snapshot": source_snapshot_from_result(&metadata),
        "remote_artifact_manifest": artifact_manifest,
        "remote_run_id": remote_run_id,
        "local_run_id": local_run_id,
        "remote_workspace": remote_workspace,
    });
    if let Some(job) = job {
        lab["remote_job_id"] = Value::String(job.id.to_string());
        lab["remote_job_status"] = json!(job.status);
        lab["artifact_refs"] = json!(job
            .artifacts
            .iter()
            .map(RunnerArtifactRef::from)
            .collect::<Vec<_>>());
    }
    if kind == "fuzz" {
        lab["fuzz"] = json!({
            "local_run_id": local_run_id,
            "remote_run_id": remote_run_id,
            "campaign_id": metadata.get("campaign_id").cloned().unwrap_or(Value::Null),
        });
    }
    object.insert("lab".to_string(), lab);
    Value::Object(object)
}

fn lab_artifact_metadata(
    metadata: Value,
    runner: &Runner,
    remote_run_id: &str,
    local_run_id: &str,
) -> Value {
    let mut object = metadata.as_object().cloned().unwrap_or_default();
    object.insert("runner_id".to_string(), json!(runner.id));
    object.insert("remote_run_id".to_string(), json!(remote_run_id));
    object.insert("local_run_id".to_string(), json!(local_run_id));
    Value::Object(object)
}

fn requested_fuzz_run_id(detail: &Value) -> Option<&str> {
    if detail.get("kind").and_then(Value::as_str) != Some("fuzz") {
        return None;
    }
    detail
        .get("command")
        .and_then(Value::as_str)
        .and_then(fuzz_run_id_from_command)
}

pub(super) fn fuzz_run_id_from_command(command: &str) -> Option<&str> {
    let mut previous_was_run_id = false;
    for token in command.split_whitespace() {
        if previous_was_run_id {
            return (!token.is_empty()).then_some(token);
        }
        if token == "--run-id" {
            previous_was_run_id = true;
            continue;
        }
        if let Some(value) = token.strip_prefix("--run-id=") {
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

pub(super) fn runner_metadata(runner: &Runner) -> Value {
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

pub(super) fn result_summary(result: &Value) -> Value {
    json!({
        "command": result.get("command").cloned(),
        "exit_code": result.get("exit_code").cloned(),
        "output_command": result.pointer("/output/command").cloned(),
        "output_status": result.pointer("/output/status").cloned(),
    })
}

pub(super) fn source_snapshot_from_result(value: &Value) -> Option<Value> {
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

pub(super) fn remote_run_matches_job_window(summary: &Value, job: &Job) -> bool {
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

pub(super) fn explicit_observation_run_ids(result: &Value, job: &Job) -> Vec<String> {
    let mut ids = Vec::new();
    for pointer in [
        "/mirror_run_id",
        "/durable_run_id",
        "/runner_result/mirror_run_id",
        "/data/mirror_run_id",
        "/data/durable_run_id",
    ] {
        push_unique_string(&mut ids, result.pointer(pointer).and_then(Value::as_str));
    }
    for pointer in ["/observation_run_ids", "/data/observation_run_ids"] {
        if let Some(values) = result.pointer(pointer).and_then(Value::as_array) {
            for value in values {
                push_unique_string(&mut ids, value.as_str());
            }
        }
    }
    for artifact in job
        .artifacts
        .iter()
        .filter_map(|artifact| artifact.path.as_deref())
    {
        if let Ok(token) = RemoteArtifactToken::parse(artifact) {
            push_unique_string(&mut ids, Some(&token.run_id));
        }
    }
    for pointer in [
        "/artifact_refs",
        "/runner_result/artifact_refs",
        "/data/artifact_refs",
    ] {
        if let Some(artifacts) = result.pointer(pointer).and_then(Value::as_array) {
            for artifact in artifacts {
                let path = artifact.get("path").and_then(Value::as_str);
                if let Some(path) = path {
                    if let Ok(token) = RemoteArtifactToken::parse(path) {
                        push_unique_string(&mut ids, Some(&token.run_id));
                    }
                }
            }
        }
    }
    ids
}

fn push_unique_string(values: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

pub(super) fn job_status_as_run_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued | JobStatus::Running => "running",
        JobStatus::Succeeded => "pass",
        JobStatus::Failed => "fail",
        JobStatus::Cancelled => "skipped",
    }
}

pub(super) fn local_job_run_id(runner_id: &str, job_id: &str) -> String {
    format!("runner-exec-{}-{}", sanitize_id_segment(runner_id), job_id)
}

pub(super) fn ms_to_rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(0))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}
