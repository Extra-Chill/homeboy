//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskExecutionHandle, AgentTaskExecutor,
    AgentTaskLimits, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence, AgentTaskWorkflowStepStatus,
    AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use homeboy_core::api_jobs::{Job, JobEvent, JobEventKind, JobStore, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[test]
fn provider_run_result_reads_declared_output_alias() {
    let role_aliases: AgentTaskProviderRoleAliases = serde_json::from_value(json!({
        "outputs": {
            "provider_run_result": ["custom_run_result"]
        }
    }))
    .expect("role aliases");
    let outcome = AgentTaskOutcome {
        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-a".to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: json!({
            "custom_run_result": {
                "run_id": "custom-run-1"
            }
        }),
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };

    assert_eq!(
        provider_run_result(&outcome, &role_aliases)
            .and_then(|result| result.get("run_id"))
            .and_then(Value::as_str),
        Some("custom-run-1")
    );
}

#[cfg(unix)]
#[test]
fn legacy_v1_pin_migration_failures_leave_durable_record_unchanged() {
    with_isolated_home(|_| {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let record = submit_plan(&test_plan(), Some("migration-failure")).expect("submit");
        let cases = [
            (
                "missing",
                temporary.path().join("missing-homeboy"),
                None,
                "missing",
            ),
            (
                "non-executable",
                temporary.path().join("non-executable-homeboy"),
                Some(identity.clone()),
                "not executable",
            ),
            (
                "identity-mismatch",
                temporary.path().join("wrong-identity-homeboy"),
                Some("homeboy test+wrong".to_string()),
                "build identity mismatch",
            ),
        ];

        for (name, legacy, artifact_identity, expected_error) in cases {
            if let Some(artifact_identity) = artifact_identity {
                fake_controller_artifact(&legacy, &artifact_identity, name);
                if name == "non-executable" {
                    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600))
                        .expect("remove executable permission");
                }
            }
            rewrite_record_for_test(&record.run_id, |record| {
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                    "originating": {
                        "build_identity": identity,
                        "pinned_executable": legacy,
                    }
                });
            })
            .expect("project v1 legacy pin");
            let before = status(&record.run_id).expect("record before migration");

            let error = validate_controller_runtime(&record.run_id)
                .expect_err("legacy migration fails closed");

            assert!(
                error.message.contains(expected_error),
                "{name}: {}",
                error.message
            );
            assert_eq!(
                status(&record.run_id).expect("record after migration"),
                before
            );
        }
    });
}

#[cfg(unix)]
#[test]
fn submit_plan_persists_owner_only_plan_file_before_observation() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let record = submit_plan(&test_plan(), Some("private-plan")).expect("submitted");

        assert_eq!(
            std::fs::metadata(&record.plan_path)
                .expect("plan metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            status(&record.run_id)
                .expect("observation record")
                .plan_path,
            record.plan_path
        );
    });
}

#[test]
fn detached_lab_run_plan_uses_one_identity_for_status_logs_artifacts_and_cancellation() {
    with_isolated_home(|_| {
        let run_id = "agent-task-detached-run-plan";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--record-run-id".to_string(),
            run_id.to_string(),
        ];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8341",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("detached run-plan is bound to the controller run");

        let status = status(run_id).expect("controller identity resolves status");
        let logs = logs(run_id).expect("controller identity resolves logs");
        let artifacts = artifacts(run_id).expect("controller identity resolves artifacts");
        assert_eq!(status.run_id, run_id);
        assert_eq!(status.metadata["runner_job_id"], "job-8341");
        assert!(!logs.events.is_empty());
        assert!(artifacts.artifacts.is_empty());

        let _cancel = super::cancellation::test_cancel_hook::install(Box::new(
            |runner_id, job_id, durable_run_id| {
                assert_eq!(runner_id, "homeboy-lab");
                assert_eq!(job_id, "job-8341");
                assert_eq!(durable_run_id, "agent-task-detached-run-plan");
                Ok((
                    homeboy_core::api_jobs::Job {
                        id: uuid::Uuid::new_v4(),
                        operation: "runner.exec".to_string(),
                        status: homeboy_core::api_jobs::JobStatus::Cancelled,
                        created_at_ms: 1,
                        updated_at_ms: 2,
                        started_at_ms: Some(1),
                        finished_at_ms: Some(2),
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
                    },
                    Vec::new(),
                ))
            },
        ));
        let cancelled = cancel_run(run_id, Some("operator requested cancellation"))
            .expect("canonical cancellation reaches the runner job");
        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            cancelled.metadata["live_cancellation"]["cancellation"],
            "runner_job_cancel"
        );
    });
}

