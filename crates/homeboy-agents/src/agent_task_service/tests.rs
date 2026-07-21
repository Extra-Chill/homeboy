#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskFailureClassification,
    AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
    AgentTaskSourceRef, AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::agent_task_lifecycle;
use crate::agent_task_lifecycle::{status as lifecycle_status, AgentTaskRunState};
use crate::agent_task_schedule::AgentTaskPlan;
use crate::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskScheduler, AgentTaskState,
};
use homeboy_core::run_lifecycle_record::RunExecutionState;
use homeboy_core::test_support::with_isolated_home;
use homeboy_core::worktree;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

#[test]
fn cook_usage_reads_scheduler_rotation_metadata_and_decrements_budget() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut plan = test_plan();
    plan.options.retry.max_attempts = 1;
    plan.options.execution_budget =
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 0, 1);
    plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
        entries: vec![AgentTaskProviderRotationEntry {
            backend: Some("fallback".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let aggregate = AgentTaskScheduler::new(RotationThenSuccess {
        calls: Arc::clone(&calls),
    })
    .run(plan);

    let usage = execution_budget_usage(&aggregate);
    let remaining = budget_remaining(
        &crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 0, 1),
        usage,
    )
    .expect("remaining total budget");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(usage.executions, 2);
    assert_eq!(usage.provider_rotations, 1);
    assert_eq!(remaining.max_provider_executions, 1);
    assert_eq!(remaining.max_provider_rotations, 0);
}

#[test]
fn cook_remediation_reserves_the_actual_provider_category_before_launch() {
    let no_retry = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(2, 0, 2);
    assert_eq!(
        reserve_remediation_budget(&no_retry, true).expect_err("same provider needs retry budget"),
        "max_same_provider_retries"
    );

    let one_retry = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(2, 1, 2);
    let reservation = reserve_remediation_budget(&one_retry, true).expect("one retry reserved");
    assert_eq!(reservation.same_provider_retries, 1);
    let exhausted = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 2);
    assert_eq!(
        reserve_remediation_budget(&exhausted, true).expect_err("second retry blocked"),
        "max_same_provider_retries"
    );

    let after_rotation = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 1, 0);
    assert_eq!(
        reserve_remediation_budget(&after_rotation, false).expect_err("rotation blocked"),
        "max_provider_rotations"
    );
}

#[test]
fn service_run_loaded_plan_persists_durable_lifecycle() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(test_plan(), Some("service-run"), SucceedingExecutor)
            .expect("service run completed");
        let record = lifecycle_status("service-run").expect("status persisted");

        assert_eq!(result.exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
        assert!(record.aggregate_path.is_some());
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
        assert_eq!(record.lifecycle.provider_runtime.len(), 1);
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            homeboy_core::run_lifecycle_record::ProviderRuntimeState::Succeeded
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].metadata["evidence_source"],
            "durable_provider_execution"
        );
    });
}

#[test]
fn provider_execution_reservation_is_exactly_once_and_terminal() {
    with_isolated_home(|_| {
        let plan = test_plan();
        agent_task_lifecycle::submit_plan(&plan, Some("provider-reservation"))
            .expect("run submitted");
        let task = &plan.tasks[0];

        assert_eq!(
            agent_task_lifecycle::reserve_provider_execution("provider-reservation", task, 1)
                .expect("first reservation"),
            agent_task_lifecycle::ProviderExecutionReservation::Acquired
        );
        assert_eq!(
            agent_task_lifecycle::reserve_provider_execution("provider-reservation", task, 1)
                .expect("restart observes reservation"),
            agent_task_lifecycle::ProviderExecutionReservation::AlreadyReserved
        );
        let calls = Arc::new(AtomicUsize::new(0));
        AgentTaskScheduler::new(CountingExecutor {
            calls: Arc::clone(&calls),
        })
        .with_run_id("provider-reservation")
        .run(plan.clone());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "an existing reservation must be reconciled, never redispatched"
        );
        agent_task_lifecycle::record_provider_execution_terminal(
            "provider-reservation",
            &task.task_id,
            1,
            "cancelled",
        )
        .expect("terminal cancellation recorded");

        let record = lifecycle_status("provider-reservation").expect("durable record");
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
        assert_eq!(
            record.metadata["provider_executions"][0]["state"],
            "cancelled"
        );
    });
}

