use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use reqwest::header;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Runner, RunnerKind};
use homeboy_core::api_jobs::{
    Job, JobArtifactMetadata, JobEvent, JobEventKind, JobStatus, RemoteRunnerObservationRunDetail,
    RunnerJobLifecycleMetadata, RunnerJobProjection,
};
use homeboy_core::error::{Error, ErrorCode};
use homeboy_core::observation::{
    runs_service, ArtifactRecord, NewRunRecord, ObservationStore, RunRecord,
};
use homeboy_core::server::{RunnerPolicy, RunnerSettings};

use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
};
use super::download::{content_disposition_filename, download_remote_artifact, resolve_placement};
use super::mirror::{
    bounded_remote_events, controller_artifact_metadata, import_mirrored_artifact_with_downloader,
    mirror_daemon_evidence, mirror_job_run, mirror_remote_observation_runs_by_id_with,
    mirror_remote_observation_runs_by_id_with_downloader, mirror_reverse_broker_evidence,
    mirror_terminal_job_artifacts_with, mirrored_patch_result, primary_mirrored_run,
    refresh_mirrored_daemon_evidence, refresh_mirrored_daemon_evidence_with,
    MIRRORED_REMOTE_EVENT_LIMIT, MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT,
};
use super::tokens::{
    is_reportable_artifact_evidence_path, is_retrievable_runner_artifact, runner_artifact_token,
    RemoteArtifactToken,
};
use super::util::{fuzz_run_id_from_command, runner_exec_run_label};

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

fn terminal_runner_job() -> Job {
    Job {
        id: Uuid::new_v4(),
        operation: "runner.exec".to_string(),
        status: JobStatus::Succeeded,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
        started_at_ms: Some(1_700_000_000_000),
        finished_at_ms: Some(1_700_000_001_000),
        event_count: 0,
        source_snapshot: None,
        path_materialization_plan: None,
        stale_reason: None,
        daemon_lease_id: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: Vec::new(),
        runner_job_projection: None,
    }
}

fn reverse_broker_fixture(
    job: Job,
    events: Vec<JobEvent>,
) -> (String, mpsc::Sender<()>, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("broker listener");
    let url = format!("http://{}", listener.local_addr().expect("broker address"));
    let (shutdown, shutdown_signal) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut paths = Vec::new();
        listener.set_nonblocking(true).expect("nonblocking broker");
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if shutdown_signal.try_recv().is_ok() {
                        break;
                    }
                    thread::yield_now();
                    continue;
                }
                Err(error) => panic!("broker request: {error}"),
            };
            stream
                .set_nonblocking(false)
                .expect("blocking request stream");
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).expect("request byte");
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).expect("request text");
            let content_length = request
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            if content_length > 0 {
                let mut body = vec![0; content_length];
                stream.read_exact(&mut body).expect("request body");
            }
            let path = request
                .lines()
                .next()
                .expect("request line")
                .split_whitespace()
                .nth(1)
                .expect("request path")
                .to_string();
            let body = if path.ends_with("/events") {
                json!({ "events": events.clone() })
            } else if path == "/runner/jobs/reconcile" {
                json!({})
            } else {
                json!({ "job": job.clone() })
            };
            paths.push(path);
            let body = json!({ "success": true, "data": { "body": body } }).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            )
            .expect("broker response");
        }
        paths
    });
    (url, shutdown, handle)
}

#[test]
fn controller_terminal_metadata_uses_exact_visual_artifact_shape_and_validates_bytes() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let prior_public_base = std::env::var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL").ok();
        std::env::set_var(
            "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL",
            "https://artifacts.example.test",
        );
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(NewRunRecord::builder("runner-exec").build())
            .expect("run");
        let source_dir = home.path().join("visual-compare/37-art-gallery-exhibition");
        fs::create_dir_all(&source_dir).expect("visual artifact directory");
        for (id, name, bytes) in [
            ("source", "source.png", b"source bytes".as_slice()),
            ("candidate", "candidate.png", b"candidate bytes".as_slice()),
            ("diff", "diff.png", b"diff bytes".as_slice()),
        ] {
            let path = source_dir.join(name);
            fs::write(&path, bytes).expect("visual artifact");
            store
                .record_artifact_with_id(&run.id, "visual_compare", &path, id, json!({}))
                .expect("controller artifact");
        }

        let metadata = controller_artifact_metadata(&[run.clone()]).expect("terminal metadata");
        assert_eq!(
            metadata
                .iter()
                .map(|artifact| artifact.id.as_str())
                .collect::<Vec<_>>(),
            ["source", "candidate", "diff"]
        );
        for artifact in &metadata {
            assert!(artifact
                .path
                .as_deref()
                .is_some_and(|path| path.contains(&run.id)));
            assert_eq!(artifact.mime.as_deref(), Some("image/png"));
            assert!(artifact.size_bytes.is_some());
            assert!(artifact
                .sha256
                .as_deref()
                .is_some_and(|sha| !sha.is_empty()));
            assert_eq!(
                artifact.url.as_deref(),
                Some(
                    format!(
                        "https://artifacts.example.test/runs/{}/artifacts/{}",
                        run.id, artifact.id
                    )
                    .as_str()
                )
            );
        }

        let source = store
            .get_artifact("source")
            .expect("source lookup")
            .expect("source");
        fs::write(&source.path, b"corrupt data").expect("corrupt controller bytes");
        let error = controller_artifact_metadata(&[run]).expect_err("checksum mismatch");
        assert_eq!(error.details["field"], "artifact.sha256");
        match prior_public_base {
            Some(value) => std::env::set_var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL", value),
            None => std::env::remove_var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"),
        }
    });
}

#[test]
fn controller_terminal_metadata_keeps_fetch_fallback_without_public_origin() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let prior_public_base = std::env::var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL").ok();
        std::env::remove_var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(NewRunRecord::builder("runner-exec").build())
            .expect("run");
        let path = home.path().join("report.txt");
        fs::write(&path, b"controller bytes").expect("artifact");
        store
            .record_artifact_with_id(&run.id, "report", &path, "report", json!({}))
            .expect("controller artifact");

        let metadata = controller_artifact_metadata(&[run.clone()]).expect("terminal metadata");
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].id, "report");
        assert_eq!(metadata[0].url, None);
        assert_eq!(
            metadata[0].metadata.as_ref().expect("metadata")["fetch_command"],
            format!("homeboy runs artifact get {} report -o <path>", run.id)
        );

        match prior_public_base {
            Some(value) => std::env::set_var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL", value),
            None => std::env::remove_var("HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"),
        }
    });
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

