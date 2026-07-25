//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup):
//! generic runner-exec run creation and identity (#9927).
#![cfg(test)]

use super::*;
use homeboy_core::api_jobs::{Job, JobEvent, JobEventKind, JobStatus, RunnerJobLogSnapshot};
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
        assert_eq!(created.kind, "runner_execution");
        assert_eq!(created.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);
        assert_eq!(created.metadata_json["runner_id"], "homeboy-lab");
        assert_eq!(created.metadata_json["runner_job_id"], "job-1");
        assert!(created.metadata_json.get("agent_task_run").is_none());

        // The generic run is a real durable record without agent-task metadata.
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let loaded = store
            .get_run("recovery-8447-lab-build-r3")
            .expect("read run")
            .expect("generic run persisted");
        assert_eq!(loaded.kind, "runner_execution");
        assert_eq!(loaded.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);
        assert!(loaded.metadata_json.get("agent_task_run").is_none());

        // (2) Reusing the same generic ID re-attaches without error.
        let reused = record_runner_exec_job_identity(
            "recovery-8447-lab-build-r3",
            "homeboy-lab",
            "job-2",
            "/runner/workspace/homeboy",
            &command,
        )
        .expect("existing generic run id re-binds");
        assert_eq!(reused.metadata_json["runner_job_id"], "job-2");

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
        assert_eq!(created.kind, "runner_execution");
        assert_eq!(created.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);
        assert_eq!(created.metadata_json["runner_id"], "homeboy-lab");
        assert!(
            created.metadata_json.get("runner_job_id").is_none(),
            "diagnostic-SSH run has no accepted runner job"
        );
        assert!(created.metadata_json.get("agent_task_run").is_none());

        // The run is a real durable record — declared artifacts can attach.
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let loaded = store
            .get_run("fisiostetic-image-specificity-v4")
            .expect("read run")
            .expect("generic run persisted");
        assert_eq!(loaded.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);
        assert!(loaded.metadata_json.get("agent_task_run").is_none());

        // (2) Re-running the same ad hoc ID is idempotent (reuses the run).
        let reused = ensure_generic_runner_exec_run(
            "fisiostetic-image-specificity-v4",
            "homeboy-lab",
            "/runner/workspace/dirty",
            &command,
        )
        .expect("existing generic ssh run id is reused");
        assert_eq!(reused.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);

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

#[test]
fn generic_runner_exec_terminal_projection_is_authoritative_and_idempotent() {
    with_isolated_home(|_| {
        let command = vec!["node".to_string(), "fuzz.mjs".to_string()];
        for (run_id, status, expected_status) in [
            ("runner-projection-success", "succeeded", "pass"),
            ("runner-projection-failure", "failed", "fail"),
            ("runner-projection-cancel", "cancelled", "fail"),
        ] {
            record_runner_exec_job_identity(
                run_id,
                "homeboy-lab",
                "00000000-0000-0000-0000-000000000123",
                "/runner/workspace",
                &command,
            )
            .expect("generic run bound to daemon job");
            record_runner_exec_artifact_declarations(
                run_id,
                &["case-log.jsonl".to_string()],
                &["artifacts".to_string()],
                &["results.json".to_string()],
            )
            .expect("declared artifact contract persists before execution");

            let snapshot = runner_snapshot(status);
            record_runner_exec_terminal_checkpoint(run_id, &snapshot)
                .expect("terminal snapshot checkpoint persists before promotion");
            record_runner_exec_artifact_refs(run_id, &[])
                .expect("empty declared promotion completes before terminal projection");
            assert!(project_terminal_runner_exec_result(run_id, &snapshot)
                .expect("terminal daemon result projects"));
            assert!(!project_terminal_runner_exec_result(run_id, &snapshot)
                .expect("duplicate terminal projection is ignored"));

            let store =
                homeboy_core::observation::ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(run_id)
                .expect("read run")
                .expect("projected run");
            assert_eq!(run.status, expected_status);
            assert!(run.finished_at.is_some());
            assert_eq!(
                run.metadata_json["runner_terminal_projection"]["state"],
                "projected"
            );
            assert_eq!(
                run.metadata_json["runner_execution_record"]["status"],
                status
            );
            assert_eq!(
                run.metadata_json["runner_exec_artifact_declarations"]["summaries"][0],
                "results.json"
            );
            if status != "succeeded" {
                assert_eq!(
                    run.metadata_json["runner_failure_diagnostics"]["job_status"],
                    status
                );
            }
        }
    });
}

