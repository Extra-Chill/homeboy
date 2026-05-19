use std::fs;
use std::path::PathBuf;

use base64::Engine;
use serde_json::{json, Value};

use crate::core::api_jobs::{Job, JobEvent, JobStatus};
use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::paths;

use super::execution::daemon_api_get;
use super::Runner;

pub fn is_remote_runner_artifact_path(path: &str) -> bool {
    path.starts_with("runner-artifact://")
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
            encode_component(&token.run_id),
            encode_component(&token.artifact_id)
        ),
    )?;
    let body = data.get("body").unwrap_or(&data);
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

#[derive(Debug, Clone)]
struct RemoteArtifactToken {
    runner_id: String,
    run_id: String,
    artifact_id: String,
}

impl RemoteArtifactToken {
    fn parse(path: &str) -> Result<Self> {
        let token = path.strip_prefix("runner-artifact://").ok_or_else(|| {
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
                "runner artifact token must be runner-artifact://<runner>/<run>/<artifact>",
                Some(path.to_string()),
                None,
            ));
        }
        Ok(Self {
            runner_id: decode_component(runner_id),
            run_id: decode_component(run_id),
            artifact_id: decode_component(artifact_id),
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
) -> Result<Option<RunRecord>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run = mirror_job_run(&store, runner, cwd, command, job, events, result)?;
    mirror_remote_observation_runs(&store, runner, job)?;
    Ok(Some(local_job_run))
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
) -> Result<()> {
    let data = daemon_api_get(&runner.id, "/runs?limit=100")?;
    let body = data.get("body").unwrap_or(&data);
    let Some(remote_runs) = body.get("runs").and_then(Value::as_array) else {
        return Ok(());
    };
    for summary in remote_runs {
        let Some(run_id) = summary.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !remote_run_matches_job_window(summary, job) {
            continue;
        }
        let detail_data =
            daemon_api_get(&runner.id, &format!("/runs/{}", encode_component(run_id)))?;
        let detail_body = detail_data.get("body").unwrap_or(&detail_data);
        let Some(detail) = detail_body.get("run") else {
            continue;
        };
        let run = remote_detail_to_run_record(detail, runner, job)?;
        import_run_if_absent(store, &run)?;
        for artifact in remote_detail_artifacts(detail, runner, &run.id)? {
            import_artifact_if_absent(store, &artifact)?;
        }
    }
    Ok(())
}

fn import_run_if_absent(store: &ObservationStore, run: &RunRecord) -> Result<()> {
    if store.get_run(&run.id)?.is_some() {
        return Ok(());
    }
    store.import_run(run)
}

fn import_artifact_if_absent(store: &ObservationStore, artifact: &ArtifactRecord) -> Result<()> {
    if store.get_artifact(&artifact.id)?.is_some() {
        return Ok(());
    }
    store.import_artifact(artifact)
}

fn remote_detail_to_run_record(detail: &Value, runner: &Runner, job: &Job) -> Result<RunRecord> {
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
            sha256: artifact
                .get("sha256")
                .and_then(Value::as_str)
                .map(str::to_string),
            size_bytes: artifact.get("size_bytes").and_then(Value::as_i64),
            mime: artifact
                .get("mime")
                .and_then(Value::as_str)
                .map(str::to_string),
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
    job: &Job,
    artifact_manifest: Option<Value>,
) -> Value {
    let mut object = metadata.as_object().cloned().unwrap_or_default();
    object.insert(
        "lab".to_string(),
        json!({
            "runner": runner_metadata(runner),
            "remote_job_id": job.id.to_string(),
            "remote_job_status": job.status,
            "source_snapshot": source_snapshot_from_result(&metadata),
            "remote_artifact_manifest": artifact_manifest,
        }),
    );
    Value::Object(object)
}

fn runner_metadata(runner: &Runner) -> Value {
    json!({
        "id": runner.id,
        "kind": runner.kind,
        "server_id": runner.server_id,
        "workspace_root": runner.workspace_root,
        "homeboy_path": runner.homeboy_path,
        "daemon": runner.daemon,
        "artifact_policy": runner.artifact_policy,
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
    format!(
        "runner-artifact://{}/{}/{}",
        encode_component(runner_id),
        encode_component(run_id),
        encode_component(artifact_id)
    )
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

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::RunnerKind;
    use uuid::Uuid;

    fn ssh_runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("srv".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            homeboy_path: None,
            daemon: true,
            concurrency_limit: None,
            artifact_policy: None,
            env: Default::default(),
            resources: Default::default(),
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