/// A traversal-shaped `filename` from the daemon body, on the relay transport.
///
/// #10586: `{"filename": "../../../../../../root/.ssh/authorized_keys"}` used
/// to be joined straight onto the cache directory, `create_dir_all` made the
/// intermediate directories, and `fs::write` put attacker-controlled bytes
/// wherever the daemon asked. It is now reduced to one component in the cache.
#[test]
fn test_download_placement_sanitizes_a_traversal_filename() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let token = RemoteArtifactToken::parse("runner-artifact://lab/run-1/artifact-1")
            .expect("parse token");
        let artifact_root = homeboy_core::paths::artifact_root().expect("artifact root");
        let cache = artifact_root.join("runner").join("lab").join("run-1");

        for hostile in [
            "../../../../../../root/.ssh/authorized_keys",
            "/root/.ssh/authorized_keys",
            "..",
            "../../etc/passwd",
        ] {
            let placement =
                resolve_placement(&token, None, Some(hostile)).expect("placement resolves");
            assert_eq!(
                placement.output_path.parent(),
                Some(cache.as_path()),
                "{hostile} escaped to {}",
                placement.output_path.display()
            );
            assert_eq!(placement.cache_dir.as_deref(), Some(cache.as_path()));
            assert!(!placement.file_name.contains('/'));
            // The reported artifact-ref name is the name on disk, never the
            // remote's, so no downstream consumer can rebuild the escape.
            assert_eq!(
                placement
                    .output_path
                    .file_name()
                    .expect("file name")
                    .to_string_lossy(),
                placement.file_name.as_str()
            );
        }
    });
}

/// The same defect through the token instead of the body.
///
/// #10586: `RemoteArtifactToken::parse` splits on `/` (which looks like a
/// containment check) and *then* percent-decodes, so `%2E%2E%2F` becomes `../`
/// after the only check. A containment check on the encoded form is no check.
/// The decoded ids are now rejected outright — they are identifiers, and
/// silently rewriting one would put bytes somewhere unpredictable.
#[test]
fn test_download_placement_rejects_a_percent_encoded_traversal_token() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let token = RemoteArtifactToken::parse(
            "runner-artifact://lab/%2E%2E%2F%2E%2E%2F%2E%2E%2Froot%2F.ssh/authorized_keys",
        )
        .expect("token still parses; that is the point");
        // The parser hands the writer an already-decoded traversal.
        assert_eq!(token.run_id, "../../../root/.ssh");

        let error = resolve_placement(&token, None, Some("authorized_keys"))
            .expect_err("the writer must refuse it");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(
            error.message.contains("single path component"),
            "{}",
            error.message
        );

        // And through the runner id, which reaches the same join.
        let runner_token =
            RemoteArtifactToken::parse("runner-artifact://%2E%2E%2F%2E%2E%2Fetc/run-1/artifact-1")
                .expect("parse token");
        assert_eq!(runner_token.runner_id, "../../etc");
        let error = resolve_placement(&runner_token, None, Some("passwd"))
            .expect_err("the writer must refuse it");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
    });
}

/// An explicit `--output` is the caller's own path and is not relocated into
/// the cache — and it is not tagged, because homeboy does not reclaim it.
#[test]
fn test_download_placement_honours_an_explicit_output_without_tagging_it() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let token = RemoteArtifactToken::parse("runner-artifact://lab/run-1/artifact-1")
            .expect("parse token");
        let explicit = home.path().join("elsewhere").join("report.json");

        let placement =
            resolve_placement(&token, Some(explicit.clone()), Some("../../evil")).expect("resolve");

        assert_eq!(placement.output_path, explicit);
        assert!(placement.cache_dir.is_none());
        assert_eq!(placement.file_name, "report.json");
    });
}

#[test]
fn test_content_disposition_filename_parses_quoted_attachment_name() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_static("attachment; filename=\"report.json\""),
    );

    assert_eq!(
        content_disposition_filename(&headers).as_deref(),
        Some("report.json")
    );
}

/// The direct-SSH transport's file name comes from a header the daemon writes,
/// and the header parser applies no shape check at all — it splits on `;`,
/// strips `filename=`, and trims quotes. Containment therefore has to live in
/// the writer, and this asserts both halves of that split (#10586).
#[test]
fn test_content_disposition_traversal_is_neutralized_by_the_writer() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_static("attachment; filename=\"../../../../etc/cron.d/x\""),
        );
        let parsed = content_disposition_filename(&headers).expect("filename");
        assert_eq!(parsed, "../../../../etc/cron.d/x");

        let token = RemoteArtifactToken::parse("runner-artifact://lab/run-1/artifact-1")
            .expect("parse token");
        let placement = resolve_placement(&token, None, Some(&parsed)).expect("resolve");
        let cache = homeboy_core::paths::artifact_root()
            .expect("artifact root")
            .join("runner")
            .join("lab")
            .join("run-1");
        assert_eq!(placement.output_path.parent(), Some(cache.as_path()));
        assert_eq!(placement.file_name, "etc_cron.d_x");
    });
}

#[test]
fn test_reportable_artifact_evidence_requires_local_or_retrievable_path() {
    homeboy_core::test_support::with_isolated_home(|home| {
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
    homeboy_core::test_support::with_isolated_home(|_| {
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
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };
        let lifecycle_event = json!({
            "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
            "identity": {
                "runner_id": "lab",
                "runner_job_id": job_id.to_string(),
                "run_id": "run-typed"
            },
            "aggregate": {
                "schema": "homeboy/agent-task-aggregate/v1",
                "plan_id": "plan-from-result",
                "status": "succeeded",
                "totals": {"skipped": 0, "succeeded": 1, "failed": 0},
                "outcomes": []
            }
        });
        let events = vec![JobEvent {
            sequence: 1,
            job_id,
            kind: JobEventKind::Result,
            timestamp_ms: 1_700_000_001_000,
            message: None,
            data: Some(json!({
                "data": {
                    "agent_task_lifecycle_event": lifecycle_event
                }
            })),
        }];
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/srv/homeboy/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &events,
            &json!({"exit_code":0,"output":{"command":"bench"}}),
            None,
            Some(
                &homeboy_core::notification_route::NotificationRoute::new(
                    "extension",
                    "opaque-origin-route",
                )
                .expect("route"),
            ),
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
        assert_eq!(
            run.metadata_json["lab"]["agent_task_lifecycle_event"]["aggregate"]["plan_id"].as_str(),
            Some("plan-from-result")
        );
        assert_eq!(
            run.metadata_json["notification_route"]["route"],
            "opaque-origin-route"
        );
    });
}

#[test]
fn direct_failure_mirror_projects_bounded_typed_diagnostics_without_artifacts() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut job = terminal_runner_job();
        job.status = JobStatus::Failed;
        let mirrored = mirror_daemon_evidence(
            &ssh_runner(),
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &json!({
                "exit_code": 2,
                "stderr": "secret=redacted\nvalidation failed",
                "phase": "path_materialization",
                "signal": "SIGTERM",
                "error": {
                    "code": "validation.invalid_argument",
                    "message": "required path is missing",
                    "details": { "field": "path" }
                },
                "data": { "execution_record": { "orchestration_provenance": {
                    "job_command_binary": { "version": "0.321.1" }
                }}}
            }),
            None,
            None,
        )
        .expect("direct failure mirror")
        .expect("mirrored failure");

        let failure = &mirrored.run.metadata_json["lab"]["failure"];
        assert_eq!(failure["failure_code"], "validation.invalid_argument");
        assert_eq!(failure["phase"], "path_materialization");
        assert_eq!(failure["exit_code"], 2);
        assert_eq!(mirrored.run.metadata_json["exit_code"], 2);
        assert_eq!(mirrored.run.homeboy_version.as_deref(), Some("0.321.1"));
        assert_eq!(failure["signal"], "SIGTERM");
        assert_eq!(
            failure["stderr_tail"],
            "secret=[REDACTED]\nvalidation failed"
        );
        assert_eq!(failure["artifact_refs"], json!([]));
        assert!(failure["stderr_sha256"].as_str().is_some());
        assert_eq!(
            failure["runner_job_logs_command"],
            format!("homeboy runner job logs lab {}", job.id)
        );
    });
}