#[test]
fn concurrent_schedulers_dispatch_one_reserved_provider_execution() {
    with_isolated_home(|_| {
        let plan = test_plan();
        agent_task_lifecycle::submit_plan(&plan, Some("concurrent-provider-reservation"))
            .expect("run submitted");
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let plan = plan.clone();
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                AgentTaskScheduler::new(CountingExecutor { calls })
                    .with_run_id("concurrent-provider-reservation")
                    .run(plan)
            }));
        }
        for thread in threads {
            let _ = thread.join().expect("scheduler thread completes");
        }

        let record = lifecycle_status("concurrent-provider-reservation").expect("durable record");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
    });
}

#[test]
fn lab_handoff_run_plan_executes_with_runner_provenance_after_transport_is_consumed() {
    with_isolated_home(|_| {
        let execution_runner = homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
        let transport_runner = homeboy_lab_runner_contract::RUNNER_ID_ENV;
        let previous_execution_runner = std::env::var_os(execution_runner);
        let previous_transport_runner = std::env::var_os(transport_runner);
        std::env::set_var(execution_runner, "homeboy-lab");
        std::env::remove_var(transport_runner);

        agent_task_lifecycle::submit_plan(&test_plan(), Some("lab-handoff-run-plan"))
            .expect("staged runner record");
        agent_task_lifecycle::record_runner_job_identity(
            "lab-handoff-run-plan",
            "homeboy-lab",
            "foreground-daemon-job",
        )
        .expect("foreground daemon binds its job before run-plan");
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
        ];
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: "lab-handoff-run-plan",
            runner_id: "homeboy-lab",
            runner_job_id: "foreground-daemon-job",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("foreground daemon accepts the Lab handoff before run-plan");

        let result = run_loaded_plan(
            test_plan(),
            Some("lab-handoff-run-plan"),
            SucceedingExecutor,
        )
        .expect("runner-local provider execution starts without a nested daemon connection");
        assert_eq!(
            agent_task_lifecycle::execution_runner_id().as_deref(),
            Some("homeboy-lab")
        );

        match previous_execution_runner {
            Some(value) => std::env::set_var(execution_runner, value),
            None => std::env::remove_var(execution_runner),
        }
        match previous_transport_runner {
            Some(value) => std::env::set_var(transport_runner, value),
            None => std::env::remove_var(transport_runner),
        }

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.totals.succeeded, 1);
        let record =
            lifecycle_status("lab-handoff-run-plan").expect("completed runner-local record");
        assert_eq!(record.runner_job_id(), Some("foreground-daemon-job"));
        assert_eq!(
            record.lab_handoff.expect("accepted daemon handoff").state,
            agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
        );
    });
}

#[test]
fn lab_runner_handoff_materializes_the_run_before_preparation_failure() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0]
            .executor
            .secret_env
            .push("__HOMEBOY_TEST_MISSING_LAB_RUNNER_HANDOFF_SECRET__".to_string());
        std::env::remove_var("__HOMEBOY_TEST_MISSING_LAB_RUNNER_HANDOFF_SECRET__");

        let error = run_loaded_plan(
            plan,
            Some("controller-proxy-interrupted-lab-runner-handoff"),
            SucceedingExecutor,
        )
        .expect_err("runner preparation fails after receiving the durable plan");
        let record = lifecycle_status("controller-proxy-interrupted-lab-runner-handoff")
            .expect("runner-scoped status resolves from its materialized record");
        let log = agent_task_lifecycle::logs("controller-proxy-interrupted-lab-runner-handoff")
            .expect("runner-scoped logs resolve from its materialized record");
        let recovery = run_submitted(
            "controller-proxy-interrupted-lab-runner-handoff".to_string(),
            SucceedingExecutor,
        )
        .expect("runner-scoped run resolves the materialized terminal record");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert!(!log.events.is_empty());
        assert_eq!(recovery.exit_code, 1);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "prepare_plan_for_execution"
        );
        assert!(agent_task_lifecycle::load_plan(&record.run_id).is_ok());
    });
}

