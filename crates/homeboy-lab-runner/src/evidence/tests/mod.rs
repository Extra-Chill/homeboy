use std::fs;

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
use homeboy_core::observation::{ArtifactRecord, NewRunRecord, ObservationStore, RunRecord};
use homeboy_core::server::{RunnerPolicy, RunnerSettings};

use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
};
use super::download::{content_disposition_filename, download_remote_artifact};
use super::mirror::{
    bounded_remote_events, controller_artifact_metadata, import_mirrored_artifact_with_downloader,
    mirror_job_run, mirror_remote_observation_runs_by_id_with,
    mirror_remote_observation_runs_by_id_with_downloader, mirror_reverse_broker_evidence,
    mirror_terminal_job_artifacts_with, mirrored_patch_result, primary_mirrored_run,
    refresh_mirrored_daemon_evidence_with, MIRRORED_REMOTE_EVENT_LIMIT,
    MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT,
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
                mime: None,
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