#[test]
fn failed_job_without_terminal_exit_code_projects_root_failure_fallback() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        job.status = JobStatus::Failed;
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &json!({ "stderr": "failed before result" }),
            None,
            None,
        )
        .expect("mirror failed job");
        assert_eq!(run.metadata_json["exit_code"], 1);
        assert_eq!(run.metadata_json["lab"]["failure"]["exit_code"], 1);
    });
}

#[test]
fn synthetic_runner_run_identity_is_stable_across_repeated_progress_mirrors() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let runner = ssh_runner();
        let job = terminal_runner_job();
        let command = vec!["homeboy".to_string(), "test".to_string()];

        let first = mirror_job_run(
            &store,
            &runner,
            "/runner/project",
            &command,
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("first progress mirror");
        let second = mirror_job_run(
            &store,
            &runner,
            "/runner/project",
            &command,
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("repeated progress mirror");

        assert_eq!(first.id, second.id);
        assert_eq!(store.list_artifacts(&first.id).expect("artifacts").len(), 0);
    });
}

#[test]
fn synthetic_progress_mirror_is_upserted_to_terminal_failure_after_restart() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let runner = ssh_runner();
        let command = vec!["homeboy".to_string(), "bench".to_string()];
        let mut progress = terminal_runner_job();
        progress.status = JobStatus::Running;
        progress.finished_at_ms = None;
        let running = mirror_job_run(
            &store,
            &runner,
            "/runner/project",
            &command,
            &progress,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("progress mirror");

        let mut terminal = progress.clone();
        terminal.status = JobStatus::Failed;
        terminal.finished_at_ms = Some(terminal.updated_at_ms);
        let failed = mirror_job_run(
            &store,
            &runner,
            "/runner/project",
            &command,
            &terminal,
            &[],
            &json!({ "exit_code": 2, "stderr": "token=secret" }),
            None,
            None,
        )
        .expect("terminal mirror after restart");

        assert_eq!(running.id, failed.id);
        assert_eq!(failed.status, "fail");
        assert_eq!(failed.metadata_json["lab"]["failure"]["exit_code"], 2);
        assert_eq!(
            failed.metadata_json["lab"]["failure"]["stderr_tail"],
            "token=[REDACTED]"
        );
    });
}

#[test]
fn runner_exec_matrix_summary_run_names_come_from_command_domain() {
    homeboy_core::test_support::with_isolated_home(|_| {
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
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };
        let command = [
            "homeboy".to_string(),
            "trace".to_string(),
            "matrix".to_string(),
            "summary".to_string(),
            "--json".to_string(),
        ];

        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/srv/homeboy/static-site-importer",
            &command,
            &job,
            &[],
            &json!({"exit_code":0}),
            None,
            None,
        )
        .expect("mirror job");

        assert_eq!(runner_exec_run_label(&command), "trace-matrix-summary");
        assert!(run.id.starts_with("runner-exec-trace-matrix-summary-lab-"));
        assert!(!run.id.contains("woo-db-api-rest-query-profile"));
        assert_eq!(
            run.metadata_json["lab"]["run_label"].as_str(),
            Some("trace-matrix-summary")
        );
    });
}

#[test]
fn mirrored_remote_events_keep_lifecycle_summary_without_stream_payloads() {
    let job_id = Uuid::new_v4();
    let events = (0..(MIRRORED_REMOTE_EVENT_LIMIT + 5))
        .map(|sequence| JobEvent {
            sequence: sequence as u64,
            job_id,
            kind: JobEventKind::Result,
            timestamp_ms: 1_700_000_000_000,
            message: Some("m".repeat(MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT + 100)),
            data: Some(json!({ "unbounded": "x".repeat(1024 * 1024) })),
        })
        .collect::<Vec<_>>();
    let bounded = bounded_remote_events(&events);

    assert_eq!(bounded.len(), MIRRORED_REMOTE_EVENT_LIMIT);
    assert_eq!(bounded[0]["sequence"], 5);
    assert!(bounded.iter().all(|event| event.get("data").is_none()));
    assert!(bounded.iter().all(|event| {
        event["message"]
            .as_str()
            .is_some_and(|message| message.len() == MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT)
    }));
}

#[test]
fn runner_exec_explicit_run_id_overrides_inferred_name() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let job_id = Uuid::new_v4();
        let job = Job {
            id: job_id,
            operation: "exec".to_string(),
            status: JobStatus::Running,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            started_at_ms: Some(1_700_000_000_000),
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };

        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/srv/homeboy/static-site-importer",
            &[
                "homeboy".to_string(),
                "runs".to_string(),
                "list".to_string(),
            ],
            &job,
            &[],
            &json!({}),
            Some("ssi-fixture-matrix-summary"),
            None,
        )
        .expect("mirror job");

        assert_eq!(run.id, "ssi-fixture-matrix-summary");
        assert_eq!(
            run.metadata_json["lab"]["explicit_run_id"].as_str(),
            Some("ssi-fixture-matrix-summary")
        );
    });
}

#[test]
fn mirroring_lab_job_preserves_agent_task_lifecycle_metadata() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        homeboy_agents::agent_task_lifecycle::record_lab_offload_planned(
            homeboy_agents::agent_task_lifecycle::LabOffloadProxyPlan {
                run_id: "agent-task-lab-mirror",
                runner_id: "lab",
                remote_workspace: "/srv/homeboy/project",
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("planned controller proxy");
        let job_id = Uuid::new_v4();
        let job = Job {
            id: job_id,
            operation: "exec".to_string(),
            status: JobStatus::Running,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            started_at_ms: Some(1_700_000_000_000),
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };

        mirror_job_run(
            &store,
            &ssh_runner(),
            "/srv/homeboy/project",
            &command,
            &job,
            &[],
            &json!({}),
            Some("agent-task-lab-mirror"),
            None,
        )
        .expect("mirror Lab job");
        let terminal_job = Job {
            status: JobStatus::Succeeded,
            finished_at_ms: Some(1_700_000_002_000),
            ..job.clone()
        };
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/srv/homeboy/project",
            &command,
            &terminal_job,
            &[],
            &json!({"exit_code": 0}),
            Some("agent-task-lab-mirror"),
            None,
        )
        .expect("mirror terminal Lab job");
        let lifecycle = homeboy_agents::agent_task_lifecycle::status("agent-task-lab-mirror")
            .expect("typed lifecycle remains readable");

        assert_eq!(run.kind, "agent-task");
        assert!(run.metadata_json.get("agent_task_run").is_some());
        assert_eq!(lifecycle.metadata["runner_id"], "lab");
        assert_eq!(lifecycle.metadata["runner_job_id"], job_id.to_string());
        assert_eq!(
            run.metadata_json["lab"]["remote_job"]["id"],
            job_id.to_string()
        );
    });
}