#[test]
fn cancelling_queued_runner_proxy_projects_to_accepted_daemon_job() {
    with_isolated_home(|_| {
        let run_id = "agent-task-queued-runner-proxy";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("queued controller proxy");
        record_runner_job_identity(run_id, "homeboy-lab", "job-pre-provider")
            .expect("persist accepted daemon job identity");

        let _cancel = super::cancellation::test_cancel_hook::install(Box::new(
            |runner_id, job_id, durable_run_id| {
                assert_eq!(runner_id, "homeboy-lab");
                assert_eq!(job_id, "job-pre-provider");
                assert_eq!(durable_run_id, "agent-task-queued-runner-proxy");
                Ok((
                    homeboy_core::api_jobs::Job {
                        id: uuid::Uuid::new_v4(),
                        operation: "runner.exec".to_string(),
                        status: homeboy_core::api_jobs::JobStatus::Cancelled,
                        created_at_ms: 1,
                        updated_at_ms: 2,
                        started_at_ms: None,
                        finished_at_ms: Some(2),
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
                    },
                    Vec::new(),
                ))
            },
        ));

        let cancelled = cancel_run(run_id, Some("controller aggregate unavailable"))
            .expect("cancellation reaches queued daemon job");
        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            cancelled.metadata["live_cancellation"]["cancellation"],
            "runner_job_cancel"
        );
    });
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
fn status_expires_an_unaccepted_handoff_but_late_runner_acceptance_wins() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("controller proxy recorded before handoff");
        rewrite_record_for_test("expired-handoff-late-acceptance", |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("typed handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("expire acceptance deadline");

        let expired = status("expired-handoff-late-acceptance")
            .expect("status reconciles the expired controller proxy");
        assert_eq!(expired.state, AgentTaskRunState::Cancelled);
        assert_eq!(expired.metadata["handoff_acceptance"]["state"], "expired");
        assert_eq!(expired.metadata["retryable"], true);

        let accepted = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            runner_job_id: "job-accepted-after-deadline",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("late acceptance supersedes only the synthetic expiry cancellation");
        assert_eq!(accepted.state, AgentTaskRunState::Running);
        assert!(accepted.has_accepted_lab_handoff());
        assert_eq!(accepted.metadata["handoff_acceptance"]["state"], "accepted");
        assert_eq!(
            accepted.metadata["runner_job_id"],
            "job-accepted-after-deadline"
        );
        assert_eq!(
            accepted.metadata["runner_execution_record"]["status"],
            "running"
        );
        let accepted_with_stale_deadline =
            rewrite_record_for_test("expired-handoff-late-acceptance", |record| {
                record
                    .lab_handoff
                    .as_mut()
                    .expect("typed accepted handoff")
                    .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
            })
            .expect("make the historical acceptance deadline stale");
        assert!(!accepted_with_stale_deadline.has_expired_pending_lab_handoff(chrono::Utc::now()));
    });
}

#[test]
fn non_retryable_pre_execution_failure_remains_invalid_input() {
    let plan = test_plan();
    let outcome = build_pre_execution_failure_outcome(
        "cook-invalid-input",
        &plan.tasks[0],
        "controller_admission",
        &Error::validation_invalid_argument("plan", "invalid input", None, None),
    );

    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
    assert_eq!(outcome.diagnostics[0].data["retryable"], false);
    assert_eq!(outcome.outputs["retryable"], false);
    assert_eq!(outcome.metadata["retryable"], false);
    assert_eq!(outcome.metadata["provider_executions_consumed"], 0);
}