#[test]
fn submitted_terminal_runs_reuse_durable_evidence_without_reexecution() {
    with_isolated_home(|_| {
        for (run_id, expected_exit_code) in [("terminal-succeeded", 0), ("terminal-failed", 1)] {
            if run_id == "terminal-succeeded" {
                run_loaded_plan(test_plan(), Some(run_id), SucceedingExecutor)
                    .expect("succeeded run completed");
            } else {
                run_loaded_plan(test_plan(), Some(run_id), TimeoutExecutor)
                    .expect("failed run completed");
            }

            let observed_request = Arc::new(Mutex::new(None));
            let result = run_submitted(
                run_id.to_string(),
                CapturingExecutor {
                    observed_request: Arc::clone(&observed_request),
                },
            )
            .expect("terminal run returns its durable aggregate");

            assert_eq!(result.exit_code, expected_exit_code);
            assert!(observed_request.lock().expect("executor lock").is_none());
        }
    });
}

#[test]
fn cancelled_terminal_run_is_not_reexecuted_without_durable_aggregate() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("terminal-cancelled"))
            .expect("run submitted");
        agent_task_lifecycle::cancel_run("terminal-cancelled", Some("test cancellation"))
            .expect("run cancelled");

        let observed_request = Arc::new(Mutex::new(None));
        let error = run_submitted(
            "terminal-cancelled".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect_err("cancelled run has no aggregate to reuse");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("terminal with state Cancelled"));
        assert!(observed_request.lock().expect("executor lock").is_none());
    });
}

#[test]
fn submitted_incomplete_run_still_executes_for_recovery() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("incomplete-queued"))
            .expect("run submitted");

        let observed_request = Arc::new(Mutex::new(None));
        let result = run_submitted(
            "incomplete-queued".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("queued run recovers through normal execution");

        assert_eq!(result.exit_code, 0);
        assert!(observed_request.lock().expect("executor lock").is_some());
        assert_eq!(
            lifecycle_status("incomplete-queued").expect("status").state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn service_persists_timed_out_run_record_and_evidence_refs() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(test_plan(), Some("service-timeout"), TimeoutExecutor)
            .expect("timeout run completed");
        let record = lifecycle_status("service-timeout").expect("status persisted");
        let artifacts = artifacts("service-timeout").expect("artifacts persisted");

        assert_eq!(result.exit_code, 1);
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::TimedOut);
        assert_eq!(record.totals.as_ref().expect("totals").timed_out, 1);
        assert!(record.aggregate_path.is_some());
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
        assert_eq!(
            record.metadata["provider_executions"][0]["state"],
            "timed_out"
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            homeboy_core::run_lifecycle_record::ProviderRuntimeState::TimedOut
        );
        assert!(record
            .artifact_refs
            .iter()
            .any(|artifact| artifact.kind == "executor-result"));
        assert!(artifacts
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "executor-result"));
    });
}

#[test]
fn service_normalizes_resolved_component_worktree_plan() {
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("homeboy".to_string());
    plan.tasks[0].workspace.materialization = serde_json::json!({
        "resolved_root": "/tmp/homeboy@service"
    });

    normalize_plan_workspaces(&mut plan).expect("workspace normalized");

    assert!(plan.tasks[0].workspace.kind.is_none());
    assert_eq!(plan.tasks[0].workspace.slug.as_deref(), Some("homeboy"));
    assert_eq!(
        plan.tasks[0].workspace.root.as_deref(),
        Some("/tmp/homeboy@service")
    );
    assert_eq!(
        plan.tasks[0].workspace.mode,
        AgentTaskWorkspaceMode::Existing
    );
    assert!(plan.tasks[0].workspace.materialization.is_null());
}