#[test]
fn test_mirrored_patch_result_reports_accessible_artifact_token() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let store = ObservationStore::open_initialized().expect("store");
        let runner = ssh_runner();
        let job_id = Uuid::new_v4();
        let mut job = Job {
            id: job_id,
            operation: "exec".to_string(),
            status: JobStatus::Succeeded,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            started_at_ms: Some(1_700_000_000_000),
            finished_at_ms: Some(1_700_000_001_000),
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };
        let artifact_id = format!("runner-fix-patch-{job_id}");
        job.artifacts.push(JobArtifactMetadata {
            id: artifact_id.clone(),
            name: Some("patch.diff".to_string()),
            path: None,
            url: None,
            mime: Some("text/x-diff".to_string()),
            size_bytes: Some(12),
            sha256: None,
            content_base64: None,
            metadata: None,
        });
        let run = mirror_job_run(
            &store,
            &runner,
            "/srv/project",
            &[
                "homeboy".to_string(),
                "runner".to_string(),
                "exec".to_string(),
            ],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("mirror job");
        let source = home.path().join("patch.diff");
        fs::write(&source, b"patch bytes!!").expect("patch bytes");
        store
            .record_artifact_with_id(&run.id, "lab_fix_patch", &source, &artifact_id, json!({}))
            .expect("record controller artifact");

        let patch = json!({
            "patch_artifact_id": artifact_id,
            "patch_artifact_path": "/srv/homeboy/.homeboy/artifacts/remote.diff",
        });

        let mirrored = mirrored_patch_result(&store, &runner, &job, Some(&patch))
            .expect("mirror patch")
            .expect("patch");

        assert!(mirrored["patch_artifact_path"]
            .as_str()
            .is_some_and(|path| path.ends_with("patch.diff")));
    });
}

#[test]
fn test_mirrored_patch_result_fails_when_patch_artifact_was_not_mirrored() {
    homeboy_core::test_support::with_isolated_home(|_| {
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
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
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
            .contains("no controller-owned artifact record is available"));
    });
}

#[test]
fn test_remote_file_artifacts_are_indexed_as_runner_tokens() {
    let detail = json!({
        "id": "run-1",
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
    let artifacts = remote_detail_artifacts(&detail, &ssh_runner(), "run-1").expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].id, "artifact-1");
    assert_eq!(artifacts[0].artifact_type, "remote_file");
    assert_eq!(artifacts[0].path, "runner-artifact://lab/run-1/artifact-1");
}

#[test]
fn test_remote_fuzz_artifacts_become_controller_owned_before_runner_cleanup() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let store = ObservationStore::open_initialized().expect("store");
        let run = RunRecord {
            id: "requested-fuzz-run".to_string(),
            kind: "fuzz".to_string(),
            component_id: Some("component-a".to_string()),
            started_at: "2026-05-16T00:00:00Z".to_string(),
            finished_at: Some("2026-05-16T00:00:01Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy fuzz run component-a".to_string()),
            cwd: Some("/srv/component-a".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: json!({}),
        };
        store.import_run(&run).expect("import fuzz run");

        let fixtures = [
            ("fuzz-results", "fuzz_results", b"results".as_slice()),
            (
                "execution-request",
                "fuzz_execution_request",
                b"request".as_slice(),
            ),
            (
                "result-envelope",
                "fuzz_result_envelope",
                b"envelope".as_slice(),
            ),
            ("coverage", "fuzz_coverage", b"coverage".as_slice()),
        ];
        let source_root = home.path().join("runner-artifacts");
        fs::create_dir_all(&source_root).expect("create runner artifact root");

        for (id, kind, bytes) in fixtures {
            let source = source_root.join(id);
            fs::write(&source, bytes).expect("write runner artifact");
            let artifact = ArtifactRecord {
                id: id.to_string(),
                run_id: run.id.clone(),
                kind: kind.to_string(),
                artifact_type: "remote_file".to_string(),
                path: runner_artifact_token("lab", "remote-fuzz-run", id),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some(format!("{:x}", Sha256::digest(bytes))),
                size_bytes: Some(i64::try_from(bytes.len()).expect("fixture size")),
                mime: None,
                metadata_json: json!({ "runner_id": "lab" }),
                created_at: "2026-05-16T00:00:01Z".to_string(),
            };
            import_mirrored_artifact_with_downloader(&store, &artifact, |_| Ok(source.clone()))
                .expect("mirror runner artifact bytes");
        }

        fs::remove_dir_all(&source_root).expect("simulate runner cleanup");
        for (id, _, bytes) in fixtures {
            let artifact = store
                .get_artifact(id)
                .expect("artifact lookup")
                .expect("controller-owned artifact");
            assert_eq!(artifact.artifact_type, "file");
            assert_eq!(fs::read(&artifact.path).expect("durable bytes"), bytes);
            assert_eq!(artifact.size_bytes, Some(bytes.len() as i64));
            assert_eq!(
                artifact.sha256.as_deref(),
                Some(format!("{:x}", Sha256::digest(bytes)).as_str())
            );
        }
    });
}

#[test]
fn test_remote_fuzz_run_mirrors_under_requested_run_id_with_lab_links() {
    let job_id = Uuid::new_v4();
    let runner = ssh_runner();
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
        path_materialization_plan: None,
        stale_reason: None,
        daemon_lease_id: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: vec![JobArtifactMetadata {
            id: "job-artifact-1".to_string(),
            name: Some("job-log.txt".to_string()),
            path: Some("runner-artifact://lab/runner-job/job-artifact-1".to_string()),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            content_base64: None,
            metadata: None,
        }],
        runner_job_projection: None,
    };
    let detail = json!({
        "id": "remote-campaign-run",
        "kind": "fuzz",
        "component_id": "component-a",
        "started_at": "2026-05-16T00:00:00Z",
        "finished_at": "2026-05-16T00:00:01Z",
        "status": "pass",
        "command": "homeboy fuzz run component-a --workload parser --run-id requested-proof",
        "cwd": "/srv/homeboy/component-a",
        "metadata": {
            "campaign_id": "campaign-123"
        },
        "artifacts": [{
            "id": "fuzz-results",
            "kind": "fuzz_results",
            "type": "file",
            "created_at": "2026-05-16T00:00:01Z"
        }]
    });

    let run = remote_detail_to_run_record(&detail, &runner, Some(&job)).expect("run record");
    let artifacts = remote_detail_artifacts(&detail, &runner, &run.id).expect("artifacts");

    assert_eq!(run.id, "requested-proof");
    assert_eq!(run.metadata_json["lab"]["local_run_id"], "requested-proof");
    assert_eq!(
        run.metadata_json["lab"]["remote_run_id"],
        "remote-campaign-run"
    );
    assert_eq!(
        run.metadata_json["lab"]["remote_job_id"],
        job_id.to_string()
    );
    assert_eq!(
        run.metadata_json["lab"]["remote_workspace"],
        "/srv/homeboy/component-a"
    );
    assert_eq!(
        run.metadata_json["lab"]["fuzz"]["campaign_id"],
        "campaign-123"
    );
    assert_eq!(
        run.metadata_json["lab"]["fuzz"]["local_run_id"],
        "requested-proof"
    );
    assert_eq!(
        run.metadata_json["lab"]["artifact_refs"][0]["artifact_id"],
        "job-artifact-1"
    );
    assert_eq!(artifacts[0].run_id, "requested-proof");
    assert_eq!(
        artifacts[0].metadata_json["local_run_id"],
        "requested-proof"
    );
    assert_eq!(
        artifacts[0].metadata_json["remote_run_id"],
        "remote-campaign-run"
    );
    assert_eq!(
        artifacts[0].path,
        "runner-artifact://lab/remote-campaign-run/fuzz-results"
    );
}

