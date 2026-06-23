use serde_json::json;
use uuid::Uuid;

use super::super::convert::{explicit_observation_run_ids, fuzz_run_id_from_command};
use super::succeeded_job;
use crate::core::api_jobs::JobArtifactMetadata;

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
fn test_explicit_observation_run_ids_prefers_result_lineage() {
    let job_id = Uuid::new_v4();
    let mut job = succeeded_job(job_id);
    job.artifacts = vec![JobArtifactMetadata {
        id: "artifact-1".to_string(),
        name: None,
        path: Some("runner-artifact://lab/run-from-job/artifact-1".to_string()),
        url: None,
        mime: None,
        size_bytes: None,
        sha256: None,
        content_base64: None,
        metadata: None,
    }];
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