#[test]
fn service_materializes_component_worktree_before_provider_dispatch() {
    with_isolated_home(|home| {
        let repo = home.path().join("fixture");
        create_git_repo(&repo);
        write_component_registration(home.path(), "fixture", &repo);
        let observed_request = Arc::new(Mutex::new(None));
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("fixture".to_string());
        plan.tasks[0].workspace.branch = Some("fix/service-task".to_string());
        plan.tasks[0].workspace.base_ref = Some("HEAD".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: "https://example.com/tasks/123".to_string(),
            revision: None,
        }];

        let result = run_loaded_plan(
            plan,
            Some("service-materialized-worktree"),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");
        let record = worktree::resolve("fixture@fix-service-task").expect("worktree record");

        assert_eq!(result.exit_code, 0);
        assert_eq!(
            record.run_id.as_deref(),
            Some("service-materialized-worktree")
        );
        assert_eq!(
            record.task_url.as_deref(),
            Some("https://example.com/tasks/123")
        );
        assert_eq!(
            record.cleanup_policy,
            worktree::CleanupPolicy::PreserveOnFailure
        );
        assert_eq!(observed.workspace.mode, AgentTaskWorkspaceMode::Existing);
        // Provider dispatch runs in an isolated per-attempt detached worktree
        // derived from the managed component worktree, so a timed-out provider
        // cannot contaminate a later rotation (#8092). The managed worktree
        // stays the preflight source of truth (its metadata is preserved
        // below), but the provider must NOT be pointed straight at it.
        let observed_root = observed
            .workspace
            .root
            .as_deref()
            .expect("provider received a workspace root");
        assert_ne!(
            observed_root, record.worktree_path,
            "provider must run in an isolated attempt worktree, not the managed worktree"
        );
        assert!(
            observed_root.contains("homeboy-agent-task-attempts"),
            "attempt worktree should live under the agent-task attempts scratch root, got {observed_root}"
        );
        assert!(observed.workspace.attempt.is_some());
        assert_eq!(observed.workspace.slug.as_deref(), Some("fixture"));
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert_eq!(observed.workspace.cleanup.as_deref(), Some("preserve"));
        assert_eq!(
            observed.workspace.materialization["id"].as_str(),
            Some("fixture@fix-service-task")
        );
        assert!(Path::new(&record.worktree_path).is_dir());
    });
}

#[test]
fn run_next_records_failed_lifecycle_when_prepare_after_claim_fails() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.tasks[0]
            .executor
            .secret_env
            .push("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__".to_string());
        std::env::remove_var("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__");
        agent_task_lifecycle::submit_plan(&plan, Some("run-next-preexecution-fails"))
            .expect("submitted");

        let error = run_next(SucceedingExecutor).expect_err("prepare failure returned");
        let record = lifecycle_status("run-next-preexecution-fails").expect("status persisted");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(record.lifecycle.execution.state, RunExecutionState::Failed);
        assert!(record.lifecycle.execution.finished_at.is_some());
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "prepare_plan_for_execution"
        );
        assert!(record.aggregate_path.is_some());
    });
}

#[test]
fn discovery_lists_durable_runs_with_operator_commands() {
    with_isolated_home(|_| {
        let plan = discovery_plan();
        agent_task_lifecycle::submit_plan(&plan, Some("run-discovery-list")).expect("submitted");

        let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
        let run = report.runs.first().expect("run");

        assert_eq!(report.schema, "homeboy/agent-task-discovery/v1");
        assert_eq!(report.filter, "all");
        assert_eq!(report.count, 1);
        assert_eq!(report.total, 1);
        assert_eq!(report.record_health.malformed, 0);
        assert_eq!(report.record_health.legacy, 0);
        assert_eq!(report.record_health.conflicting, 0);
        assert!(!report.truncated);
        assert!(report.limit.is_none());
        assert!(report
            .lab_discovery
            .runner_scoped_command
            .contains("--runner"));
        assert!(report.lab_discovery.fallback_command.contains("runs list"));
        assert_eq!(run.run_id, "run-discovery-list");
        assert_eq!(run.state, AgentTaskRunState::Queued);
        assert_eq!(run.repo.as_deref(), Some("homeboy"));
        assert_eq!(run.workspace.as_deref(), Some("/tmp/homeboy"));
        assert_eq!(
            run.task_url.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/issues/4386")
        );
        assert_eq!(run.counts.queued, 1);
        assert!(run
            .commands
            .status
            .ends_with("agent-task status run-discovery-list"));
        assert!(run
            .commands
            .logs
            .ends_with("agent-task logs run-discovery-list"));
        assert!(run
            .commands
            .artifacts
            .ends_with("agent-task artifacts run-discovery-list"));
        assert!(run
            .commands
            .review
            .ends_with("agent-task review run-discovery-list"));
        assert!(run
            .commands
            .retry
            .ends_with("agent-task retry run-discovery-list --run"));
        assert!(run
            .commands
            .run_plan
            .contains("homeboy --runner <runner-id> agent-task run-plan --plan @"));
        assert!(run
            .commands
            .run_plan
            .contains("/agent-task-runs/run-discovery-list/plan.json"));
    });
}