#[test]
fn status_repairs_runner_plan_projection_and_missing_controller_plan_fails_closed() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let record =
            submit_plan(&plan, Some("runner-projected-status")).expect("controller plan submitted");
        rewrite_record_for_test(&record.run_id, |projected| {
            projected.plan_path =
                "/runner/agent-task-runs/runner-projected-status/plan.json".to_string();
        })
        .expect("runner transport path projected");

        assert_eq!(
            status(&record.run_id).expect("controller status").plan_path,
            record.plan_path
        );
        assert_eq!(
            store::read_record(&record.run_id)
                .expect("repaired record")
                .plan_path,
            record.plan_path
        );

        std::fs::remove_file(&record.plan_path).expect("remove controller plan");
        let error = status(&record.run_id).expect_err("missing controller plan fails closed");
        assert_eq!(error.code, ErrorCode::InternalIoError);
        let diagnostic = error.details["error"]
            .as_str()
            .expect("structured ownership diagnostic");
        assert!(diagnostic.contains("authoritative controller-owned plan"));
        assert!(diagnostic.contains("runner execution transport"));
    });
}

#[test]
fn slow_materialization_remains_discoverable_with_source_identity_and_is_idempotent() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let mut durable_plan = test_plan();
        durable_plan.tasks[0].task_id = "https://github.com/example/project/issues/42".to_string();
        durable_plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: "https://github.com/example/project/issues/42".to_string(),
            revision: None,
        }];
        let started = std::time::Instant::now();
        let first = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        })
        .expect("proxy persisted before staging");

        // Deliberately exceed a caller's short wait budget after the durable
        // write, as a workspace/dependency materializer can in production.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(started.elapsed() > std::time::Duration::from_millis(1));

        let visible = status("slow-materialization").expect("immediately discoverable");
        assert_eq!(visible.run_id, first.run_id);
        assert_eq!(
            visible.tasks[0].task_id,
            "https://github.com/example/project/issues/42"
        );

        let resumed = record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        })
        .expect("resume does not duplicate staging record");
        assert_eq!(resumed.run_id, first.run_id);
        let persisted = load_plan("slow-materialization").expect("one persisted plan");
        assert_eq!(persisted.tasks.len(), 1);
        assert_eq!(
            persisted.tasks[0].source_refs[0].uri,
            "https://github.com/example/project/issues/42"
        );

        let with_child = record_lab_offload_phase_executions(
            "slow-materialization",
            "hydrating",
            ["runner-job-42".to_string()],
        )
        .expect("child staging job recorded");
        assert_eq!(
            with_child.metadata["materialization_execution_ids"],
            json!(["runner-job-42"])
        );
    });
}

