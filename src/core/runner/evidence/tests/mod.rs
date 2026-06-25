use std::fs;

use reqwest::header;
use serde_json::json;
use uuid::Uuid;

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobStatus};
use crate::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use crate::core::runner::{Runner, RunnerKind};
use crate::core::server::{RunnerPolicy, RunnerSettings};

use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
};
use super::download::{content_disposition_filename, download_remote_artifact};
use super::mirror::{mirror_job_run, mirrored_patch_result, primary_mirrored_run};
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
            None,
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
fn runner_exec_matrix_summary_run_names_come_from_command_domain() {
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
fn runner_exec_explicit_run_id_overrides_inferred_name() {
    crate::test_support::with_isolated_home(|_| {
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
        stale_reason: None,
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
        stale_reason: None,
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
