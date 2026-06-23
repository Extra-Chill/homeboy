use serde_json::json;

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobStatus};
use crate::core::observation::RunRecord;
use uuid::Uuid;

use super::super::conversion::{explicit_observation_run_ids, fuzz_run_id_from_command};
use super::super::mirror::primary_mirrored_run;

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