#[test]
fn discovery_active_filters_to_queued_and_running_runs() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-queued"))
            .expect("queued submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-running"))
            .expect("running submitted");
        agent_task_lifecycle::mark_running("run-active-running").expect("marked running");
        run_loaded_plan(
            discovery_plan(),
            Some("run-active-complete"),
            SucceedingExecutor,
        )
        .expect("completed");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
        let run_ids: Vec<_> = report.runs.iter().map(|run| run.run_id.as_str()).collect();

        assert_eq!(report.filter, "active");
        assert_eq!(report.count, 2);
        assert!(run_ids.contains(&"run-active-queued"));
        assert!(run_ids.contains(&"run-active-running"));
        assert!(!run_ids.contains(&"run-active-complete"));
    });
}

#[test]
fn discovery_active_marks_runner_backed_running_run_as_stale_retryable() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-stale"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-runner-stale", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-123",
            });
        })
        .expect("running runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
        let run = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-runner-stale")
            .expect("runner-backed run listed");

        assert_eq!(run.runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(run.runner_job_id.as_deref(), Some("job-123"));
        assert_eq!(run.stale, Some(true));
        assert_eq!(
            run.stale_reason.as_deref(),
            Some("runner_job_unverified_after_daemon_restart")
        );
        assert_eq!(run.retryable, Some(true));
    });
}

#[test]
fn discovery_active_classifies_liveness_and_source() {
    with_isolated_home(|_| {
        // Queued run: always classified active.
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-queued"))
            .expect("queued submitted");

        // Stale runner-backed run: lifecycle flags it stale -> Stale.
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-stale"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-live-stale", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-xyz",
            });
        })
        .expect("stale runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");

        let queued = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-queued")
            .expect("queued listed");
        assert_eq!(queued.liveness, Some(AgentTaskLiveness::Active));
        assert!(queued.source == "local" || queued.source.starts_with("runner:"));

        let stale = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-stale")
            .expect("stale listed");
        assert_eq!(stale.liveness, Some(AgentTaskLiveness::Stale));
        assert_eq!(stale.source, "runner:homeboy-lab");

        let summary = report.liveness_summary.expect("active summary present");
        assert!(summary.active >= 1);
        assert_eq!(summary.stale, 1);
        assert_eq!(
            summary.reconcile_command,
            "homeboy agent-task active --reconcile"
        );
    });
}

#[test]
fn reconcile_dry_run_reports_but_does_not_cancel_stale_runs() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-dry"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-reconcile-dry", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-dry",
            });
        })
        .expect("stale record stored");

        let report = reconcile_stale_active_runs(true).expect("dry run reconciled");
        assert!(report.dry_run);
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.considered, 1);
        assert_eq!(report.runs[0].action, "would-reconcile");

        // Record must remain running after a dry run.
        let record = lifecycle_status("run-reconcile-dry").expect("status");
        assert_eq!(record.state, AgentTaskRunState::Running);
    });
}

#[test]
fn reconcile_cancels_stale_running_record_without_manual_edit() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-live"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-reconcile-live", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-live",
            });
        })
        .expect("stale record stored");

        let report = reconcile_stale_active_runs(false).expect("reconciled");
        assert!(!report.dry_run);
        assert_eq!(report.reconciled, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.runs[0].action, "reconciled");

        let record = lifecycle_status("run-reconcile-live").expect("status");
        assert_eq!(record.state, AgentTaskRunState::Cancelled);

        // A genuinely-active run reconcile pass leaves nothing to do.
        let empty = reconcile_stale_active_runs(false).expect("nothing to reconcile");
        assert_eq!(empty.considered, 0);
        assert_eq!(empty.reconciled, 0);
    });
}

