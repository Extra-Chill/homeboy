use serde_json::json;

use crate::core::api_jobs::JobArtifactMetadata;
use crate::core::runner::{RunnerExecMode, RunnerExecOutput};

use super::super::result::remote_runner_result_from_exec_output;

#[test]
fn reverse_worker_result_preserves_exec_patch_and_artifacts() {
    let result = remote_runner_result_from_exec_output(
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Local,
            argv: vec!["homeboy".to_string(), "refactor".to_string()],
            remote_cwd: "/srv/workspace".to_string(),
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: Some(json!({
                "patch_artifact_id": "patch.diff",
                "modified_files": ["src/lib.rs"],
            })),
            mutation_artifacts: Some(crate::core::runner::RunnerMutationArtifacts {
                patch_ref: Some(crate::core::runner::RunnerArtifactRef {
                    artifact_id: "patch.diff".to_string(),
                    name: Some("patch.diff".to_string()),
                    path: Some("/srv/workspace/.homeboy/patch.diff".to_string()),
                    url: None,
                    mime: Some("text/x-diff".to_string()),
                    size_bytes: Some(42),
                    sha256: Some("abc123".to_string()),
                    transport: None,
                }),
                file_bundle_ref: None,
                operation_log_ref: None,
            }),
            artifacts: vec![JobArtifactMetadata {
                id: "patch.diff".to_string(),
                name: Some("patch.diff".to_string()),
                path: Some("/srv/workspace/.homeboy/patch.diff".to_string()),
                url: None,
                mime: Some("text/x-diff".to_string()),
                size_bytes: Some(42),
                sha256: Some("abc123".to_string()),
                content_base64: None,
                metadata: Some(json!({ "kind": "lab_fix_patch" })),
            }],
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        },
        0,
        None,
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(
        result.patch.as_ref().expect("patch")["patch_artifact_id"],
        "patch.diff"
    );
    assert_eq!(
        result.data.as_ref().expect("data")["patch"]["modified_files"][0],
        "src/lib.rs"
    );
    assert_eq!(result.artifacts[0].id, "patch.diff");
    assert_eq!(
        result
            .mutation_artifacts
            .as_ref()
            .and_then(|artifacts| artifacts.patch_ref.as_ref())
            .map(|artifact| artifact.artifact_id.as_str()),
        Some("patch.diff")
    );
}

#[test]
fn reverse_worker_result_mirrors_file_artifact_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artifact_path = dir.path().join("report.txt");
    std::fs::write(&artifact_path, b"worker artifact bytes").expect("write artifact");

    let result = remote_runner_result_from_exec_output(
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Local,
            argv: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
            remote_cwd: dir.path().display().to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            mutation_artifacts: None,
            artifacts: vec![JobArtifactMetadata {
                id: "report".to_string(),
                name: Some("report.txt".to_string()),
                path: Some("report.txt".to_string()),
                url: None,
                mime: Some("text/plain".to_string()),
                size_bytes: Some(21),
                sha256: None,
                content_base64: None,
                metadata: None,
            }],
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        },
        0,
        None,
    );

    assert_eq!(
        result.artifacts[0].content_base64.as_deref(),
        Some("d29ya2VyIGFydGlmYWN0IGJ5dGVz")
    );
}

#[test]
fn reverse_worker_result_attaches_typed_agent_task_lifecycle_event() {
    let result = remote_runner_result_from_exec_output(
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab-default".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Local,
            argv: vec!["homeboy".to_string(), "agent-task".to_string()],
            remote_cwd: "/srv/workspace".to_string(),
            exit_code: 0,
            stdout: concat!(
                "runner chatter\n",
                "{\"success\":true,\"data\":{",
                "\"schema\":\"homeboy/agent-task-aggregate/v1\",",
                "\"plan_id\":\"plan-typed\",",
                "\"status\":\"succeeded\",",
                "\"totals\":{\"skipped\":0,\"succeeded\":1,\"failed\":0},",
                "\"outcomes\":[]}}"
            )
            .to_string(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: Some("job-typed".to_string()),
            job_events: None,
            mirror_run_id: Some("run-typed".to_string()),
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        },
        0,
        None,
    );

    let event = &result.data.as_ref().expect("data")["agent_task_lifecycle_event"];
    assert_eq!(
        event["schema"],
        "homeboy/agent-task-run-plan-lifecycle-event/v1"
    );
    assert_eq!(event["identity"]["runner_id"], "lab-default");
    assert_eq!(event["identity"]["runner_job_id"], "job-typed");
    assert_eq!(event["identity"]["run_id"], "run-typed");
    assert_eq!(event["aggregate"]["plan_id"], "plan-typed");
}