#[test]
fn generic_runner_exec_rejects_stale_terminal_snapshot_binding() {
    with_isolated_home(|_| {
        let command = vec!["true".to_string()];
        record_runner_exec_job_identity(
            "runner-projection-stale",
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &command,
        )
        .expect("bound run");
        let mut stale = runner_snapshot("succeeded");
        stale.job.id =
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000124").expect("stale job id");
        let error = project_terminal_runner_exec_result("runner-projection-stale", &stale)
            .expect_err("delayed terminal snapshot is rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run("runner-projection-stale")
            .expect("read")
            .expect("run");
        assert_eq!(run.status, "running");
    });
}

#[test]
fn synchronous_diagnostic_ssh_and_local_runs_finish_with_artifacts_and_replay_safely() {
    with_isolated_home(|_| {
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let temp = tempfile::NamedTempFile::new().expect("artifact");
        std::fs::write(temp.path(), "diagnostic output").expect("write artifact");
        for (run_id, transport, exit_code, expected_status) in [
            ("diagnostic-ssh-success", "diagnostic_ssh", 0, "pass"),
            ("local-failure", "local", 17, "fail"),
        ] {
            ensure_generic_runner_exec_run(run_id, "runner", "/workspace", &["true".to_string()])
                .expect("synchronous run");
            record_runner_exec_artifact_declarations(
                run_id,
                &["diagnostic.log".to_string()],
                &[],
                &[],
            )
            .expect("artifact declaration");
            let artifact = store
                .record_artifact_with_id(
                    run_id,
                    "diagnostic_log",
                    temp.path(),
                    &format!("{run_id}-artifact"),
                    serde_json::json!({ "promoted_by": "runner.exec" }),
                )
                .expect("artifact retained before direct terminal projection");
            record_runner_exec_artifact_refs(run_id, &[artifact]).expect("artifact refs");
            assert!(
                finish_runner_exec_direct(run_id, transport, exit_code).expect("direct terminal")
            );
            assert!(
                !finish_runner_exec_direct(run_id, transport, exit_code).expect("restart replay")
            );
            let run = store.get_run(run_id).expect("read").expect("run");
            assert_eq!(run.status, expected_status);
            assert_eq!(
                run.metadata_json["runner_execution_record"]["transport"],
                transport
            );
            assert_eq!(store.list_artifacts(run_id).expect("artifacts").len(), 1);
        }
    });
}

fn runner_snapshot(status: &str) -> RunnerJobLogSnapshot {
    let job_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").expect("job id");
    let status = match status {
        "succeeded" => JobStatus::Succeeded,
        "failed" => JobStatus::Failed,
        "cancelled" => JobStatus::Cancelled,
        _ => panic!("terminal status"),
    };
    RunnerJobLogSnapshot {
        job: Job {
            id: job_id,
            operation: "exec".to_string(),
            status,
            created_at_ms: 1,
            updated_at_ms: 2,
            started_at_ms: Some(1),
            finished_at_ms: Some(2),
            event_count: 1,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: Some("homeboy-lab".to_string()),
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        },
        events: vec![JobEvent {
            sequence: 1,
            job_id,
            kind: if status == JobStatus::Succeeded {
                JobEventKind::Result
            } else {
                JobEventKind::Error
            },
            timestamp_ms: 2,
            message: Some("terminal result".to_string()),
            data: Some(serde_json::json!({
                "exit_code": if status == JobStatus::Succeeded { 0 } else { 1 },
                "classification": "timeout",
            })),
        }],
    }
}