#[test]
fn disconnected_proxy_projects_terminal_child_aggregate_once_reachable() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let mut record = status("agent-task-disconnected-child").expect("status");
        assert!(is_accepted_runner_handoff(&record));
        assert_eq!(
            record.metadata["runner_job_id"],
            "00000000-0000-0000-0000-000000000123"
        );
        assert!(record.metadata.get("pre_execution_failure").is_none());
        let mut running_snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        running_snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
        running_snapshot.events.clear();
        reconcile_runner_job_snapshot(&mut record, &running_snapshot)
            .expect("remote process heartbeat is retained");
        assert_eq!(record.metadata["phase"], "executing");
        let heartbeat = record
            .lifecycle
            .heartbeat
            .clone()
            .expect("confirmed heartbeat");

        record.annotate_runner_disconnected();
        assert_eq!(record.metadata["runner_liveness"], "disconnected");
        assert_eq!(
            record.metadata["stale_running_reason"],
            "runner_disconnected"
        );
        assert_eq!(record.lifecycle.heartbeat, Some(heartbeat));

        let child_plan = test_plan();
        let mut child_aggregate = succeeded_aggregate(&child_plan);
        child_aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
            "patch",
            "patch",
            None,
            Some("/runner/artifacts/patch.diff"),
        )];
        child_aggregate.outcomes[0].diagnostics = vec![AgentTaskDiagnostic {
            class: "provider.attempt".to_string(),
            message: "attempt 1 succeeded".to_string(),
            data: json!({ "attempt": 1 }),
        }];
        let snapshot = terminal_child_snapshot(&child_aggregate);

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("terminal reconciliation");
        let once = record.clone();
        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("repeated reconciliation");

        assert_eq!(
            record, once,
            "repeated terminal reconciliation is idempotent"
        );
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert!(is_accepted_runner_handoff(&record));
        assert_eq!(record.artifact_refs[0].uri, "/runner/artifacts/patch.diff");
        assert_eq!(record.metadata["runner_job_status"], "succeeded");
        assert_eq!(record.metadata["runner_liveness"], "reachable");
        let aggregate = store::read_aggregate("agent-task-disconnected-child")
            .expect("projected child aggregate");
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "provider.attempt"
        );
    });
}

#[test]
fn accepted_handoff_projects_a_remote_timeout_aggregate_even_when_daemon_transport_succeeds() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-remote-timeout",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("accepted handoff");
        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.status = AgentTaskAggregateStatus::Failed;
        aggregate.totals = AgentTaskAggregateTotals {
            timed_out: 1,
            ..Default::default()
        };
        aggregate.outcomes[0].status = AgentTaskOutcomeStatus::Timeout;
        aggregate.events[0].state = AgentTaskState::Failed;

        let mut snapshot = terminal_child_snapshot(&aggregate);
        let identity = snapshot.events[0]
            .data
            .as_mut()
            .and_then(|event| event.get_mut("identity"))
            .and_then(Value::as_object_mut)
            .expect("terminal lifecycle identity");
        identity.insert("persisted_run_id".to_string(), json!(record.run_id));
        identity.insert("run_id".to_string(), json!(record.run_id));

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect("remote timeout aggregate reconciles");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(
            record.totals.as_ref().map(|totals| totals.timed_out),
            Some(1)
        );
        assert_eq!(record.metadata["runner_job_status"], "succeeded");
    });
}

#[test]
fn terminal_projection_is_reader_complete_when_interrupted_after_commit_and_retry_is_idempotent() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        store::interrupt_after_terminal_commit_for_test();

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect_err("interruption after the committed envelope is surfaced");

        assert_eq!(
            store::read_record("agent-task-disconnected-child")
                .expect("committed controller projection")
                .state,
            AgentTaskRunState::Succeeded
        );
        let (status_record, log, artifacts) = std::thread::scope(|scope| {
            let status_reader = scope.spawn(|| status("agent-task-disconnected-child"));
            let log_reader = scope.spawn(|| logs("agent-task-disconnected-child"));
            let artifact_reader = scope.spawn(|| artifacts("agent-task-disconnected-child"));
            (
                status_reader
                    .join()
                    .expect("status reader")
                    .expect("committed status"),
                log_reader
                    .join()
                    .expect("log reader")
                    .expect("committed log"),
                artifact_reader
                    .join()
                    .expect("artifact reader")
                    .expect("committed artifacts"),
            )
        });
        assert_eq!(status_record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert!(artifacts.artifacts.is_empty());

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("idempotent retry");
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert!(store::aggregate_path(&record.run_id)
            .expect("aggregate path")
            .exists());
    });
}