#[test]
fn test_fuzz_run_id_from_command_accepts_split_and_equals_forms() {
    assert_eq!(
        fuzz_run_id_from_command("homeboy fuzz run component --run-id proof-1"),
        Some("proof-1")
    );
    assert_eq!(
        fuzz_run_id_from_command("homeboy fuzz run component --run-id=proof-2"),
        Some("proof-2")
    );
}

#[test]
fn test_primary_mirrored_run_prefers_fuzz_run_identity() {
    let runner_exec = RunRecord {
        id: "runner-exec-lab-job".to_string(),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: "2026-05-16T00:00:00Z".to_string(),
        finished_at: Some("2026-05-16T00:00:01Z".to_string()),
        status: "pass".to_string(),
        command: None,
        cwd: None,
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: json!({}),
    };
    let fuzz = RunRecord {
        id: "requested-proof".to_string(),
        kind: "fuzz".to_string(),
        ..runner_exec.clone()
    };

    let primary = primary_mirrored_run(&[runner_exec, fuzz]).expect("primary fuzz run");

    assert_eq!(primary.id, "requested-proof");
}

#[test]
fn test_explicit_observation_run_ids_prefers_result_lineage() {
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
        path_materialization_plan: None,
        stale_reason: None,
        daemon_lease_id: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: vec![JobArtifactMetadata {
            id: "artifact-1".to_string(),
            name: None,
            path: Some("runner-artifact://lab/run-from-job/artifact-1".to_string()),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            content_base64: None,
            metadata: None,
        }],
        runner_job_projection: None,
    };
    let result = json!({
        "mirror_run_id": "run-a",
        "observation_run_ids": ["run-b", "run-a"],
        "runner_result": {
            "artifact_refs": [{
                "artifact_id": "artifact-2",
                "path": "runner-artifact://lab/run-from-ref/artifact-2"
            }]
        }
    });

    assert_eq!(
        explicit_observation_run_ids(&result, &job),
        vec![
            "run-a".to_string(),
            "run-b".to_string(),
            "run-from-job".to_string(),
            "run-from-ref".to_string(),
        ]
    );
}

#[test]
fn terminal_mirroring_imports_only_the_submitted_overlapping_job_run_and_artifacts() {
    let mut first = terminal_runner_job();
    first.artifacts = vec![JobArtifactMetadata {
        id: "first-result".to_string(),
        name: Some("fuzz_results".to_string()),
        path: Some("runner-artifact://lab/first-run/first-result".to_string()),
        url: None,
        mime: None,
        size_bytes: None,
        sha256: None,
        content_base64: None,
        metadata: None,
    }];
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    second.artifacts[0].id = "second-result".to_string();
    second.artifacts[0].path = Some("runner-artifact://lab/second-run/second-result".to_string());

    for (job, run_id) in [(&mut first, "first-run"), (&mut second, "second-run")] {
        job.runner_job_projection = Some(RunnerJobProjection {
            runner_id: "lab".to_string(),
            command: "homeboy fuzz run".to_string(),
            cwd: Some("/srv/homeboy/project".to_string()),
            source: "runner-daemon".to_string(),
            kind: "runner.exec".to_string(),
            lifecycle: Some(RunnerJobLifecycleMetadata {
                durable_run_id: Some(run_id.to_string()),
                ..Default::default()
            }),
        });
    }

    for (job, expected_run, other_run, artifact_id) in [
        (&first, "first-run", "second-run", "first-result"),
        (&second, "second-run", "first-run", "second-result"),
    ] {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let runner = ssh_runner();
            let details = std::collections::HashMap::from([
                (
                    "first-run",
                    json!({
                        "id": "first-run",
                        "kind": "fuzz",
                        "started_at": "2023-11-14T22:13:20Z",
                        "finished_at": "2023-11-14T22:13:21Z",
                        "status": "pass",
                        "artifacts": [{"id": "first-result", "kind": "fuzz_results", "type": "file", "size_bytes": 1, "sha256": "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"}]
                    }),
                ),
                (
                    "second-run",
                    json!({
                        "id": "second-run",
                        "kind": "fuzz",
                        "started_at": "2023-11-14T22:13:20Z",
                        "finished_at": "2023-11-14T22:13:21Z",
                        "status": "fail",
                        "artifacts": [{"id": "second-result", "kind": "fuzz_results", "type": "file", "size_bytes": 1, "sha256": "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d"}]
                    }),
                ),
            ]);

            let run_ids = explicit_observation_run_ids(&json!({}), job);
            let downloaded = tempfile::tempdir().expect("download directory");
            let mirrored = mirror_remote_observation_runs_by_id_with_downloader(
                &store,
                &runner,
                job,
                &run_ids,
                None,
                |run_id| Ok(details.get(run_id).cloned()),
                |artifact_path| {
                    let path = downloaded
                        .path()
                        .join(artifact_path.rsplit('/').next().expect("artifact id"));
                    let bytes = if artifact_path.ends_with("first-result") {
                        b"a".as_slice()
                    } else {
                        b"b".as_slice()
                    };
                    fs::write(&path, bytes).expect("artifact bytes");
                    Ok(path)
                },
            )
            .expect("mirror submitted terminal run");

            assert_eq!(mirrored.len(), 1);
            assert_eq!(mirrored[0].id, expected_run);
            assert_eq!(
                mirrored[0].metadata_json["lab"]["remote_job_id"],
                job.id.to_string()
            );
            assert!(store
                .get_run(other_run)
                .expect("other run lookup")
                .is_none());
            assert_eq!(
                store
                    .get_artifact(artifact_id)
                    .expect("artifact lookup")
                    .expect("mirrored artifact")
                    .run_id,
                expected_run
            );
        });
    }
}

#[test]
fn terminal_mirroring_withholds_output_when_declared_run_projection_is_missing() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let run_ids = vec![
            "concurrent-run-a".to_string(),
            "concurrent-run-b".to_string(),
        ];
        let job = terminal_runner_job();
        let mut requested = Vec::new();

        let error = mirror_remote_observation_runs_by_id_with(
            &store,
            &ssh_runner(),
            &job,
            &run_ids,
            None,
            |run_id| {
                requested.push(run_id.to_string());
                Err(Error::new(
                    ErrorCode::InternalUnexpected,
                    "daemon request failed: run record not found",
                    json!({ "http_status": 404, "path": format!("/runs/{run_id}") }),
                ))
            },
        )
        .expect_err("terminal output requires controller-visible run projections");

        assert_eq!(requested, vec!["concurrent-run-a"]);
        assert_eq!(error.details["runner_id"], "lab");
        assert_eq!(error.details["run_id"], "concurrent-run-a");
        assert_eq!(error.details["source_error"]["details"]["http_status"], 404);
        assert!(!error.retryable.unwrap_or(true));
    });
}

