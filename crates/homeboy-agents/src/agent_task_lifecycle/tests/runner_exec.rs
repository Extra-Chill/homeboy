//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup):
//! generic runner-exec run creation and identity (#9927).
#![cfg(test)]

use super::*;
use homeboy_core::api_jobs::{Job, JobStore, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;

#[test]
fn runner_exec_run_id_creates_generic_run_on_demand() {
    // #8447: `runner exec --run-id <new-id>` documents an explicit persisted
    // evidence ID, but the ID was routed through agent-task lifecycle lookup and
    // rejected as a missing agent-task record before the command executed. A new
    // ad hoc ID must own a generic runner-execution run created on demand.
    with_isolated_home(|_| {
        let command = vec!["cargo".to_string(), "build".to_string()];

        // (1) A new valid runner-exec ID creates and binds a generic run.
        let created = record_runner_exec_job_identity(
            "recovery-8447-lab-build-r3",
            "homeboy-lab",
            "job-1",
            "/runner/workspace/homeboy",
            &command,
        )
        .expect("new ad hoc run id creates a generic runner-exec run");
        assert_eq!(created.metadata["kind"], RUNNER_EXEC_RUN_KIND);
        assert_eq!(created.metadata["runner_id"], "homeboy-lab");
        assert_eq!(created.metadata["runner_job_id"], "job-1");

        // The generic run is a real durable record readable through status.
        let loaded = status("recovery-8447-lab-build-r3").expect("generic run persisted");
        assert_eq!(loaded.metadata["kind"], RUNNER_EXEC_RUN_KIND);

        // (2) Reusing the same generic ID re-attaches without error.
        let reused = record_runner_exec_job_identity(
            "recovery-8447-lab-build-r3",
            "homeboy-lab",
            "job-2",
            "/runner/workspace/homeboy",
            &command,
        )
        .expect("existing generic run id re-binds");
        assert_eq!(reused.metadata["runner_job_id"], "job-2");

        // (3) An ID already owned by an agent-task lifecycle run is a different
        //     owner: reusing it as a generic runner-exec run fails closed before
        //     any runner mutation, with an ownership diagnostic.
        submit_plan(&test_plan(), Some("agent-task-owned-8447")).expect("agent-task run submitted");
        let collision = record_runner_exec_job_identity(
            "agent-task-owned-8447",
            "homeboy-lab",
            "job-3",
            "/runner/workspace/homeboy",
            &command,
        )
        .expect_err("reusing an agent-task id as a generic runner-exec run must fail closed");
        assert_eq!(collision.code, ErrorCode::ValidationInvalidArgument);
        assert!(
            collision
                .message
                .contains("already exists as an agent-task run"),
            "ownership diagnostic should name the conflicting agent-task owner: {}",
            collision.message
        );
    });
}

#[test]
fn diagnostic_ssh_run_id_creates_generic_run_without_a_runner_job() {
    // #9485: the diagnostic-SSH transport executes synchronously and never
    // accepts a durable runner job, but `runner exec --ssh --run-id <new-id>`
    // with declared `--artifact`/`--summary` still needs a persisted run to
    // attach evidence to. A new ad hoc ID must create a generic runner-exec run
    // on demand even with no runner job, restoring #8447 for the SSH path.
    with_isolated_home(|_| {
        let command = vec!["node".to_string(), "fuzz.mjs".to_string()];

        // (1) A new valid ad hoc ID creates a generic run with no runner_job_id.
        let created = ensure_generic_runner_exec_run(
            "fisiostetic-image-specificity-v4",
            "homeboy-lab",
            "/runner/workspace/dirty",
            &command,
        )
        .expect("new ad hoc ssh run id creates a generic runner-exec run");
        assert_eq!(created.metadata["kind"], RUNNER_EXEC_RUN_KIND);
        assert_eq!(created.metadata["runner_id"], "homeboy-lab");
        assert!(
            created.metadata.get("runner_job_id").is_none(),
            "diagnostic-SSH run has no accepted runner job"
        );

        // The run is a real durable record — declared artifacts can attach.
        let loaded = status("fisiostetic-image-specificity-v4").expect("generic run persisted");
        assert_eq!(loaded.metadata["kind"], RUNNER_EXEC_RUN_KIND);

        // (2) Re-running the same ad hoc ID is idempotent (reuses the run).
        let reused = ensure_generic_runner_exec_run(
            "fisiostetic-image-specificity-v4",
            "homeboy-lab",
            "/runner/workspace/dirty",
            &command,
        )
        .expect("existing generic ssh run id is reused");
        assert_eq!(reused.metadata["kind"], RUNNER_EXEC_RUN_KIND);

        // (3) An ID owned by an agent-task run fails closed with a typed
        //     ownership diagnostic before any runner mutation.
        submit_plan(&test_plan(), Some("agent-task-owned-9485")).expect("agent-task run submitted");
        let collision = ensure_generic_runner_exec_run(
            "agent-task-owned-9485",
            "homeboy-lab",
            "/runner/workspace/dirty",
            &command,
        )
        .expect_err("reusing an agent-task id as a generic runner-exec run must fail closed");
        assert_eq!(collision.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(collision.details["field"], "run_id");
        assert!(collision
            .message
            .contains("already exists as an agent-task run"));
        assert!(collision.details["tried"]
            .as_array()
            .is_some_and(|tried| tried.iter().any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("distinct --run-id")))));
    });
}

#[test]
fn generic_runner_exec_run_supports_artifact_attachment() {
    // The end-to-end contract #9485 restores: once the generic run exists, the
    // declared evidence attaches to it (previously `run record not found`).
    with_isolated_home(|_| {
        use homeboy_core::observation::ObservationStore;

        ensure_generic_runner_exec_run(
            "adhoc-evidence-9485",
            "homeboy-lab",
            "/runner/workspace",
            &["node".to_string(), "fuzz.mjs".to_string()],
        )
        .expect("generic run created");

        let store = ObservationStore::open_initialized().expect("store");
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = temp.path().join("summary.json");
        std::fs::write(&summary, r#"{"status":"fail","findings":3}"#).expect("write summary");

        let artifact = store
            .record_artifact_with_metadata(
                "adhoc-evidence-9485",
                "summary",
                &summary,
                serde_json::json!({ "promoted_by": "runner.exec" }),
            )
            .expect("declared summary attaches to the generic run");
        assert_eq!(artifact.run_id, "adhoc-evidence-9485");
    });
}