#[test]
fn terminal_proxy_reconciliation_hydrates_persisted_dispatch_terminal_states() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        for (run_id, aggregate_status, outcome_status, expected_state) in [
            (
                "agent-task-persisted-dispatch-failure",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskRunState::Failed,
            ),
            (
                "agent-task-persisted-dispatch-timeout",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Timeout,
                AgentTaskRunState::Failed,
            ),
        ] {
            let mut record = record_detached_lab_run(DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            })
            .expect("running proxy");
            let mut aggregate = succeeded_aggregate(&test_plan());
            aggregate.status = aggregate_status;
            aggregate.outcomes[0].status = outcome_status;
            aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: format!("homeboy://lab/{run_id}/transcript"),
                label: Some("Provider transcript".to_string()),
            }];

            reconcile_runner_job_snapshot(
                &mut record,
                &persisted_terminal_result_snapshot(&aggregate),
            )
            .expect("hydrate persisted dispatch result");

            let artifact_report = artifacts(run_id).expect("controller artifacts");
            assert_eq!(record.state, expected_state);
            assert!(artifact_report
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == "transcript"));
        }
    });
}

#[test]
fn recovery_preserves_terminal_runner_identity_before_projecting_runner_artifacts() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let run_id = "agent-task-recovered-runner-artifact";
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "runner/a:lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("detached handoff");
        record.ensure_metadata_object().remove("runner_id");
        record.ensure_metadata_object().remove("runner_job_id");

        let patch = "runner patch";
        let finalized = homeboy_core::paths::artifact_root()
            .expect("artifact root")
            .join("executor-finalized")
            .join(run_id)
            .join("patch.diff");
        std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
            .expect("create finalized parent");
        std::fs::write(&finalized, patch).expect("write finalized patch");
        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/home/runner/.homeboy/executor-finalized/patch.diff".to_string()),
            url: Some(
                "homeboy://agent-task/run/detached-run/artifacts#task=task-a&artifact=patch"
                    .to_string(),
            ),
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(format!("{:x}", sha2::Sha256::digest(patch.as_bytes()))),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
        let mut snapshot = terminal_child_snapshot(&aggregate);
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["runner_id"] =
            json!("runner/a:lab");
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["run_id"] = json!(run_id);
        snapshot.events[0].data.as_mut().expect("event data")["identity"]["persisted_run_id"] =
            json!(run_id);
        let event = crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(
            Some(&snapshot.events),
        )
        .expect("terminal lifecycle event");
        assert_eq!(event.identity.runner_id, "runner/a:lab");
        assert_eq!(
            event.identity.runner_job_id,
            "00000000-0000-0000-0000-000000000123"
        );
        assert_eq!(event.identity.run_id.as_deref(), Some(run_id));
        assert_eq!(event.identity.persisted_run_id.as_deref(), Some(run_id));

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("terminal recovery");

        assert_eq!(record.metadata["runner_id"], "runner/a:lab");
        assert_eq!(
            record.metadata["runner_job_id"],
            "00000000-0000-0000-0000-000000000123"
        );
        assert_eq!(
            record.metadata["artifact_projection"]["status"], "complete",
            "{:#}",
            record.metadata
        );
        let artifacts = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .list_artifacts(run_id)
            .expect("artifact projections");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "file");
        let projected = verified_controller_artifact_projection_path(
            run_id,
            &aggregate.outcomes[0].task_id,
            &aggregate.outcomes[0].artifacts[0],
        );
        assert_eq!(
            projected.expect("verify projection"),
            Some(std::path::PathBuf::from(&artifacts[0].path))
        );
        assert_ne!(
            artifacts[0].path,
            "/home/runner/.homeboy/executor-finalized/patch.diff"
        );
    });
}