#[test]
fn terminal_mirroring_persists_declared_run_and_artifact_for_controller_review() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("controller store");
        let run_id = "bench-controller-review".to_string();
        let bytes = b"reviewable visual evidence";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let download_dir = tempfile::tempdir().expect("download directory");
        let download_path = download_dir.path().join("diff.png");
        let job = terminal_runner_job();

        let mirrored = mirror_remote_observation_runs_by_id_with_downloader(
            &store,
            &ssh_runner(),
            &job,
            std::slice::from_ref(&run_id),
            None,
            |requested| {
                assert_eq!(requested, run_id);
                Ok(Some(json!({
                    "id": run_id,
                    "kind": "bench",
                    "started_at": "2023-11-14T22:13:20Z",
                    "finished_at": "2023-11-14T22:13:21Z",
                    "status": "fail",
                    "artifacts": [{
                        "id": "visual-diff",
                        "kind": "visual_compare",
                        "type": "file",
                        "size_bytes": bytes.len(),
                        "sha256": digest,
                        "mime": "image/png"
                    }]
                })))
            },
            |_| {
                fs::write(&download_path, bytes).expect("write downloaded artifact");
                Ok(download_path.clone())
            },
        )
        .expect("terminal runner evidence projects to the controller");

        assert_eq!(mirrored[0].id, run_id);
        assert_eq!(
            store
                .get_run(&run_id)
                .expect("controller run lookup")
                .expect("controller run"),
            mirrored[0]
        );
        let artifact = store
            .get_artifact("visual-diff")
            .expect("controller artifact lookup")
            .expect("controller artifact");
        assert_eq!(artifact.run_id, run_id);
        assert_eq!(artifact.artifact_type, "file");
        assert_eq!(
            fs::read(&artifact.path).expect("reviewer reads artifact"),
            bytes
        );
    });
}

#[test]
fn terminal_mirroring_keeps_every_declared_visual_artifact_after_runner_cleanup() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("controller store");
        let run_id = "bench-visual-review".to_string();
        let workspace = tempfile::tempdir().expect("runner workspace");
        let artifacts = (0..21)
            .map(|index| {
                let id = format!("visual-{index:02}");
                let bytes = format!("png-{index}").into_bytes();
                let path = workspace.path().join(format!("{id}.png"));
                fs::write(&path, &bytes).expect("runner artifact");
                json!({
                    "id": id,
                    "kind": "visual_compare",
                    "type": "file",
                    "size_bytes": bytes.len(),
                    "sha256": format!("{:x}", Sha256::digest(&bytes)),
                    "mime": "image/png"
                })
            })
            .collect::<Vec<_>>();
        let job = terminal_runner_job();

        mirror_remote_observation_runs_by_id_with_downloader(
            &store,
            &ssh_runner(),
            &job,
            std::slice::from_ref(&run_id),
            None,
            |_| {
                Ok(Some(json!({
                    "id": run_id,
                    "kind": "bench",
                    "started_at": "2023-11-14T22:13:20Z",
                    "finished_at": "2023-11-14T22:13:21Z",
                    "status": "pass",
                    "artifacts": artifacts,
                })))
            },
            |artifact_path| {
                let id = artifact_path.rsplit('/').next().expect("artifact id");
                Ok(workspace.path().join(format!("{id}.png")))
            },
        )
        .expect("all declared visual artifacts are durably mirrored");

        workspace.close().expect("runner workspace cleanup");
        drop(store);

        // A reviewer opens the controller store after the runner is gone and
        // retrieves bytes by the stable run/artifact reference alone.
        let reader = ObservationStore::open_initialized().expect("reviewer store");
        let persisted = reader.list_artifacts(&run_id).expect("persisted artifacts");
        assert_eq!(persisted.len(), 21);
        for artifact in persisted {
            assert_eq!(artifact.artifact_type, "file");
            assert!(artifact.sha256.is_some());
            assert!(!fs::read(&artifact.path)
                .expect("controller artifact")
                .is_empty());
        }
        let artifact = runs_service::resolve_artifact_for_run(&reader, &run_id, "visual-00")
            .expect("reviewer resolves durable artifact");
        let reviewer_copy = tempfile::NamedTempFile::new().expect("reviewer output");
        let outcome = runs_service::copy_local_file_artifact(
            artifact,
            Some(reviewer_copy.path().to_path_buf()),
        )
        .expect("reviewer retrieves durable artifact");
        assert_eq!(outcome.run_id, run_id);
        assert_eq!(outcome.artifact_id, "visual-00");
        assert_eq!(
            fs::read(reviewer_copy.path()).expect("reviewer bytes"),
            b"png-0"
        );
    });
}

#[test]
fn terminal_mirroring_copies_declared_directory_with_tree_hash() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("controller store");
        let run_id = "bench-directory-review".to_string();
        let workspace = tempfile::tempdir().expect("runner workspace");
        let directory = workspace.path().join("visuals");
        fs::create_dir_all(directory.join("nested")).expect("directory");
        fs::write(directory.join("baseline.png"), b"baseline").expect("baseline");
        fs::write(directory.join("nested/diff.png"), b"diff").expect("diff");
        let tree_sha256 =
            homeboy_core::observation::directory_tree_sha256(&directory).expect("tree hash");

        mirror_remote_observation_runs_by_id_with_downloader(
            &store,
            &ssh_runner(),
            &terminal_runner_job(),
            std::slice::from_ref(&run_id),
            None,
            |_| {
                Ok(Some(json!({
                    "id": run_id,
                    "kind": "bench",
                    "started_at": "2023-11-14T22:13:20Z",
                    "finished_at": "2023-11-14T22:13:21Z",
                    "status": "pass",
                    "artifacts": [{
                        "id": "visual-directory",
                        "kind": "visual_compare",
                        "type": "directory",
                        "path": "runner-artifact://lab/bench-directory-review/visual-directory",
                        "sha256": tree_sha256.clone()
                    }]
                })))
            },
            |_| Ok(directory.clone()),
        )
        .expect("directory is durably mirrored");

        workspace.close().expect("runner workspace cleanup");
        let artifact = store
            .get_artifact("visual-directory")
            .expect("artifact lookup")
            .expect("artifact");
        assert_eq!(artifact.artifact_type, "directory");
        assert_eq!(artifact.sha256.as_deref(), Some(tree_sha256.as_str()));
        assert!(std::path::Path::new(&artifact.path).is_dir());
    });
}

#[test]
fn terminal_mirroring_rejects_url_only_declared_artifacts() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("controller store");
        let run_id = "bench-expired-tunnel".to_string();
        let error = mirror_remote_observation_runs_by_id_with_downloader(
            &store,
            &ssh_runner(),
            &terminal_runner_job(),
            std::slice::from_ref(&run_id),
            None,
            |_| {
                Ok(Some(json!({
                    "id": run_id,
                    "kind": "bench",
                    "started_at": "2023-11-14T22:13:20Z",
                    "finished_at": "2023-11-14T22:13:21Z",
                    "status": "pass",
                    "artifacts": [{
                        "id": "expired-tunnel",
                        "kind": "visual_compare",
                        "type": "url",
                        "url": "https://expired.example/visual.png"
                    }]
                })))
            },
            |_| unreachable!("URL artifacts must not be downloaded as terminal evidence"),
        )
        .expect_err("URL-only artifact must fail terminal projection");

        assert!(error.message.contains("durably project all artifacts"));
        assert!(store.get_run(&run_id).expect("run lookup").is_none());
    });
}