#[test]
fn discovery_latest_returns_only_newest_run() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-a"))
            .expect("first submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-z"))
            .expect("second submitted");

        let report = discover_runs(AgentTaskDiscoveryFilter::Latest).expect("latest listed");

        assert_eq!(report.filter, "latest");
        assert_eq!(report.count, 1);
        assert_eq!(report.runs[0].run_id, "run-latest-z");
    });
}

#[test]
fn discovery_limit_caps_list_and_reports_total() {
    with_isolated_home(|_| {
        for run_id in ["run-cap-a", "run-cap-b", "run-cap-c"] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        }

        let report = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions { limit: Some(2) },
        )
        .expect("listed with limit");

        assert_eq!(report.count, 2);
        assert_eq!(report.total, 3);
        assert_eq!(report.limit, Some(2));
        assert!(report.truncated);
        assert_eq!(report.runs.len(), 2);
    });
}

#[test]
fn discovery_latest_ignores_limit() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-a"))
            .expect("submitted");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-z"))
            .expect("submitted");

        let report = discover_runs_with_options(
            AgentTaskDiscoveryFilter::Latest,
            AgentTaskDiscoveryOptions { limit: Some(5) },
        )
        .expect("latest listed");

        // `latest` is always a single run; a limit is a no-op and not echoed.
        assert_eq!(report.count, 1);
        assert!(report.limit.is_none());
        assert!(!report.truncated);
    });
}

#[test]
fn discovery_runner_backed_run_emits_runner_scoped_commands() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-commands"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-runner-commands", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({
                "runner_id": "homeboy-lab",
            });
        })
        .expect("runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
        let run = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-runner-commands")
            .expect("runner-backed run listed");

        // Commands must be valid for the run's location: runner-scoped.
        assert_eq!(
            run.commands.status,
            "homeboy --runner homeboy-lab agent-task status run-runner-commands"
        );
        assert_eq!(
            run.commands.logs,
            "homeboy --runner homeboy-lab agent-task logs run-runner-commands"
        );
        assert_eq!(
            run.commands.review,
            "homeboy --runner homeboy-lab agent-task review run-runner-commands"
        );
        assert_eq!(
            run.commands.reconcile,
            "homeboy --runner homeboy-lab agent-task cancel run-runner-commands --reason stale-running"
        );
    });
}

#[test]
fn discovery_keeps_controller_handoff_commands_resolvable_after_runner_reconnect() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        agent_task_lifecycle::record_lab_offload_planned(
            agent_task_lifecycle::LabOffloadProxyPlan {
                run_id: "controller-handoff-reconnect",
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: Some(&discovery_plan()),
            },
        )
        .expect("controller handoff persisted before runner acceptance");
        let before_acceptance = discover_runs(AgentTaskDiscoveryFilter::Active).expect("listed");
        let queued = before_acceptance
            .runs
            .iter()
            .find(|run| run.run_id == "controller-handoff-reconnect")
            .expect("unaccepted controller handoff listed");
        assert_eq!(
            queued.commands.status,
            "homeboy agent-task status controller-handoff-reconnect"
        );
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: "controller-handoff-reconnect",
            runner_id: "homeboy-lab",
            runner_job_id: "reconnected-daemon-job",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        })
        .expect("accepted handoff remains controller materialized");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("listed");
        let run = report
            .runs
            .iter()
            .find(|run| run.run_id == "controller-handoff-reconnect")
            .expect("accepted controller handoff listed");

        assert_eq!(run.runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(
            run.commands.status,
            "homeboy agent-task status controller-handoff-reconnect"
        );
        assert_eq!(
            run.commands.logs,
            "homeboy agent-task logs controller-handoff-reconnect"
        );
        assert!(agent_task_lifecycle::status(&run.run_id).is_ok());
        assert!(agent_task_lifecycle::logs(&run.run_id).is_ok());
    });
}