#[test]
fn terminal_reconciliation_reuses_verified_directly_imported_artifact() {
    with_isolated_home(|home| {
        let patch = b"patch bytes";
        let source = home.path().join("imported.patch");
        std::fs::write(&source, patch).expect("write imported patch");
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: Some("patch".to_string()),
            semantic_key: None,
            path: Some("/runner/private/patch.diff".to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
            metadata: json!({ "executor_artifact_finalized": true }),
        });
        let submitted = submit_plan(&plan, Some("direct-import-reconciliation")).expect("submit");
        record_runner_job_identity(&submitted.run_id, "homeboy-lab", "job-1")
            .expect("runner identity");

        let mut hash = sha2::Sha256::new();
        sha2::Digest::update(&mut hash, submitted.run_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, aggregate.outcomes[0].task_id.as_bytes());
        sha2::Digest::update(&mut hash, [0]);
        sha2::Digest::update(&mut hash, b"patch");
        let artifact_id = format!("agent-task-{:x}", hash.finalize());
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        store
            .import_artifact(&homeboy_core::observation::ArtifactRecord {
                id: artifact_id,
                run_id: submitted.run_id.clone(),
                kind: "patch".to_string(),
                artifact_type: "file".to_string(),
                path: source.display().to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
                size_bytes: Some(patch.len() as i64),
                mime: Some("text/x-patch".to_string()),
                metadata_json: json!({ "name": "patch" }),
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .expect("direct artifact import");

        record_run_aggregate(&submitted.run_id, &plan, &aggregate)
            .expect("terminal reconciliation");
        reconcile_terminal_artifact_projection(&submitted.run_id).expect("repeated reconciliation");

        let record = store::read_record(&submitted.run_id).expect("terminal record");
        assert_eq!(record.metadata["artifact_projection"]["status"], "complete");
        let artifact = homeboy_core::observation::runs_service::resolve_artifact_for_run(
            &store,
            &submitted.run_id,
            "patch",
        )
        .expect("actionable imported patch");
        let output = home.path().join("recovered.patch");
        homeboy_core::observation::runs_service::copy_local_file_artifact(
            artifact,
            Some(output.clone()),
        )
        .expect("recover patch without runner");
        assert_eq!(std::fs::read(output).expect("recovered patch bytes"), patch);
    });
}

#[test]
fn running_observation_projects_each_terminal_aggregate_state() {
    with_isolated_home(|_| {
        let cases = [
            (
                "terminal-success",
                AgentTaskAggregateStatus::Succeeded,
                AgentTaskOutcomeStatus::Succeeded,
                "succeeded",
            ),
            (
                "terminal-failure",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Failed,
                "failed",
            ),
            (
                "terminal-partial",
                AgentTaskAggregateStatus::PartialFailure,
                AgentTaskOutcomeStatus::Failed,
                "partial_failure",
            ),
            (
                "terminal-cancelled",
                AgentTaskAggregateStatus::Cancelled,
                AgentTaskOutcomeStatus::Cancelled,
                "cancelled",
            ),
        ];
        for (run_id, aggregate_status, outcome_status, terminal_state) in cases {
            let plan = test_plan();
            let mut aggregate = succeeded_aggregate(&plan);
            aggregate.status = aggregate_status;
            aggregate.outcomes[0].status = outcome_status;
            submit_plan(&plan, Some(run_id)).expect("submit");
            mark_running(run_id).expect("running");
            record_run_aggregate(run_id, &plan, &aggregate).expect("terminal aggregate");

            let observation = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("store")
                .get_run(run_id)
                .expect("observation")
                .expect("existing running observation transitioned");
            assert_ne!(observation.status, "running");
            assert_eq!(
                observation.metadata_json["agent_task_terminal_state"],
                terminal_state
            );
        }
    });
}

#[test]
fn pre_dispatch_failure_persists_failed_run_without_provider_handle() {
    with_isolated_home(|_| {
        let record = record_pre_dispatch_failure(AgentTaskPreDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "cook-lab-predispatch",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-lab-predispatch".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--cwd".to_string(),
                    "/runner/workspace/repo".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                failure_message: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout",
                stdout: "",
                stderr: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout\n",
                exit_code: 1,
            })
            .expect("pre-dispatch failure recorded");

        let loaded = status("cook-lab-predispatch").expect("status loaded");
        let log = logs("cook-lab-predispatch").expect("logs loaded");
        let artifact_report = artifacts("cook-lab-predispatch").expect("artifacts loaded");
        let legacy_status_path = homeboy_core::paths::homeboy_data()
            .expect("homeboy data")
            .join("agent-task-runs")
            .join("cook-lab-predispatch")
            .join("status.json");
        std::fs::remove_file(
            homeboy_core::paths::homeboy_data()
                .expect("homeboy data")
                .join("agent-task-runs")
                .join("cook-lab-predispatch")
                .join("aggregate.json"),
        )
        .expect("aggregate file removed");
        let mirrored_log = logs("cook-lab-predispatch").expect("mirrored logs loaded");
        let mirrored_artifacts =
            artifacts("cook-lab-predispatch").expect("mirrored artifacts loaded");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert!(loaded.provider_handles.is_empty());
        assert_eq!(log.events[1].state, AgentTaskState::Failed);
        assert_eq!(mirrored_log.events[1].state, AgentTaskState::Failed);
        assert_eq!(loaded.metadata["provider_run_ids"], serde_json::json!([]));
        assert_eq!(
            loaded.artifact_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            artifact_report.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            mirrored_artifacts.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert!(
            !legacy_status_path.exists(),
            "agent-task status.json is no longer the primary durable run record"
        );
    });
}

#[test]
fn record_completed_run_exposes_logs_and_artifacts() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("patch.diff".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some("/tmp/patch.diff".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "file:///tmp/transcript.json".to_string(),
                    label: Some("provider transcript".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };

        let record =
            record_completed_run(&plan, &aggregate, Some("run-complete")).expect("recorded");
        let log = logs(&record.run_id).expect("logs");
        let artifacts = artifacts(&record.run_id).expect("artifacts");

        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
        assert_eq!(artifacts.artifacts[0].id, "patch");
        assert_eq!(artifacts.evidence_refs[0].kind, "transcript");
    });
}