#[test]
fn reverse_broker_lookup_projects_only_embedded_typed_run_details() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let job = terminal_runner_job();
        let run = RunRecord {
            id: "embedded-run".to_string(),
            kind: "bench".to_string(),
            component_id: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: Some("2026-01-01T00:01:00Z".to_string()),
            status: "succeeded".to_string(),
            command: None,
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: json!({}),
        };
        let result = json!({
            "exit_code": 0,
            "observation_run_ids": [run.id.clone()],
            "observation_run_details": [RemoteRunnerObservationRunDetail::v1(run, Vec::new())],
        });

        let mirrored = mirror_reverse_broker_evidence(
            &ssh_runner(),
            "http://127.0.0.1:1",
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &result,
            None,
            None,
        )
        .expect("embedded terminal detail does not request a broker run endpoint")
        .expect("terminal evidence is mirrored");
        assert_eq!(mirrored.run.id, "embedded-run");

        let legacy = json!({ "exit_code": 0, "observation_run_ids": ["legacy-run"] });
        let mirrored = mirror_reverse_broker_evidence(
            &ssh_runner(),
            "http://127.0.0.1:1",
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &legacy,
            None,
            None,
        )
        .expect("old worker terminal result retains controller-owned evidence")
        .expect("terminal evidence is mirrored");
        assert_ne!(mirrored.run.id, "legacy-run");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .get_run(&mirrored.run.id)
            .expect("run lookup")
            .expect("controller synthetic run");
        assert_eq!(
            run.metadata_json["lab"]["reverse_broker"]["unresolved_run_refs"],
            json!(["legacy-run"])
        );
        assert_eq!(
            run.metadata_json["lab"]["reverse_broker"]["observation_run_details"],
            json!("unavailable_legacy")
        );
    });
}

#[test]
fn reverse_failure_mirror_retains_terminal_failure_projection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut job = terminal_runner_job();
        job.status = JobStatus::Failed;
        let mirrored = mirror_reverse_broker_evidence(
            &ssh_runner(),
            "http://127.0.0.1:1",
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &json!({
                "exit_code": 2,
                "stderr": "invalid runner input",
                "data": {
                    "phase": "preflight",
                    "error": { "code": "validation.invalid_argument", "message": "invalid input" },
                    "execution_record": { "runner_id": "lab", "transport": "reverse_broker" },
                    "orchestration_provenance": { "source": "reverse" }
                },
                "artifact_refs": [{ "id": "failure-log", "path": "runner-artifact://lab/job/failure-log" }]
            }),
            None,
            None,
        )
        .expect("reverse failure mirror")
        .expect("mirrored failure");

        let failure = &mirrored.run.metadata_json["lab"]["failure"];
        assert_eq!(failure["failure_code"], "validation.invalid_argument");
        assert_eq!(failure["phase"], "preflight");
        assert_eq!(failure["execution_record"]["transport"], "reverse_broker");
        assert_eq!(failure["artifact_refs"], json!([]));
    });
}

#[test]
fn reverse_broker_refresh_uses_persisted_transport_after_store_reopen() {
    homeboy_core::test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("persist runner");
        let job = terminal_runner_job();
        let events = vec![JobEvent {
            sequence: 1,
            job_id: job.id,
            kind: JobEventKind::Result,
            timestamp_ms: job.finished_at_ms.expect("terminal timestamp"),
            message: None,
            data: Some(json!({ "exit_code": 0 })),
        }];
        let (broker_url, shutdown, broker) = reverse_broker_fixture(job.clone(), events.clone());
        let mirrored = mirror_reverse_broker_evidence(
            &ssh_runner(),
            &broker_url,
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &events,
            &json!({ "exit_code": 0 }),
            None,
            None,
        )
        .expect("persist reverse mirror")
        .expect("reverse mirror");

        // Refresh opens a new observation store and must recover its transport
        // choice from persisted provenance, not a live daemon session.
        let refreshed = refresh_mirrored_daemon_evidence(&mirrored.run.id)
            .expect("refresh via persisted reverse broker")
            .expect("refreshed evidence");
        assert_eq!(refreshed[0].id, mirrored.run.id);
        assert_eq!(
            refreshed[0].metadata_json["lab"]["reverse_broker"]["broker_url"],
            broker_url
        );
        assert_eq!(
            {
                shutdown.send(()).expect("stop broker");
                broker.join().expect("broker requests")
            },
            vec![
                format!("/jobs/{}", job.id),
                format!("/jobs/{}/events", job.id),
                "/runner/jobs/reconcile".to_string(),
            ]
        );
    });
}

#[test]
fn reverse_result_refresh_treats_preterminal_absence_as_pending_then_projects_terminal_once() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        job.status = JobStatus::Queued;
        job.finished_at_ms = None;
        let command = vec!["homeboy".to_string(), "bench".to_string()];

        for status in [JobStatus::Queued, JobStatus::Running] {
            job.status = status;
            let evidence = mirror_reverse_broker_evidence(
                &ssh_runner(),
                "http://broker.invalid",
                "/runner/project",
                &command,
                &job,
                &[],
                &json!({}),
                None,
                None,
            )
            .expect("preterminal result is pending")
            .expect("pending evidence");
            assert_eq!(
                evidence.run.metadata_json["lab"]["result_availability"]["state"],
                "pending"
            );
            assert_eq!(
                evidence.run.metadata_json["lab"]["result_availability"]["last_observed_phase"],
                serde_json::to_value(status).expect("status JSON"),
            );
            assert_eq!(
                evidence.run.metadata_json["lab"]["result_availability"]["next_poll_action"],
                "refresh_mirrored_daemon_evidence"
            );
        }

        job.status = JobStatus::Succeeded;
        job.finished_at_ms = Some(job.updated_at_ms);
        let terminal = mirror_reverse_broker_evidence(
            &ssh_runner(),
            "http://broker.invalid",
            "/runner/project",
            &command,
            &job,
            &[],
            &json!({ "exit_code": 0 }),
            None,
            None,
        )
        .expect("valid terminal result")
        .expect("terminal evidence");
        assert_eq!(
            terminal.run.metadata_json["lab"]["result_availability"]["state"],
            "terminal"
        );
        assert_eq!(store.list_runs(Default::default()).expect("runs").len(), 1);
    });
}

#[test]
fn direct_result_refresh_treats_preterminal_absence_as_pending() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut job = terminal_runner_job();
        job.status = JobStatus::Running;
        job.finished_at_ms = None;
        let evidence = mirror_daemon_evidence(
            &ssh_runner(),
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("preterminal direct result is pending")
        .expect("pending direct evidence");

        assert_eq!(
            evidence.run.metadata_json["lab"]["result_availability"]["state"],
            "pending"
        );
        assert_eq!(
            evidence.run.metadata_json["lab"]["result_availability"]["last_observed_phase"],
            "running"
        );
    });
}

