//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup):
//! generic runner-exec run creation and identity (#9927).
#![cfg(test)]

use super::*;
use homeboy_core::api_jobs::{
    Job, JobEvent, JobEventKind, JobStatus, RunnerJobLogSnapshot, RunnerJobProjection,
};
use homeboy_core::test_support::with_isolated_home;

/// The tests below drive the store-rooted lifecycle entry points. Resolving the
/// store once here keeps the ambient lookup in a single place instead of at
/// every call site, and lets the ambient wrappers be deleted (#7505).
fn test_lifecycle_store() -> AgentTaskLifecycleStore {
    AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store")
}

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
fn generic_runner_exec_adopts_and_reuses_a_direct_runner_exec_run() {
    with_isolated_home(|_| {
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let direct = store
            .start_run(
                homeboy_core::observation::NewRunRecord::builder("runner-exec")
                    .command("homeboy runner exec lab".to_string())
                    .build(),
            )
            .expect("direct runner exec run");
        let command = vec!["true".to_string()];

        let adopted = ensure_generic_runner_exec_run(&direct.id, "lab", "/workspace", &command)
            .expect("adopt direct runner exec run");
        let reused = ensure_generic_runner_exec_run(&direct.id, "lab", "/workspace", &command)
            .expect("repeat direct runner exec run");

        assert_eq!(adopted.id, direct.id);
        assert_eq!(reused.id, direct.id);
        assert_eq!(reused.metadata_json["kind"], RUNNER_EXEC_RUN_KIND);
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
fn pre_handoff_connection_failure_is_terminal_local_evidence_without_a_runner_job() {
    with_isolated_home(|_| {
        let run_id = "runner-exec-connection-failure";
        ensure_generic_runner_exec_run(
            run_id,
            "homeboy-lab",
            "/runner/workspace",
            &[
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
        )
        .expect("persist attempt before connection");
        record_runner_exec_pre_handoff_phase(run_id, "connection").expect("record phase");
        let error = Error::new(
            ErrorCode::InternalUnexpected,
            "new tunnel exited before its process identity was captured: Authorization: Bearer abc.def.ghi",
            serde_json::json!({ "token": "secretvalue123" }),
        );

        assert!(finish_runner_exec_pre_handoff_failure(
            run_id,
            "pre_handoff",
            "connection",
            false,
            &error
        )
        .expect("terminalize connection failure"));

        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run = store.get_run(run_id).expect("read").expect("persisted run");
        assert_eq!(run.status, "fail");
        assert!(run.finished_at.is_some());
        assert_eq!(run.metadata_json["runner_exec_phase"], "connection");
        assert_eq!(
            run.metadata_json["runner_pre_handoff_failure"]["code"],
            "internal.unexpected"
        );
        assert!(!run.metadata_json["runner_pre_handoff_failure"]
            .to_string()
            .contains("abc.def.ghi"));
        assert_eq!(
            run.metadata_json["runner_pre_handoff_failure"]["details"]["token"],
            "[REDACTED]"
        );
        assert!(run.metadata_json.get("runner_job_id").is_none());
        assert!(store.list_artifacts(run_id).expect("artifacts").is_empty());
        assert_eq!(
            run.metadata_json["runner_terminal_projection"]["state"],
            "pre_handoff_failed"
        );
        assert!(
            run.metadata_json["runner_pre_handoff_failure"]["recovery"]["evidence"]
                .as_str()
                .expect("evidence command")
                .contains(run_id)
        );
        assert!(
            run.metadata_json["runner_pre_handoff_failure"]["recovery"]["retry"]
                .as_str()
                .expect("retry command")
                .contains("--run-id <new-run-id>")
        );
    });
}

#[test]
fn duplicate_pre_handoff_failure_is_idempotent() {
    with_isolated_home(|_| {
        let run_id = "runner-exec-duplicate-pre-handoff";
        let command = vec!["homeboy".to_string(), "review".to_string()];
        ensure_generic_runner_exec_run(run_id, "homeboy-lab", "/runner/workspace", &command)
            .expect("first attempt");
        let error = Error::internal_unexpected("connection refused");
        assert!(finish_runner_exec_pre_handoff_failure(
            run_id,
            "pre_handoff",
            "connection",
            false,
            &error
        )
        .expect("first terminal failure"));

        let duplicate =
            ensure_generic_runner_exec_run(run_id, "homeboy-lab", "/runner/workspace", &command)
                .expect("duplicate lookup reuses the persisted attempt");
        assert_eq!(duplicate.status, "fail");
        assert!(!finish_runner_exec_pre_handoff_failure(
            run_id,
            "pre_handoff",
            "connection",
            false,
            &error
        )
        .expect("duplicate terminalization is ignored"));

        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run = store.get_run(run_id).expect("read").expect("run");
        assert!(run.metadata_json.get("runner_job_id").is_none());
        assert!(store.list_artifacts(run_id).expect("artifacts").is_empty());
    });
}

#[test]
fn accepted_handoff_marker_prevents_pre_handoff_terminalization() {
    with_isolated_home(|_| {
        let run_id = "runner-exec-accepted-but-unbound";
        ensure_generic_runner_exec_run(run_id, "homeboy-lab", "/runner/workspace", &[])
            .expect("persist attempt");
        let error = Error::internal_unexpected("controller binding failed");

        assert!(!finish_runner_exec_pre_handoff_failure(
            run_id,
            "pre_handoff",
            "handoff",
            true,
            &error
        )
        .expect("accepted handoff remains non-terminal"));

        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run(run_id)
            .expect("read")
            .expect("run");
        assert_eq!(run.status, "running");
        assert!(run
            .metadata_json
            .get("runner_pre_handoff_failure")
            .is_none());
    });
}

#[test]
fn accepted_binding_wins_the_pre_handoff_terminalization_race() {
    with_isolated_home(|_| {
        let run_id = "runner-exec-accepted-race";
        let command = vec!["homeboy".to_string(), "review".to_string()];
        ensure_generic_runner_exec_run(run_id, "homeboy-lab", "/runner/workspace", &command)
            .expect("persist attempt");

        // This mirrors the race between a controller that has already built a
        // failure payload and a binding write that wins before its CAS update.
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let bound = std::sync::Arc::new(std::sync::Barrier::new(2));
        let bind_ready = ready.clone();
        let bind_done = bound.clone();
        let binding = std::thread::spawn(move || {
            bind_ready.wait();
            record_runner_exec_job_identity(
                run_id,
                "homeboy-lab",
                "accepted-job",
                "/runner/workspace",
                &command,
            )
            .expect("accepted binding");
            bind_done.wait();
        });
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run = store.get_run(run_id).expect("read").expect("attempt");
        let failure = serde_json::json!({ "phase": "handoff", "code": "connection", "message": "lost", "details": {}, "recovery": {} });
        let projection = serde_json::json!({ "state": "pre_handoff_failed" });
        let execution = serde_json::to_value(
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::terminal(
                run.id,
                "homeboy-lab",
                "pre_handoff",
                1,
            ),
        )
        .expect("execution record");
        ready.wait();
        bound.wait();

        assert!(!store
            .finish_running_runner_exec_pre_handoff_failure(run_id, failure, projection, execution,)
            .expect("CAS terminalization"));
        binding.join().expect("binding thread");
        let run = store.get_run(run_id).expect("read").expect("run");
        assert_eq!(run.status, "running");
        assert_eq!(run.metadata_json["runner_job_id"], "accepted-job");
        assert!(run
            .metadata_json
            .get("runner_pre_handoff_failure")
            .is_none());
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
            record_runner_exec_artifact_refs_in_store(&test_lifecycle_store(), run_id, &[])
                .expect("empty declared promotion completes before terminal projection");
            assert!(project_terminal_runner_exec_result_in_store(
                &test_lifecycle_store(),
                run_id,
                &snapshot
            )
            .expect("terminal daemon result projects"));
            assert!(!project_terminal_runner_exec_result_in_store(
                &test_lifecycle_store(),
                run_id,
                &snapshot
            )
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
fn generic_runner_exec_preserves_submission_provenance_on_failure() {
    with_isolated_home(|_| {
        let run_id = "runner-provenance-before-spawn";
        record_runner_exec_job_identity(
            run_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &["homeboy".to_string(), "bench".to_string()],
        )
        .expect("bound run");
        let record = homeboy_core::runner_execution_envelope::RunnerExecutionRecord::planned(
            run_id,
            "homeboy-lab",
            "dispatch",
        )
        .with_orchestration_provenance(Some(
            homeboy_core::runner_execution_envelope::OrchestrationTargetProvenance::new(
                "homeboy-lab",
                homeboy_core::runner_execution_envelope::BinaryProvenance {
                    owner: "controller".to_string(),
                    path: Some("/usr/local/bin/homeboy".to_string()),
                    version: Some("0.334.0".to_string()),
                    build_identity: Some("homeboy 0.334.0+controller".to_string()),
                },
                homeboy_core::runner_execution_envelope::BinaryProvenance {
                    owner: "daemon".to_string(),
                    path: Some("http://127.0.0.1:3000".to_string()),
                    version: Some("0.334.0".to_string()),
                    build_identity: Some("homeboy 0.334.0+daemon".to_string()),
                },
                homeboy_core::runner_execution_envelope::BinaryProvenance {
                    owner: "runner configuration".to_string(),
                    path: Some("/opt/homeboy".to_string()),
                    version: None,
                    build_identity: None,
                },
            ),
        ));
        record_runner_exec_execution_record(run_id, &record)
            .expect("submission provenance persists");
        record_runner_exec_artifact_refs_in_store(&test_lifecycle_store(), run_id, &[])
            .expect("complete artifact projection");
        project_terminal_runner_exec_result_in_store(
            &test_lifecycle_store(),
            run_id,
            &runner_snapshot("failed"),
        )
        .expect("failed terminal result projects");

        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run(run_id)
            .expect("read")
            .expect("run");
        assert_eq!(
            run.metadata_json["runner_execution_record"]["status"],
            "failed"
        );
        assert_eq!(
            run.metadata_json["runner_execution_record"]["orchestration_provenance"]
                ["runner_daemon_binary"]["build_identity"],
            "homeboy 0.334.0+daemon"
        );
        assert!(
            run.metadata_json["runner_execution_record"]["orchestration_provenance"]
                ["runner_command_binary"]["version"]
                .is_null()
        );
    });
}

#[test]
fn terminal_runner_exec_projects_only_complete_valid_nested_run_references() {
    with_isolated_home(|_| {
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let child = store
            .start_run(homeboy_core::observation::NewRunRecord::builder("bench").build())
            .expect("child run");
        let diagnostic = tempfile::NamedTempFile::new().expect("diagnostic");
        std::fs::write(diagnostic.path(), "child failure").expect("write diagnostic");
        store
            .record_artifact_with_metadata(
                &child.id,
                "failure_diagnostic",
                diagnostic.path(),
                serde_json::json!({ "failure_diagnostic": true, "failure_diagnostic_rank": 1 }),
            )
            .expect("child diagnostic");
        store
            .finish_run(&child.id, homeboy_core::observation::RunStatus::Fail, None)
            .expect("finish child");

        let run_id = "outer-nested-command-result";
        record_runner_exec_job_identity(
            run_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &["homeboy".to_string(), "bench".to_string()],
        )
        .expect("outer run");
        record_runner_exec_artifact_refs_in_store(&test_lifecycle_store(), run_id, &[])
            .expect("complete artifact projection");
        let mut snapshot = runner_snapshot("succeeded");
        snapshot.events[0].data = Some(serde_json::json!({
            "stdout": serde_json::json!({
                "schema": "homeboy/command-result/v3",
                "command": "bench",
                "success": true,
                "exit_code": 0,
                "status": "succeeded",
                "refs": { "runs": [{
                    "id": child.id,
                    "kind": "bench",
                    "source": "forged-runner-label"
                }] }
            }).to_string()
        }));
        project_terminal_runner_exec_result_in_store(&test_lifecycle_store(), run_id, &snapshot)
            .expect("project outer run");

        let outer = store
            .get_run(run_id)
            .expect("read outer")
            .expect("outer run");
        assert_eq!(outer.status, "pass");
        assert_eq!(
            outer.metadata_json["descendant_run_evidence"][0]["run_id"],
            child.id
        );
        assert_eq!(
            outer.metadata_json["descendant_run_evidence"][0]["source"],
            homeboy_core::observation::evidence_report::DESCENDANT_RUN_EVIDENCE_SOURCE_TERMINAL_COMMAND_RESULT
        );
        assert!(store
            .list_artifacts(run_id)
            .expect("outer artifacts")
            .is_empty());

        let malformed =
            crate::agent_task_lifecycle::runner_exec::terminal_command_result_descendants(
                &store,
                run_id,
                &RunnerJobLogSnapshot {
                    events: vec![JobEvent {
                        data: Some(serde_json::json!({
                            "capture": { "stdout": { "truncated": true } },
                            "stdout": "{\"schema\":\"homeboy/command-result/v3\"}"
                        })),
                        ..snapshot.events[0].clone()
                    }],
                    ..snapshot.clone()
                },
            );
        assert!(malformed.is_none());

        let adversarial =
            crate::agent_task_lifecycle::runner_exec::terminal_command_result_descendants(
                &store,
                run_id,
                &RunnerJobLogSnapshot {
                    events: vec![JobEvent {
                        data: Some(serde_json::json!({
                            "stdout": serde_json::json!({
                                "schema": "homeboy/command-result/v3",
                                "command": "bench",
                                "success": true,
                                "exit_code": 0,
                                "status": "succeeded",
                                "refs": { "runs": [{
                                    "id": "untrusted-child",
                                    "kind": "bench",
                                    "source": "attacker"
                                }] }
                            }).to_string()
                        })),
                        ..snapshot.events[0].clone()
                    }],
                    ..snapshot
                },
            );
        assert!(adversarial.is_none());
    });
}

#[test]
fn terminal_runner_exec_rejects_indirect_descendant_cycles() {
    with_isolated_home(|_| {
        let parent_id = "cycle-parent";
        record_runner_exec_job_identity(
            parent_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &["homeboy".to_string(), "bench".to_string()],
        )
        .expect("parent run");
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let child = store
            .start_run(homeboy_core::observation::NewRunRecord::builder("bench").build())
            .expect("child run");
        store
            .update_run_metadata(
                &child.id,
                serde_json::json!({
                    "descendant_run_evidence": [{
                        "schema": "homeboy/descendant-run-evidence-ref/v1",
                        "run_id": parent_id,
                        "kind": "runner_execution",
                        "source": "controller.terminal_command_result.refs.runs"
                    }]
                }),
            )
            .expect("record child edge");
        let mut snapshot = runner_snapshot("succeeded");
        snapshot.events[0].data = Some(serde_json::json!({
            "stdout": serde_json::json!({
                "schema": "homeboy/command-result/v3",
                "command": "bench",
                "success": true,
                "exit_code": 0,
                "status": "succeeded",
                "refs": { "runs": [{
                    "id": child.id,
                    "kind": "bench",
                    "source": "forged"
                }] }
            }).to_string()
        }));

        assert!(terminal_command_result_descendants(&store, parent_id, &snapshot).is_none());
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
        let error = project_terminal_runner_exec_result_in_store(
            &test_lifecycle_store(),
            "runner-projection-stale",
            &stale,
        )
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
fn generic_runner_exec_accepts_exact_direct_job_projection_binding() {
    with_isolated_home(|_| {
        let run_id = "runner-projection-direct";
        record_runner_exec_job_identity(
            run_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &["composer".to_string(), "install".to_string()],
        )
        .expect("bound direct run");
        let mut snapshot = runner_snapshot("succeeded");
        snapshot.job.target_runner_id = None;
        snapshot.job.runner_job_projection = Some(RunnerJobProjection {
            runner_id: "other-runner".to_string(),
            command: "composer install".to_string(),
            cwd: Some("/runner/workspace".to_string()),
            source: "runner-daemon".to_string(),
            kind: "runner.exec".to_string(),
            lifecycle: None,
        });

        let error = project_terminal_runner_exec_result_in_store(
            &test_lifecycle_store(),
            run_id,
            &snapshot,
        )
        .expect_err("mismatched direct runner projection is rejected");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);

        snapshot
            .job
            .runner_job_projection
            .as_mut()
            .expect("projection")
            .runner_id = "homeboy-lab".to_string();
        project_terminal_runner_exec_result_in_store(&test_lifecycle_store(), run_id, &snapshot)
            .expect("exact direct runner projection is accepted");
        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run(run_id)
            .expect("read")
            .expect("run");
        assert_eq!(run.status, "pass");
    });
}

#[test]
fn generic_runner_exec_projection_failure_retains_terminal_evidence() {
    with_isolated_home(|_| {
        let run_id = "runner-projection-failure-evidence";
        record_runner_exec_job_identity(
            run_id,
            "homeboy-lab",
            "00000000-0000-0000-0000-000000000123",
            "/runner/workspace",
            &["true".to_string()],
        )
        .expect("bound generic run");
        let snapshot = runner_snapshot("failed");
        let error = Error::internal_unexpected("artifact projection transport failed");

        record_runner_exec_projection_failure(run_id, &snapshot, &error)
            .expect("terminal snapshot and projection error persist");

        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run(run_id)
            .expect("read")
            .expect("run");
        assert_eq!(run.status, "running");
        assert_eq!(
            run.metadata_json["runner_terminal_projection"]["state"],
            "projection_failed"
        );
        assert_eq!(
            run.metadata_json["runner_terminal_projection"]["job_id"],
            snapshot.job.id.to_string()
        );
        assert_eq!(
            run.metadata_json["runner_terminal_projection"]["event_count"],
            snapshot.events.len()
        );
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
            record_runner_exec_artifact_refs_in_store(&test_lifecycle_store(), run_id, &[artifact])
                .expect("artifact refs");
            assert!(finish_runner_exec_direct_in_store(
                &test_lifecycle_store(),
                run_id,
                transport,
                exit_code
            )
            .expect("direct terminal"));
            assert!(!finish_runner_exec_direct_in_store(
                &test_lifecycle_store(),
                run_id,
                transport,
                exit_code
            )
            .expect("restart replay"));
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

#[test]
fn declaration_replay_uses_literal_path_and_tilde_keys() {
    with_isolated_home(|_| {
        let run_id = "escaped-declaration-recovery";
        ensure_generic_runner_exec_run(run_id, "runner", "/workspace", &["true".to_string()])
            .expect("run");
        let declarations = ["artifacts/result.json", "a~b/c"];
        record_runner_exec_artifact_declarations(
            run_id,
            &declarations
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
            &[],
            &[],
        )
        .expect("declarations");
        for declaration in declarations {
            record_runner_exec_declaration_promotion_in_store(
                &test_lifecycle_store(),
                run_id,
                "artifact",
                declaration,
                &[],
            )
            .expect("promotion checkpoint");
            // A duplicate recovery writes the same key and must remain visible.
            record_runner_exec_declaration_promotion_in_store(
                &test_lifecycle_store(),
                run_id,
                "artifact",
                declaration,
                &[],
            )
            .expect("duplicate checkpoint");
        }
        let run = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .get_run(run_id)
            .expect("read")
            .expect("run");
        for declaration in declarations {
            assert!(runner_exec_declaration_is_promoted(
                &run,
                "artifact",
                declaration,
            ));
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