#[test]
fn submitted_run_can_be_loaded_marked_running_and_completed() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-execute")).expect("submitted");

        let loaded_plan = load_plan("run-execute").expect("plan loaded");
        let running = mark_running("run-execute").expect("marked running");
        let aggregate = succeeded_aggregate(&loaded_plan);

        let completed =
            record_run_aggregate("run-execute", &loaded_plan, &aggregate).expect("completed");
        let durable_status = status("run-execute").expect("status");

        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.tasks[0].state, AgentTaskState::Running);
        assert_eq!(
            running.lifecycle.execution.state,
            RunExecutionState::Running
        );
        assert!(running.lifecycle.heartbeat.is_some());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(
            completed.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(completed.totals, Some(aggregate.totals.clone()));
        assert_eq!(durable_status.state, AgentTaskRunState::Succeeded);
        assert_eq!(durable_status.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(durable_status.totals, Some(aggregate.totals.clone()));
        assert!(completed.aggregate_path.is_some());
    });
}

#[test]
fn completed_run_persists_opaque_provider_handles_from_outcome_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].metadata = json!({
            "provider_handle": AgentTaskExecutionHandle {
                kind: AgentTaskExecutionHandleKind::ProviderRun,
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                run_id: "provider-run-123".to_string(),
                stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                metadata: json!({ "opaque": { "provider_owned": true } }),
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-handle")).expect("recorded");

        assert_eq!(record.provider_handles.len(), 1);
        assert_eq!(record.provider_handles[0].task_id, "task-a");
        assert_eq!(record.provider_handles[0].backend, "sample-runtime");
        assert_eq!(
            record.provider_handles[0].provider_run_id,
            "provider-run-123"
        );
        assert_eq!(
            record.provider_handles[0].stream_uri.as_deref(),
            Some("provider://runs/provider-run-123/events")
        );
        assert_eq!(
            record.provider_handles[0].state,
            Some(AgentTaskState::Succeeded)
        );
        assert_eq!(
            record.provider_handles[0].metadata["opaque"]["provider_owned"],
            json!(true)
        );
        assert_eq!(
            record.metadata["provider_run_ids"],
            json!(["provider-run-123"])
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            ProviderRuntimeState::Succeeded
        );
        assert_eq!(
            record.lifecycle.external_runtime_ids[0].value,
            "provider-run-123"
        );
        assert_eq!(
            record.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::NotApplicable
        );
    });
}