#[test]
fn malformed_terminal_reverse_result_keeps_transport_and_payload_diagnostics() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let job = terminal_runner_job();
        let error = mirror_reverse_broker_evidence(
            &ssh_runner(),
            "http://broker.invalid",
            "/runner/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            &json!({ "stdout": "missing exit code" }),
            None,
            None,
        )
        .expect_err("malformed terminal result is actionable");

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(error.message.contains(&job.id.to_string()));
        assert!(error.message.contains("reverse-broker"));
        assert!(error.message.contains("RemoteRunnerJobResult"));
        assert!(error.message.contains("missing exit code"));
    });
}

#[test]
fn legacy_terminal_artifacts_use_the_controller_runner_run_and_survive_runner_disconnect() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        let bytes = b"legacy terminal evidence";
        job.artifacts = vec![JobArtifactMetadata {
            id: "legacy-result".to_string(),
            name: Some("result.json".to_string()),
            path: Some("/runner-only/result.json".to_string()),
            url: Some("https://runner.invalid/result.json".to_string()),
            mime: Some("application/json".to_string()),
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            content_base64: None,
            metadata: None,
        }];
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner/project",
            &["homeboy".to_string(), "test".to_string()],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("synthetic terminal run");

        mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |artifact_id| {
            assert_eq!(artifact_id, "legacy-result");
            Ok(bytes.to_vec())
        })
        .expect("project all legacy terminal artifacts");

        let artifacts = super::mirror::controller_artifact_metadata(&[run.run.clone()])
            .expect("controller artifact response");
        assert_eq!(artifacts.len(), 1);
        assert_ne!(
            artifacts[0].path.as_deref(),
            Some("/runner-only/result.json")
        );
        assert_eq!(
            fs::read(artifacts[0].path.as_ref().expect("controller path")).expect("bytes"),
            bytes
        );
    });
}

#[test]
fn terminal_artifacts_missing_integrity_are_permanent_provenance_errors() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        job.artifacts = vec![JobArtifactMetadata {
            id: "missing-digest".to_string(),
            name: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: Some(1),
            sha256: None,
            content_base64: None,
            metadata: None,
        }];
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner",
            &[],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("synthetic run");
        let error =
            mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |_| Ok(vec![1]))
                .expect_err("missing provenance is not retryable");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(!error.retryable.unwrap_or(true));
        assert!(store.list_artifacts(&run.id).expect("artifacts").is_empty());
    });
}

#[test]
fn terminal_artifact_missing_mime_is_a_permanent_provenance_error() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        let bytes = b"typed terminal artifact";
        job.artifacts = vec![JobArtifactMetadata {
            id: "missing-mime".to_string(),
            name: Some("artifact.unknown".to_string()),
            path: Some("/runner/artifact.unknown".to_string()),
            url: None,
            mime: None,
            size_bytes: Some(bytes.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            content_base64: None,
            metadata: None,
        }];
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner",
            &[],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("synthetic run");

        let error = mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |_| {
            Ok(bytes.to_vec())
        })
        .expect_err("missing media type is rejected");

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(
            error.details["source_error"]["details"]["field"],
            "artifact.mime"
        );
        assert!(!error.retryable.unwrap_or(true));
        assert!(store.list_artifacts(&run.id).expect("artifacts").is_empty());
    });
}

#[test]
fn terminal_artifact_download_failure_rolls_back_every_artifact_and_retry_is_idempotent() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        let first = b"first terminal artifact";
        let second = b"second terminal artifact";
        job.artifacts = [("first", first.as_slice()), ("second", second.as_slice())]
            .into_iter()
            .map(|(id, bytes)| JobArtifactMetadata {
                id: id.to_string(),
                name: None,
                path: None,
                url: None,
                mime: Some("application/octet-stream".to_string()),
                size_bytes: Some(bytes.len() as u64),
                sha256: Some(format!("{:x}", Sha256::digest(bytes))),
                content_base64: None,
                metadata: None,
            })
            .collect();
        let run = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner",
            &[],
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("synthetic run");
        let error = mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |id| {
            if id == "second" {
                Err(Error::internal_io("runner disconnected", None))
            } else {
                Ok(first.to_vec())
            }
        })
        .expect_err("later artifact download fails the complete projection");
        assert!(error.retryable.unwrap_or(false));
        assert!(store.list_artifacts(&run.id).expect("artifacts").is_empty());

        let first_projection =
            mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |id| {
                Ok(if id == "first" {
                    first.to_vec()
                } else {
                    second.to_vec()
                })
            })
            .expect("retry projects complete artifact set");
        let replay = mirror_terminal_job_artifacts_with(&store, &ssh_runner(), &job, &run, |id| {
            Ok(if id == "first" {
                first.to_vec()
            } else {
                second.to_vec()
            })
        })
        .expect("identical retry is idempotent");
        assert_eq!(first_projection, replay);
        assert_eq!(store.list_artifacts(&run.id).expect("artifacts").len(), 2);
    });
}

#[test]
fn failed_refresh_discards_its_synthetic_run_after_artifact_projection_error() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        job.artifacts = vec![JobArtifactMetadata {
            id: "malformed".to_string(),
            name: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: Some(1),
            sha256: None,
            content_base64: None,
            metadata: None,
        }];
        let command = vec!["homeboy".to_string(), "test".to_string()];
        let synthetic_id = super::util::local_job_run_id("lab", &job.id.to_string(), "test");
        refresh_mirrored_daemon_evidence_with(
            &store,
            &ssh_runner(),
            "/runner",
            &command,
            &job,
            &[],
            &json!({}),
            None,
            || Ok(()),
        )
        .expect_err("malformed terminal artifact fails refresh");
        assert!(store
            .get_run(&synthetic_id)
            .expect("synthetic lookup")
            .is_none());
    });
}

#[test]
fn failed_refresh_preserves_a_concurrent_synthetic_winner() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let mut job = terminal_runner_job();
        job.artifacts = vec![JobArtifactMetadata {
            id: "malformed".to_string(),
            name: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: Some(1),
            sha256: None,
            content_base64: None,
            metadata: None,
        }];
        let command = vec!["homeboy".to_string(), "test".to_string()];
        let winner = mirror_job_run(
            &store,
            &ssh_runner(),
            "/runner",
            &command,
            &job,
            &[],
            &json!({}),
            None,
            None,
        )
        .expect("concurrent winner");
        refresh_mirrored_daemon_evidence_with(
            &store,
            &ssh_runner(),
            "/runner",
            &command,
            &job,
            &[],
            &json!({}),
            None,
            || Ok(()),
        )
        .expect_err("malformed terminal artifact fails refresh");
        assert!(store.get_run(&winner.id).expect("winner lookup").is_some());
    });
}

#[test]
fn terminal_mirroring_rejects_unrelated_missing_run_projections() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = ObservationStore::open_initialized().expect("store");
        let run_ids = vec!["expected-run".to_string()];
        let job = terminal_runner_job();

        let error = mirror_remote_observation_runs_by_id_with(
            &store,
            &ssh_runner(),
            &job,
            &run_ids,
            None,
            |_| {
                Err(Error::new(
                    ErrorCode::InternalUnexpected,
                    "daemon request failed: run record not found",
                    json!({ "http_status": 404, "path": "/runs/other-run" }),
                ))
            },
        )
        .expect_err("unrelated missing projection must remain fail-closed");

        assert_eq!(error.details["path"], "/runs/other-run");
    });
}