#[test]
fn reconcile_terminalizes_an_unaccepted_controller_handoff_after_its_deadline() {
    with_isolated_home(|_| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        agent_task_lifecycle::record_lab_offload_planned(
            agent_task_lifecycle::LabOffloadProxyPlan {
                run_id: "controller-handoff-unaccepted",
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: Some(&discovery_plan()),
            },
        )
        .expect("controller handoff persisted before runner acceptance");
        agent_task_lifecycle::rewrite_record_for_test("controller-handoff-unaccepted", |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("typed handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("expire acceptance deadline");

        let active = discover_runs(AgentTaskDiscoveryFilter::Active).expect("listed");
        let run = active
            .runs
            .iter()
            .find(|run| run.run_id == "controller-handoff-unaccepted")
            .expect("unaccepted handoff listed");
        assert_eq!(run.liveness, Some(AgentTaskLiveness::Unreconciled));
        assert_eq!(
            run.commands.status,
            "homeboy agent-task status controller-handoff-unaccepted"
        );

        let reconciliation = reconcile_stale_active_runs(false).expect("reconciled");
        assert_eq!(reconciliation.reconciled, 1);
        assert_eq!(reconciliation.runs[0].action, "reconciled");
        assert_eq!(
            lifecycle_status("controller-handoff-unaccepted")
                .expect("terminal controller record")
                .state,
            AgentTaskRunState::Cancelled
        );
    });
}

#[derive(Clone)]
struct SucceedingExecutor;

impl AgentTaskExecutorAdapter for SucceedingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct TimeoutExecutor;

impl AgentTaskExecutorAdapter for TimeoutExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some("provider exceeded timeout_ms=50".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "executor-result".to_string(),
                uri: "file:///tmp/executor-result.json".to_string(),
                label: Some("executor result".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_task.provider_timeout".to_string(),
                message: "provider exceeded timeout_ms=50".to_string(),
                data: serde_json::json!({ "timeout_ms": 50 }),
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct CapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}

impl AgentTaskExecutorAdapter for CountingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct RotationThenSuccess {
    calls: Arc<AtomicUsize>,
}

impl AgentTaskExecutorAdapter for RotationThenSuccess {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: if call == 0 {
                AgentTaskOutcomeStatus::ProviderError
            } else {
                AgentTaskOutcomeStatus::Succeeded
            },
            summary: None,
            failure_classification: (call == 0).then_some(AgentTaskFailureClassification::Provider),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

impl AgentTaskExecutorAdapter for CapturingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        *self
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

fn create_git_repo(path: &Path) {
    std::fs::create_dir_all(path).expect("repo dir");
    homeboy_core::test_support::run_git_fixture_command(path, &["init", "-q"]);
    homeboy_core::test_support::run_git_fixture_command(
        path,
        &["config", "user.email", "homeboy@example.com"],
    );
    homeboy_core::test_support::run_git_fixture_command(
        path,
        &["config", "user.name", "Homeboy Test"],
    );
    std::fs::write(path.join("README.md"), "initial\n").expect("readme");
    homeboy_core::test_support::run_git_fixture_command(path, &["add", "."]);
    homeboy_core::test_support::run_git_fixture_command(path, &["commit", "-q", "-m", "initial"]);
}

fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
    let dir = home.join(".config/homeboy/components");
    std::fs::create_dir_all(&dir).expect("components dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::json!({
            "local_path": local_path,
            "remote_path": format!("wp-content/plugins/{id}")
        })
        .to_string(),
    )
    .expect("component registration");
}

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "service-plan",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "service-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("service".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

fn discovery_plan() -> AgentTaskPlan {
    let mut plan = test_plan();
    plan.group_key = Some("homeboy".to_string());
    plan.tasks[0].group_key = Some("homeboy".to_string());
    plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
        kind: "task".to_string(),
        uri: "https://github.com/Extra-Chill/homeboy/issues/4386".to_string(),
        revision: None,
    }];
    plan.tasks[0].workspace.root = Some("/tmp/homeboy".to_string());
    plan.tasks[0].workspace.slug = Some("homeboy".to_string());
    plan
}