#[test]
fn status_recovers_terminal_state_from_durable_aggregate() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-stale-status")).expect("submitted");
        mark_running("run-stale-status").expect("marked running");
        let aggregate = succeeded_aggregate(&plan);
        store::write_aggregate("run-stale-status", &aggregate).expect("aggregate written");

        let recovered = status("run-stale-status").expect("status recovered");
        let persisted = store::read_record("run-stale-status").expect("record persisted");

        assert_eq!(recovered.state, AgentTaskRunState::Succeeded);
        assert_eq!(recovered.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(recovered.totals, Some(aggregate.totals.clone()));
        assert_eq!(persisted.state, AgentTaskRunState::Succeeded);
        assert_eq!(persisted.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(persisted.totals, Some(aggregate.totals.clone()));
    });
}

#[test]
fn mark_running_rejects_live_running_record() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-live-owner")).expect("submitted");
        mark_running("run-live-owner").expect("marked running");

        let error = mark_running("run-live-owner").expect_err("live run rejected");

        assert!(error.message.contains("already running"));
    });
}

#[test]
fn cancel_run_emits_recovery_commands_for_runner_backed_run() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("run-cancel-runner")).expect("submitted");
        let mut record = store::read_record("run-cancel-runner").expect("record");
        record.state = AgentTaskRunState::Running;
        record.tasks[0].state = AgentTaskState::Running;
        // Runner-backed: owner pid lives on the runner host (not running
        // here), so live cancellation must hand back recovery commands.
        record.metadata = json!({
            "runner_pid": u32::MAX,
            "runner_id": "lab-a",
            "runner_job_id": "job-123",
        });
        store::write_record(&record).expect("stored runner record");

        let cancelled = cancel_run("run-cancel-runner", None).expect("runner run cancelled");

        assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
        assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
        let unsupported = &cancelled.metadata["live_cancellation_unsupported"];
        assert!(unsupported.is_object());
        assert_eq!(unsupported["runner_id"], json!("lab-a"));
        assert_eq!(unsupported["runner_job_id"], json!("job-123"));
        let commands = unsupported["recovery_commands"]
            .as_array()
            .expect("recovery commands array");
        assert!(!commands.is_empty());
        // The first recovery command should route cancellation to the
        // owning runner so the operator can act deterministically.
        assert!(commands[0]
            .as_str()
            .expect("command string")
            .contains("homeboy runner exec lab-a"));
        // No real local process was signalled.
        assert!(cancelled.metadata.get("live_cancellation").is_none());
    });
}

#[test]
fn record_health_recovers_after_interrupted_migration_without_changing_terminal_status() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("interrupted-terminal")).expect("submitted");
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let mut observation = store
            .get_run("interrupted-terminal")
            .expect("read")
            .expect("observation");
        observation.status = "pass".to_string();
        observation.finished_at = Some("2026-01-01T00:01:00Z".to_string());
        observation.metadata_json = json!({});
        store
            .upsert_imported_run(&observation)
            .expect("terminal malformed fixture");

        store::fail_next_record_write_for_test();
        assert!(reconcile_record_health(false).is_err());
        assert_eq!(
            record_health_summary().expect("still malformed").malformed,
            1
        );
        let applied = reconcile_record_health(false).expect("retry migration");
        assert_eq!(applied.migrated, 1);
        let repaired = status("interrupted-terminal").expect("repaired");
        assert_eq!(repaired.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            repaired.lifecycle.execution.finished_at.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
    });
}

#[test]
fn artifact_refs_dedup_identical_refs_across_artifacts_and_evidence() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "transcript",
            "transcript",
            Some("file:///tmp/transcript.json"),
            None,
        )],
        vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("transcript artifact".to_string()),
        }],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(
        refs.len(),
        1,
        "exact-duplicate refs collapse to a single entry"
    );
}
