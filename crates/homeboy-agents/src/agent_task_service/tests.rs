#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskFailureClassification,
    AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
    AgentTaskSourceRef, AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::agent_task_lifecycle;
use crate::agent_task_lifecycle::{reconcile_status as lifecycle_status, AgentTaskRunState};
use crate::agent_task_schedule::AgentTaskPlan;
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskScheduler, AgentTaskState,
};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::run_lifecycle_record::RunExecutionState;
use homeboy_core::test_support::{with_isolated_home, write_component_registration};
use homeboy_core::worktree;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> crate::agent_task_lifecycle::AgentTaskLifecycleStore {
    crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        .expect("lifecycle store")
}

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
    let aggregate = AgentTaskScheduler::new(Arc::new(RotationThenSuccess {
        calls: Arc::clone(&calls),
    }))
    .run(plan);

    let usage = execution_budget_usage(&aggregate);
    let mut budget = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 0, 1);
    budget.deadline_unix_ms = Some(u64::MAX);
    let remaining = budget_remaining(&budget, usage).expect("remaining total budget");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(usage.executions, 2);
    assert_eq!(usage.provider_rotations, 1);
    assert_eq!(remaining.max_provider_executions, 1);
    assert_eq!(remaining.max_provider_rotations, 0);
    assert_eq!(remaining.deadline_unix_ms, budget.deadline_unix_ms);
}

#[test]
fn terminal_executor_uses_durable_execution_when_outcome_executor_is_absent() {
    let mut plan = test_plan();
    plan.tasks[0].executor.model = Some("openai/gpt-5.6-terra".to_string());
    let aggregate = aggregate_with_outcome(serde_json::json!({}));
    let follow_up = plan.tasks[0].executor.clone();
    let durable = serde_json::json!([{
        "task_id": "service-task",
        "attempt": 1,
        "backend": "test",
        "model": "openai/gpt-5.6-terra",
        "state": "succeeded"
    }]);

    assert_eq!(
        terminal_executor_matches(&aggregate, &plan, Some(&durable), &follow_up),
        Some(true)
    );
}

#[test]
fn terminal_executor_preserves_normalized_outcome_identity() {
    let mut plan = test_plan();
    plan.tasks[0].executor.model = Some("normalized-model".to_string());
    let aggregate = aggregate_with_outcome(serde_json::json!({
        "executor": {
            "backend": "test",
            "selector": "service",
            "model": "normalized-model"
        }
    }));

    assert_eq!(
        terminal_executor_matches(&aggregate, &plan, None, &plan.tasks[0].executor),
        Some(true)
    );

    let conflicting = serde_json::json!([{
        "task_id": "service-task",
        "attempt": 1,
        "backend": "other",
        "model": "normalized-model",
        "state": "succeeded"
    }]);
    assert_eq!(
        terminal_executor_matches(
            &aggregate,
            &plan,
            Some(&conflicting),
            &plan.tasks[0].executor
        ),
        None
    );
}

#[test]
fn terminal_executor_uses_rotated_terminal_identity_not_initial_plan() {
    let mut plan = test_plan();
    plan.tasks[0].executor.model = Some("initial-model".to_string());
    let aggregate = aggregate_with_outcome(serde_json::json!({
        "provider_rotation": {
            "attempts": [{
                "attempt": 1,
                "rotation_index": 0,
                "backend": "rotated",
                "selector": "terminal-selector",
                "model": "terminal-model",
                "status": "succeeded",
                "summary": null
            }]
        }
    }));
    let durable = serde_json::json!([{
        "task_id": "service-task",
        "attempt": 1,
        "backend": "rotated",
        "model": "terminal-model",
        "state": "succeeded"
    }]);
    let mut follow_up = plan.tasks[0].executor.clone();
    follow_up.backend = "rotated".to_string();
    follow_up.selector = Some("terminal-selector".to_string());
    follow_up.model = Some("terminal-model".to_string());

    assert_eq!(
        terminal_executor_matches(&aggregate, &plan, Some(&durable), &follow_up),
        Some(true)
    );
    assert_eq!(
        terminal_executor_matches(&aggregate, &plan, Some(&durable), &plan.tasks[0].executor),
        Some(false)
    );
}

#[test]
fn terminal_executor_rejects_conflicting_or_unavailable_authority() {
    let mut plan = test_plan();
    plan.tasks[0].executor.model = Some("initial-model".to_string());
    let aggregate = aggregate_with_outcome(serde_json::json!({
        "provider_rotation": {
            "attempts": [{
                "attempt": 1,
                "rotation_index": 0,
                "backend": "rotated",
                "selector": "terminal-selector",
                "model": "terminal-model",
                "status": "succeeded",
                "summary": null
            }]
        }
    }));
    let conflicting = serde_json::json!([{
        "task_id": "service-task",
        "attempt": 1,
        "backend": "other",
        "model": "terminal-model",
        "state": "succeeded"
    }]);

    assert_eq!(
        terminal_executor_matches(
            &aggregate,
            &plan,
            Some(&conflicting),
            &plan.tasks[0].executor
        ),
        None
    );
    assert_eq!(
        terminal_executor_matches(
            &aggregate_with_outcome(serde_json::json!({})),
            &plan,
            None,
            &plan.tasks[0].executor,
        ),
        None
    );
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
fn cook_budget_preflight_rejects_unfunded_attempts_and_form_remediation() {
    let default_provider_budget =
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 0);
    let error = validate_effective_cook_budget(3, &default_provider_budget)
        .expect_err("three Cook attempts require three provider executions");
    assert!(
        error.message.contains("--max-attempts 3"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains("--max-provider-executions 3 --max-same-provider-retries 2"),
        "{}",
        error.message
    );

    let rotations_only = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 0, 2);
    let error = validate_effective_cook_budget(3, &rotations_only)
        .expect_err("required form remediation cannot rotate providers");
    assert!(
        error.message.contains("review-form retries"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains("provider-rotations 2 cannot replace"),
        "{}",
        error.message
    );

    assert_eq!(
        validate_effective_cook_budget(
            3,
            &crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 2, 0),
        )
        .expect("funded Cook budget")
        .requested_attempts,
        3
    );
}

#[test]
fn cook_retry_intent_derives_single_provider_and_gate_remediation_budgets() {
    let resolved = resolve_cook_budget(2, 0, None, None, None).expect("derived budget");

    assert_eq!(resolved.requested_attempts, 2);
    assert_eq!(resolved.provider_executions, 2);
    assert_eq!(resolved.same_provider_remediations, 1);
    assert_eq!(resolved.provider_rotations, 0);

    let explicit = resolve_cook_budget(2, 0, Some(2), Some(1), Some(0))
        .expect("compatible explicit caller retains its budget");
    assert_eq!(explicit, resolved);
}

#[test]
fn cook_retry_intent_adds_configured_rotation_allowance() {
    let resolved = resolve_cook_budget(2, 2, None, None, None).expect("derived rotation budget");

    assert_eq!(resolved.provider_executions, 4);
    assert_eq!(resolved.same_provider_remediations, 1);
    assert_eq!(resolved.provider_rotations, 2);
    assert_eq!(resolved.truncated_provider_rotations, 0);
}

#[test]
fn cook_retry_intent_explicit_execution_cap_truncates_configured_rotations() {
    let resolved = resolve_cook_budget(1, 2, Some(1), None, None)
        .expect("an explicit execution cap bounds inherited rotations");

    assert_eq!(resolved.requested_provider_executions, 3);
    assert_eq!(resolved.provider_executions, 1);
    assert_eq!(resolved.requested_provider_rotations, 2);
    assert_eq!(resolved.provider_rotations, 0);
    assert_eq!(resolved.truncated_provider_rotations, 2);
}

#[test]
fn cook_retry_intent_preserves_explicit_zero_rotation_override() {
    let resolved = resolve_cook_budget(2, 2, None, None, Some(0))
        .expect("an explicit rotation disablement is a valid Cook policy");

    assert_eq!(resolved.provider_executions, 2);
    assert_eq!(resolved.same_provider_remediations, 1);
    assert_eq!(resolved.provider_rotations, 0);
}

#[test]
fn cook_retry_intent_rejects_contradictory_explicit_rotation_with_correction() {
    let error = resolve_cook_budget(2, 1, Some(2), Some(1), Some(1))
        .expect_err("rotation requires its own provider execution allowance");

    assert_eq!(error.details["field"], "max-provider-executions");
    assert!(
        error.message.contains(
            "--max-provider-executions 3 --max-same-provider-retries 1 --max-provider-rotations 1"
        ),
        "{}",
        error.message
    );
}

#[test]
fn cook_retry_intent_rejects_explicitly_disabled_gate_remediation() {
    let error = resolve_cook_budget(2, 0, None, Some(0), None)
        .expect_err("a gate remediation slot cannot be disabled when Cook may retry");

    assert_eq!(error.details["field"], "max-same-provider-retries");
    assert!(
        error
            .message
            .contains("--max-provider-executions 2 --max-same-provider-retries 1"),
        "{}",
        error.message
    );
}

#[test]
fn service_run_loaded_plan_persists_durable_lifecycle() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(
            test_plan(),
            Some("service-run"),
            Arc::new(SucceedingExecutor),
        )
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
            agent_task_lifecycle::reserve_provider_execution_in_store(
                &test_lifecycle_store(),
                "provider-reservation",
                task,
                1
            )
            .expect("first reservation"),
            agent_task_lifecycle::ProviderExecutionReservation::Acquired
        );
        assert_eq!(
            agent_task_lifecycle::reserve_provider_execution_in_store(
                &test_lifecycle_store(),
                "provider-reservation",
                task,
                1
            )
            .expect("restart observes reservation"),
            agent_task_lifecycle::ProviderExecutionReservation::AlreadyReserved
        );
        let calls = Arc::new(AtomicUsize::new(0));
        AgentTaskScheduler::new(Arc::new(CountingExecutor {
            calls: Arc::clone(&calls),
        }))
        .with_run_id("provider-reservation")
        .run(plan.clone());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "an existing reservation must be reconciled, never redispatched"
        );
        agent_task_lifecycle::record_provider_execution_terminal_in_store(
            &test_lifecycle_store(),
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
fn local_reservation_advances_heartbeat_to_execution_start() {
    // A local (in-process) cook must advance its heartbeat when the provider
    // execution is reserved, so `status`/`activity` can distinguish an active
    // provider from a hung preflight instead of showing the submission-time
    // heartbeat for the whole run (#8396).
    with_isolated_home(|_| {
        let plan = test_plan();
        agent_task_lifecycle::submit_plan(&plan, Some("local-heartbeat")).expect("run submitted");
        let task = &plan.tasks[0];

        agent_task_lifecycle::reserve_provider_execution_in_store(
            &test_lifecycle_store(),
            "local-heartbeat",
            task,
            1,
        )
        .expect("reservation acquired");

        let record = lifecycle_status("local-heartbeat").expect("durable record");
        let started_at = record.metadata["provider_executions"][0]["started_at"]
            .as_str()
            .expect("running execution start recorded");
        let heartbeat = record
            .lifecycle
            .heartbeat
            .as_ref()
            .expect("heartbeat advanced on reservation");
        assert_eq!(
            heartbeat.last_seen_at, started_at,
            "heartbeat should advance to provider-execution start for a local cook"
        );
        assert_eq!(
            heartbeat.owner_pid,
            Some(std::process::id()),
            "a local cook's owner PID is the reserving process"
        );
    });
}

#[test]
fn local_provider_execution_observes_its_durable_running_boundary() {
    with_isolated_home(|_| {
        let observed = Arc::new(Mutex::new(None));
        run_loaded_plan(
            test_plan(),
            Some("local-provider-boundary"),
            Arc::new(LocalBoundaryExecutor {
                run_id: "local-provider-boundary".to_string(),
                observed: Arc::clone(&observed),
            }),
        )
        .expect("local provider run completes");

        assert_eq!(
            observed.lock().expect("provider boundary").as_ref(),
            Some(&Value::String("running".to_string()))
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
                AgentTaskScheduler::new(Arc::new(CountingExecutor { calls }))
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
        let transport_runner = homeboy_runner_contract::RUNNER_ID_ENV;
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
            Arc::new(SucceedingExecutor),
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
            Arc::new(SucceedingExecutor),
        )
        .expect_err("runner preparation fails after receiving the durable plan");
        let record = lifecycle_status("controller-proxy-interrupted-lab-runner-handoff")
            .expect("runner-scoped status resolves from its materialized record");
        let log = agent_task_lifecycle::logs("controller-proxy-interrupted-lab-runner-handoff")
            .expect("runner-scoped logs resolve from its materialized record");
        let recovery = run_submitted(
            "controller-proxy-interrupted-lab-runner-handoff".to_string(),
            Arc::new(SucceedingExecutor),
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
fn runner_exec_materializes_the_run_before_incomplete_harvest_transport_failure() {
    with_isolated_home(|_| {
        let source_env = homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV;
        let lab_env = homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV;
        let previous_source = std::env::var_os(source_env);
        let previous_lab = std::env::var_os(lab_env);
        std::env::set_var(source_env, "{}");
        std::env::remove_var(lab_env);

        let error = run_loaded_plan(
            test_plan(),
            Some("runner-exec-incomplete-harvest-transport"),
            Arc::new(SucceedingExecutor),
        )
        .expect_err("runner exec source metadata requires paired Lab transport");

        match previous_source {
            Some(value) => std::env::set_var(source_env, value),
            None => std::env::remove_var(source_env),
        }
        match previous_lab {
            Some(value) => std::env::set_var(lab_env, value),
            None => std::env::remove_var(lab_env),
        }

        let record = lifecycle_status("runner-exec-incomplete-harvest-transport")
            .expect("pre-execution failure remains inspectable");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("incomplete Lab snapshot transport"));
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "validate_harvest_transport"
        );
    });
}

#[test]
fn submitted_terminal_runs_reuse_durable_evidence_without_reexecution() {
    with_isolated_home(|_| {
        for (run_id, expected_exit_code) in [("terminal-succeeded", 0), ("terminal-failed", 1)] {
            if run_id == "terminal-succeeded" {
                run_loaded_plan(test_plan(), Some(run_id), Arc::new(SucceedingExecutor))
                    .expect("succeeded run completed");
            } else {
                run_loaded_plan(test_plan(), Some(run_id), Arc::new(TimeoutExecutor))
                    .expect("failed run completed");
            }

            let observed_request = Arc::new(Mutex::new(None));
            let result = run_submitted(
                run_id.to_string(),
                Arc::new(CapturingExecutor {
                    observed_request: Arc::clone(&observed_request),
                }),
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
            Arc::new(CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            }),
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
            Arc::new(CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            }),
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
fn submitted_run_persists_and_executes_its_admitted_fallback_route() {
    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("readiness fixture");
        let script = temp.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:model==='fallback',classification:model==='fallback'?'ready':'account',retryable:false,remediation:'switch',reason:model==='fallback'?'':'blocked',cache_key:model,identity:{model}}));",
        )
        .expect("readiness script");
        let mut provider: crate::agent_task_provider::AgentTaskExecutorProvider =
            serde_json::from_value(serde_json::json!({
                "id": "service",
                "backend": "test"
            }))
            .expect("provider fixture");
        provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec!["node".to_string(), script.display().to_string()],
                ..CommandInvocation::default()
            }
            .into(),
        );
        let catalog = crate::agent_task_provider::AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        };
        let mut plan = test_plan();
        plan.tasks[0].executor.model = Some("primary".to_string());
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("fallback".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let observed_request = Arc::new(Mutex::new(None));
        agent_task_lifecycle::submit_plan(&plan, Some("submitted-fallback"))
            .expect("submitted plan");

        run_submitted_with_timeout_and_catalog(
            "submitted-fallback".to_string(),
            None,
            Arc::new(CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            }),
            &catalog,
        )
        .expect("admitted fallback executes");

        assert_eq!(
            observed_request
                .lock()
                .expect("observed request")
                .as_ref()
                .and_then(|request| request.executor.model()),
            Some("fallback")
        );
        let persisted = agent_task_lifecycle::load_plan("submitted-fallback").expect("plan");
        assert_eq!(persisted.tasks[0].executor.model(), Some("fallback"));
        assert_eq!(
            persisted.tasks[0].metadata["provider_readiness_routing"]["next_rotation_index"],
            1
        );
    });
}

#[test]
fn submitted_run_admission_denial_never_enters_running_or_spends_budget() {
    with_isolated_home(|_| {
        let missing = "__HOMEBOY_TEST_MISSING_SUBMITTED_ADMISSION_SECRET__";
        std::env::remove_var(missing);
        let mut plan = test_plan();
        plan.tasks[0].executor.secret_env.push(missing.to_string());
        agent_task_lifecycle::submit_plan(&plan, Some("submitted-missing-secret"))
            .expect("submitted plan");

        let error = run_submitted_with_timeout_and_catalog(
            "submitted-missing-secret".to_string(),
            None,
            Arc::new(SucceedingExecutor),
            &crate::agent_task_provider::AgentTaskProviderCatalog::default(),
        )
        .expect_err("missing secret blocks admission");
        let record = lifecycle_status("submitted-missing-secret").expect("durable failure");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_ne!(record.lifecycle.execution.state, RunExecutionState::Running);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "admit_plan_provider_dispatchability"
        );
        assert!(record
            .metadata
            .get("provider_executions")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty));
    });
}

#[test]
fn service_persists_timed_out_run_record_and_evidence_refs() {
    with_isolated_home(|_| {
        let result = run_loaded_plan(
            test_plan(),
            Some("service-timeout"),
            Arc::new(TimeoutExecutor),
        )
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
fn persisted_timeout_candidate_is_admitted_for_continuation() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        create_git_repo(workspace.path());
        let mut plan = test_plan();
        plan.tasks[0].workspace.root = Some(workspace.path().display().to_string());
        plan.tasks[0].limits.timeout_ms = Some(1);

        let result = run_loaded_plan(
            plan,
            Some("service-timeout-candidate"),
            Arc::new(TimeoutAfterWritingPatchExecutor),
        )
        .expect("timeout candidate run completed");
        assert_eq!(result.exit_code, 0);

        let lifecycle_store = test_lifecycle_store();
        let aggregate = lifecycle_store
            .read_aggregate("service-timeout-candidate")
            .expect("persisted aggregate");
        let outcome = aggregate.outcomes.first().expect("timeout outcome");
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
        let artifact = outcome
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "patch")
            .expect("persisted timeout patch");
        for key in [
            "run_id",
            "task_id",
            "producer_attempt",
            "base_ref",
            "provider_backend",
            "repository_identity",
            "workspace_identity",
        ] {
            assert!(
                artifact.metadata.get(key).is_some_and(|value| {
                    value.as_str().is_some_and(|value| !value.is_empty()) || value.is_u64()
                }),
                "persisted timeout patch is missing {key}: {:#?}",
                artifact.metadata
            );
        }
        assert_eq!(artifact.metadata["run_id"], "service-timeout-candidate");
        assert_eq!(artifact.metadata["task_id"], outcome.task_id);

        let source = serde_json::to_string(&aggregate).expect("serialize persisted aggregate");
        let admitted = crate::agent_task_promotion::preflight_recoverable_candidate_promotion_in_observation_store(
            &crate::agent_task_promotion::AgentTaskPromotionOptions {
                source,
                source_run_id: Some("service-timeout-candidate".to_string()),
                source_path: Some(lifecycle_store.aggregate_path("service-timeout-candidate")),
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "timeout-continuation-target".to_string(),
                task_id: Some(outcome.task_id.clone()),
                artifact_id: Some(artifact.id.clone()),
                dry_run: false,
                gates: crate::agent_task_gate::VerifyGateOptions::default(),
                provider_command: None,
                provider_invocation: None,
            },
            &lifecycle_store
                .open_observation_initialized()
                .expect("observation store"),
        )
        .expect("persisted timeout candidate is eligible for cook continuation");
        assert_eq!(admitted.id, artifact.id);
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
            Arc::new(CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            }),
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
            observed_root.contains("controller-scratch/attempts"),
            "attempt worktree should live under the registered controller scratch root, got {observed_root}"
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
fn run_next_quarantines_missing_required_secret_before_claiming_and_runs_later_work() {
    with_isolated_home(|_| {
        let mut plan = test_plan();
        plan.metadata = serde_json::json!({
            "attempt_dispatch": { "kind": "test-detached" }
        });
        plan.tasks[0]
            .executor
            .secret_env
            .push("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__".to_string());
        std::env::remove_var("__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__");
        agent_task_lifecycle::submit_plan(&plan, Some("run-next-a-missing-secret"))
            .expect("submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b-eligible"))
            .expect("eligible work submitted");

        let result = run_next(Arc::new(SucceedingExecutor)).expect("eligible work runs");
        let record =
            lifecycle_status("run-next-a-missing-secret").expect("quarantined record persisted");

        assert_eq!(
            result.value.expect("eligible aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].run_id, "run-next-a-missing-secret");
        assert_eq!(
            result.skipped[0].dispatcher_kind.as_deref(),
            Some("test-detached")
        );
        assert_eq!(
            result.skipped[0].category,
            "required_environment_preflight_failed"
        );
        assert_eq!(result.skipped[0].error_code, "validation.invalid_argument");
        assert_eq!(
            result.skipped[0].summary,
            "required environment is unavailable for queued execution"
        );
        assert_eq!(result.skipped[0].provider_id.as_deref(), Some("service"));
        assert_eq!(
            result.skipped[0].required_environment_variables,
            vec!["__HOMEBOY_TEST_MISSING_SECRET_ENV_RUN_NEXT__"]
        );
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.tasks[0].state, AgentTaskState::Queued);
        assert_eq!(record.lifecycle.execution.state, RunExecutionState::Queued);
        assert_eq!(
            record.metadata["queue_quarantine"]["error_code"],
            "validation.invalid_argument"
        );
        assert!(record.aggregate_path.is_none());
        assert_eq!(
            lifecycle_status("run-next-b-eligible")
                .expect("eligible record")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn run_next_quarantines_an_older_ineligible_record_and_executes_the_next_record() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-test-detached"))
            .expect("older record submitted");
        let plan_path = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-runs/run-next-test-detached/plan.json");
        let mut malformed: Value =
            serde_json::from_slice(&std::fs::read(&plan_path).expect("older plan"))
                .expect("plan JSON");
        malformed["options"]["execution_budget"]["version"] = serde_json::json!(999);
        std::fs::write(
            &plan_path,
            serde_json::to_vec(&malformed).expect("encode plan"),
        )
        .expect("persist malformed plan");

        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-eligible"))
            .expect("eligible record submitted");
        let calls = Arc::new(AtomicUsize::new(0));

        let result = run_next(Arc::new(CountingExecutor {
            calls: Arc::clone(&calls),
        }))
        .expect("next eligible record executes");
        let quarantined = agent_task_lifecycle::exact_record("run-next-test-detached")
            .expect("quarantined record");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.value.expect("aggregate").plan_id, "service-plan");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].run_id, "run-next-test-detached");
        assert_eq!(quarantined.state, AgentTaskRunState::Queued);
        assert_eq!(
            quarantined.metadata["queue_quarantine"]["category"],
            "queue_admission_preflight_failed"
        );
        assert_eq!(
            lifecycle_status("run-next-eligible")
                .expect("eligible record")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn run_next_bounds_stale_global_admission_and_reports_progress() {
    with_isolated_home(|_| {
        for index in 0..agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS {
            let run_id = format!("bounded-stale-{index:03}");
            agent_task_lifecycle::submit_plan(&test_plan(), Some(&run_id))
                .expect("stale submitted");
            let plan_path = homeboy_core::paths::homeboy_data()
                .expect("data path")
                .join("agent-task-runs")
                .join(&run_id)
                .join("plan.json");
            let mut plan: Value = serde_json::from_slice(&std::fs::read(&plan_path).expect("plan"))
                .expect("plan JSON");
            plan["options"]["execution_budget"]["version"] = serde_json::json!(999);
            std::fs::write(&plan_path, serde_json::to_vec(&plan).expect("encode plan"))
                .expect("persist stale plan");
        }
        agent_task_lifecycle::submit_plan(&test_plan(), Some("bounded-ready"))
            .expect("ready work submitted");

        let first = run_next(Arc::new(SucceedingExecutor)).expect("bounded claim returns");
        assert!(first.value.is_none());
        assert_eq!(
            first.queue_admission.inspected,
            agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS
        );
        assert!(first.queue_admission.limit_reached);
        assert_eq!(
            first.skipped.len(),
            agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS
        );

        let second =
            run_next(Arc::new(SucceedingExecutor)).expect("next claim progresses to ready work");
        assert_eq!(
            second.value.expect("ready aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(second.queue_admission.inspected, 1);
        assert!(!second.queue_admission.limit_reached);
        assert_eq!(
            lifecycle_status("bounded-ready")
                .expect("ready status")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn run_next_bounds_malformed_continuation_admission_and_progresses_on_retry() {
    with_isolated_home(|_| {
        let queue = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-cook-continuations");
        std::fs::create_dir_all(&queue).expect("continuation queue");
        for index in 0..=agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS {
            std::fs::write(queue.join(format!("{index:03}.pending")), "not JSON")
                .expect("malformed continuation");
        }
        agent_task_lifecycle::submit_plan(&test_plan(), Some("continuation-ready"))
            .expect("ready work submitted");

        let first =
            run_next(Arc::new(SucceedingExecutor)).expect("bounded continuation scan returns");
        assert!(first.value.is_none());
        assert_eq!(
            first.queue_admission.inspected,
            agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS
        );
        assert!(first.queue_admission.limit_reached);
        assert_eq!(
            std::fs::read_dir(&queue)
                .expect("continuation queue")
                .filter_map(std::result::Result::ok)
                .filter(
                    |entry| entry.path().extension().and_then(|value| value.to_str())
                        == Some("failed")
                )
                .count(),
            agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS
        );

        let second = run_next(Arc::new(SucceedingExecutor)).expect("later ready work progresses");
        assert_eq!(
            second.value.expect("ready aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(second.queue_admission.inspected, 2);
        assert!(!second.queue_admission.limit_reached);
    });
}

#[test]
fn run_next_shares_admission_budget_between_continuations_and_queued_records() {
    with_isolated_home(|_| {
        let queue = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-cook-continuations");
        std::fs::create_dir_all(&queue).expect("continuation queue");
        for index in 0..agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS - 1 {
            std::fs::write(queue.join(format!("{index:03}.pending")), "not JSON")
                .expect("malformed continuation");
        }
        agent_task_lifecycle::submit_plan(&test_plan(), Some("shared-stale"))
            .expect("stale submitted");
        let plan_path = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-runs/shared-stale/plan.json");
        let mut stale: Value =
            serde_json::from_slice(&std::fs::read(&plan_path).expect("plan")).expect("plan JSON");
        stale["options"]["execution_budget"]["version"] = serde_json::json!(999);
        std::fs::write(&plan_path, serde_json::to_vec(&stale).expect("encode plan"))
            .expect("persist stale plan");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("shared-ready"))
            .expect("ready submitted");

        let first = run_next(Arc::new(SucceedingExecutor)).expect("shared budget returns");
        assert!(first.value.is_none());
        assert_eq!(
            first.queue_admission.inspected,
            agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS
        );
        assert!(first.queue_admission.limit_reached);

        let second =
            run_next(Arc::new(SucceedingExecutor)).expect("ready work progresses next invocation");
        assert_eq!(
            second.value.expect("ready aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(second.queue_admission.inspected, 1);
    });
}

#[test]
fn run_next_quarantines_stale_cook_identity_and_runs_later_work() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("stale-identity"))
            .expect("stale Cook record submitted");
        agent_task_lifecycle::rewrite_record_for_test("stale-identity", |record| {
            record.metadata["cook_id"] = serde_json::json!("missing-cook-recipe");
        })
        .expect("stale Cook identity persisted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("identity-ready"))
            .expect("ready work submitted");

        let result = run_next(Arc::new(SucceedingExecutor)).expect("ready work runs");
        let stale = agent_task_lifecycle::exact_record("stale-identity").expect("stale record");

        assert_eq!(
            result.value.expect("ready aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].run_id, "stale-identity");
        assert!(stale.metadata.get("queue_quarantine").is_some());
        assert_eq!(
            lifecycle_status("identity-ready")
                .expect("ready status")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn run_next_redacts_adversarial_provider_readiness_diagnostics_everywhere() {
    with_isolated_home(|_| {
        const LEAKS: [&str; 5] = [
            "LEAK_READINESS_REASON",
            "LEAK_REMEDIATION",
            "LEAK_IDENTITY",
            "LEAK_COMMAND_OUTPUT",
            "LEAK_ENV_VALUE",
        ];
        let temp = tempfile::tempdir().expect("temporary readiness provider");
        let script = temp.path().join("adversarial-readiness.js");
        std::fs::write(
            &script,
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'LEAK_COMMAND_OUTPUT',retryable:false,remediation:'LEAK_REMEDIATION',reason:'LEAK_READINESS_REASON',cache_key:'LEAK_ENV_VALUE',identity:{value:'LEAK_IDENTITY'}}));",
        )
        .expect("readiness script written");
        let mut provider: crate::agent_task_provider::AgentTaskExecutorProvider =
            serde_json::from_value(serde_json::json!({
                "id": "locally-trusted-readiness-provider",
                "backend": "adversarial-readiness"
            }))
            .expect("provider fixture");
        provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec!["node".to_string(), script.display().to_string()],
                ..CommandInvocation::default()
            }
            .into(),
        );
        assert_eq!(provider.backend, "adversarial-readiness");
        assert!(provider.readiness_invocation.is_some());

        let mut adversarial = test_plan();
        adversarial.tasks[0].executor.backend = "adversarial-readiness".to_string();
        adversarial.tasks[0].executor.selector = None;
        assert_eq!(
            adversarial.tasks[0].executor.backend,
            "adversarial-readiness"
        );
        crate::agent_task_provider::preflight_plan_provider_runtime_readiness_with_providers(
            &adversarial,
            &[provider.clone()],
            &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("adversarial readiness provider rejects the plan");
        agent_task_lifecycle::submit_plan(&adversarial, Some("run-next-a-adversarial-readiness"))
            .expect("adversarial run submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b-eligible"))
            .expect("eligible run submitted");

        let result = run_next_with_cook_dispatcher_and_queue_preflight(
            Arc::new(SucceedingExecutor),
            |_| Ok(None),
            None,
            |_, plan| {
                if plan.tasks[0].executor.backend == "adversarial-readiness" {
                    crate::agent_task_provider::preflight_plan_provider_runtime_readiness_with_providers(
                        plan,
                        &[provider.clone()],
                        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
                    )
                } else {
                    Ok(())
                }
            },
        )
        .expect("adversarial readiness skip does not block eligible work");
        let record =
            lifecycle_status("run-next-a-adversarial-readiness").expect("quarantined record");
        let status = agent_task_lifecycle::reconcile_status("run-next-a-adversarial-readiness")
            .expect("status projection");
        let logs = agent_task_lifecycle::logs("run-next-a-adversarial-readiness")
            .expect("logs projection");
        let rendered = serde_json::to_string(&serde_json::json!({
            "record": record,
            "status": status,
            "logs": logs,
            "queue_skips": result.skipped,
        }))
        .expect("queue projections serialize");

        assert_eq!(
            result.value.expect("eligible aggregate").plan_id,
            "service-plan"
        );
        assert_eq!(
            record.metadata["queue_quarantine"]["category"],
            "queue_admission_preflight_failed"
        );
        assert_eq!(
            record.metadata["queue_quarantine"]["error_code"],
            "validation.invalid_argument"
        );
        assert!(record.metadata["queue_quarantine"].get("details").is_none());
        assert!(record.metadata["queue_quarantine"].get("reason").is_none());
        for leak in LEAKS {
            assert!(
                !rendered.contains(leak),
                "redacted queue output leaked {leak}"
            );
        }
    });
}

#[test]
fn run_submitted_selects_an_exact_run_id_without_claiming_older_queued_work() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-exact-older"))
            .expect("older run submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-exact-target"))
            .expect("target run submitted");
        let calls = Arc::new(AtomicUsize::new(0));

        let result = run_submitted(
            "run-exact-target".to_string(),
            Arc::new(CountingExecutor {
                calls: Arc::clone(&calls),
            }),
        )
        .expect("exact run executes");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.value.plan_id, "service-plan");
        assert_eq!(
            lifecycle_status("run-exact-older")
                .expect("older record")
                .state,
            AgentTaskRunState::Queued
        );
        assert_eq!(
            lifecycle_status("run-exact-target")
                .expect("target record")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn quarantine_and_cancellation_race_keeps_cancellation_terminal() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("quarantine-cancel-race"))
            .expect("run submitted");
        let barrier = Arc::new(Barrier::new(2));
        let cancel_barrier = Arc::clone(&barrier);
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            agent_task_lifecycle::cancel_run("quarantine-cancel-race", Some("operator cancel"))
        });
        barrier.wait();
        let quarantine = agent_task_lifecycle::quarantine_queued_run_exact_in_store(
            &test_lifecycle_store(),
            "quarantine-cancel-race",
            "operator quarantine",
        );

        let cancelled = cancel.join().expect("cancellation thread completes");
        let record = lifecycle_status("quarantine-cancel-race").expect("durable record");

        assert!(cancelled.is_ok());
        if quarantine.is_ok() {
            assert_eq!(
                record.metadata["queue_quarantine"]["category"],
                "operator_quarantine"
            );
        }
        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.metadata["cancel_reason"], "operator cancel");
    });
}

#[test]
fn quarantined_run_requires_explicit_rearm_before_it_is_eligible() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("quarantine-rearm"))
            .expect("run submitted");
        let operator_reason = format!("maintenance\n{}", "x".repeat(300));
        let quarantined = agent_task_lifecycle::quarantine_queued_run_exact_in_store(
            &test_lifecycle_store(),
            "quarantine-rearm",
            &operator_reason,
        )
        .expect("exact queued run quarantined");

        assert_eq!(quarantined.state, AgentTaskRunState::Queued);
        assert!(!quarantined.state.is_terminal());
        assert_eq!(
            quarantined.metadata["queue_quarantine"]["category"],
            "operator_quarantine"
        );
        assert_eq!(
            quarantined.metadata["queue_quarantine"]["operator_reason"]
                .as_str()
                .expect("normalized operator reason")
                .len(),
            240
        );
        assert!(!quarantined.metadata["queue_quarantine"]["operator_reason"]
            .as_str()
            .expect("normalized operator reason")
            .contains('\n'));
        assert!(agent_task_lifecycle::mark_running("quarantine-rearm").is_err());

        let rearmed = agent_task_lifecycle::rearm_quarantined_run_in_store(
            &test_lifecycle_store(),
            "quarantine-rearm",
        )
        .expect("exact quarantined run rearmed");
        assert_eq!(rearmed.state, AgentTaskRunState::Queued);
        assert!(rearmed.metadata.get("queue_quarantine").is_none());
    });
}

#[test]
fn quarantine_and_rearm_reject_sanitized_aliases_without_mutating_the_literal_record() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("foo_bar"))
            .expect("literal record submitted");

        let quarantine_error = agent_task_lifecycle::quarantine_queued_run_exact_in_store(
            &test_lifecycle_store(),
            "foo/bar",
            "bad",
        )
        .expect_err("sanitized alias rejected");
        let unchanged = lifecycle_status("foo_bar").expect("literal record remains queued");
        assert_eq!(
            quarantine_error.code.as_str(),
            "validation.invalid_argument"
        );
        assert_eq!(unchanged.state, AgentTaskRunState::Queued);
        assert!(unchanged.metadata.get("queue_quarantine").is_none());

        agent_task_lifecycle::quarantine_queued_run_exact_in_store(
            &test_lifecycle_store(),
            "foo_bar",
            "explicit quarantine",
        )
        .expect("literal record quarantined");
        let rearm_error = agent_task_lifecycle::rearm_quarantined_run_in_store(
            &test_lifecycle_store(),
            "foo/bar",
        )
        .expect_err("sanitized alias rejected");
        let quarantined = lifecycle_status("foo_bar").expect("quarantine remains intact");
        assert_eq!(rearm_error.code.as_str(), "validation.invalid_argument");
        assert_eq!(quarantined.state, AgentTaskRunState::Queued);
        assert_eq!(
            quarantined.metadata["queue_quarantine"]["category"],
            "operator_quarantine"
        );
        assert_eq!(
            quarantined.metadata["queue_quarantine"]["operator_reason"],
            "explicit quarantine"
        );
    });
}

#[test]
fn discovery_lists_durable_runs_with_operator_commands() {
    with_isolated_home(|_| {
        let mut plan = discovery_plan();
        plan.metadata["cook_repository_identity"] = serde_json::json!({
            "repository_name": "homeboy",
            "component_id": "homeboy-cli"
        });
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
        // The prose apology this replaced told operators to go run a second,
        // runner-scoped command themselves. Discovery now points at the surface
        // that federates runner-resident records instead (#W3-15).
        assert!(report.federated_command.contains("homeboy activity"));
        assert_eq!(run.run_id, "run-discovery-list");
        assert_eq!(run.state, AgentTaskRunState::Queued);
        assert_eq!(run.repo.as_deref(), Some("homeboy"));
        assert_eq!(run.component.as_deref(), Some("homeboy-cli"));
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
            Arc::new(SucceedingExecutor),
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
fn pending_detached_cook_handoff_is_discoverable_before_attempt_materialization() {
    with_isolated_home(|_| {
        let cook_id = "pending-detached-cook-handoff";
        let parent = agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("persist handoff parent before child materialization");

        // The handoff parent is the durable operator handle while no provider
        // attempt exists yet, so both discovery views must expose it.
        assert!(parent.tasks.is_empty());
        assert_eq!(
            parent.metadata["detached_cook_handoff"]["admission_state"],
            "pre_supervisor"
        );
        assert!(agent_task_lifecycle::has_pending_detached_cook_handoff(
            &parent
        ));
        let mut legacy_parent = parent.clone();
        legacy_parent.metadata["detached_cook_handoff"]
            .as_object_mut()
            .expect("handoff metadata")
            .remove("admission_state");
        assert!(
            agent_task_lifecycle::has_pending_detached_cook_handoff(&legacy_parent),
            "records written before admission_state remain protected from recovery"
        );
        let active = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active discovery");
        assert!(active.runs.iter().any(|run| run.run_id == cook_id));
        let all = discover_runs(AgentTaskDiscoveryFilter::All).expect("all discovery");
        assert!(all.runs.iter().any(|run| run.run_id == cook_id));
    });
}

#[test]
fn discovery_active_reads_runner_backed_record_without_reconciliation() {
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
        assert_eq!(run.stale, None);
        assert_eq!(run.stale_reason, None);
        assert_eq!(run.retryable, None);
        assert_eq!(run.liveness, Some(AgentTaskLiveness::Active));
    });
}

/// #W3-4: `is_reconcilable` is now public and emitted as `liveness_reconcilable`,
/// and the CLI's hand-rolled four-way bucketing was replaced by it. This pins
/// the predicate against the bucketing it replaced for **all four** variants:
/// the CLI grouped `Some(Active) | None` into `active` and everything else into
/// a named non-active bucket, so "not in the active bucket" must be exactly
/// "reconcilable". Any divergence here is a behaviour change.
#[test]
fn is_reconcilable_matches_the_replaced_cli_bucketing_for_every_variant() {
    for (liveness, expected) in [
        (AgentTaskLiveness::Active, false),
        (AgentTaskLiveness::Stale, true),
        (AgentTaskLiveness::Suspect, true),
        (AgentTaskLiveness::Unreconciled, true),
    ] {
        assert_eq!(
            liveness.is_reconcilable(),
            expected,
            "{liveness:?} reconcilability"
        );
        // The CLI's `active` bucket was `Some(Active) | None`; a classified run
        // lands in the `active` bucket exactly when it is not reconcilable.
        assert_eq!(
            liveness.as_str() == "active",
            !liveness.is_reconcilable(),
            "{liveness:?} bucket membership"
        );
        // The wire label the CLI keys buckets by is the same string the enum
        // serializes to, so bucketing by `as_str()` cannot drift from `liveness`.
        assert_eq!(
            serde_json::to_value(liveness).expect("serialize liveness"),
            serde_json::Value::String(liveness.as_str().to_string())
        );
    }

    // An unclassified run (the `all`/`latest` filters do not classify) defaults
    // to Active, which is what the replaced `None` arm did.
    assert!(!Option::<AgentTaskLiveness>::None
        .unwrap_or(AgentTaskLiveness::Active)
        .is_reconcilable());

    assert_eq!(AgentTaskLiveness::ALL.len(), 4);
}

#[test]
fn discovery_active_classifies_liveness_and_source() {
    with_isolated_home(|_| {
        // A queued run without a live owner or submission lease is stale.
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
                "stale_running": true,
            });
        })
        .expect("stale runner-backed record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");

        let queued = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-queued")
            .expect("queued listed");
        assert_eq!(queued.liveness, Some(AgentTaskLiveness::Stale));
        assert!(queued.source == "local" || queued.source.starts_with("runner:"));

        let stale = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-live-stale")
            .expect("stale listed");
        assert_eq!(stale.liveness, Some(AgentTaskLiveness::Stale));
        assert_eq!(stale.source, "runner:homeboy-lab");

        // The reconcilable predicate travels with the classification, so a
        // consumer never has to map the four values itself (#W3-4).
        assert_eq!(queued.liveness_reconcilable, Some(true));
        assert_eq!(stale.liveness_reconcilable, Some(true));

        let summary = report.liveness_summary.expect("active summary present");
        assert_eq!(summary.active, 0);
        assert_eq!(summary.stale, 2);
        assert_eq!(
            summary.reconcilable,
            summary.stale + summary.suspect + summary.unreconciled
        );
        assert_eq!(
            summary.reconcile_command,
            "homeboy agent-task active --reconcile --dry-run"
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
            record.metadata = serde_json::json!({ "runner_pid": u32::MAX });
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
            record.metadata = serde_json::json!({ "runner_pid": u32::MAX });
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
fn scoped_reconcile_applies_only_the_inspected_run_and_preserves_other_stale_records() {
    with_isolated_home(|_| {
        for run_id in ["run-scoped-target", "run-scoped-unrelated"] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({ "runner_pid": u32::MAX });
            })
            .expect("stale record stored");
        }

        // Normalize the unrelated record before snapshotting it. The scoped
        // operation must not write it while reconciling the requested run.
        let unrelated_before = serde_json::to_value(
            lifecycle_status("run-scoped-unrelated").expect("unrelated status"),
        )
        .expect("serialize unrelated record");

        let preview = reconcile_run("run-scoped-target", true).expect("scoped preview");
        assert_eq!(preview.scope, "run:run-scoped-target");
        assert_eq!(preview.authorization, "preview");
        assert_eq!(preview.considered, 1);
        assert_eq!(preview.runs[0].action, "would-reconcile");
        assert_eq!(
            lifecycle_status("run-scoped-target")
                .expect("target after preview")
                .state,
            AgentTaskRunState::Running
        );

        let applied = reconcile_run("run-scoped-target", false).expect("scoped apply");
        assert_eq!(applied.authorization, "explicit-apply");
        assert_eq!(applied.reconciled, 1);
        assert_eq!(
            lifecycle_status("run-scoped-target")
                .expect("target after apply")
                .state,
            AgentTaskRunState::Cancelled
        );
        assert_eq!(
            serde_json::to_value(
                lifecycle_status("run-scoped-unrelated").expect("unrelated after")
            )
            .expect("serialize unrelated record"),
            unrelated_before
        );
    });
}

#[test]
fn scoped_reconcile_keeps_exact_attempts_separate_and_expands_cook_aliases_to_their_parent() {
    with_isolated_home(|_| {
        let cook_id = "cook-reconcile-alias";
        let attempt_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&attempt_id)).expect("attempt");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            // Model the durable parent/attempt link while both projections are
            // stale after normal handoff completion has redirected the parent.
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_id);
            record.metadata["detached_cook_handoff"]["state"] = serde_json::json!("redirected");
        })
        .expect("link stale parent projection");
        for run_id in [cook_id, attempt_id.as_str()] {
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
                record.metadata["runner_pid"] = serde_json::json!(u32::MAX);
                if record.run_id == attempt_id {
                    record.metadata["cook_id"] = serde_json::json!(cook_id);
                }
            })
            .expect("stale record");
        }
        index_cook_attempt(cook_id, &attempt_id);

        let exact = reconcile_run(&attempt_id, true).expect("exact attempt preview");
        assert_eq!(exact.scope, format!("run:{attempt_id}"));
        assert_eq!(exact.resolved_run_ids, vec![attempt_id.clone()]);
        assert_eq!(exact.runs.len(), 1);

        let alias = reconcile_run(cook_id, true).expect("logical Cook preview");
        assert_eq!(alias.scope, format!("run:{cook_id}"));
        assert_eq!(alias.requested_run_id.as_deref(), Some(cook_id));
        assert_eq!(
            alias.resolved_run_ids,
            vec![cook_id.to_string(), attempt_id.clone()]
        );
        assert_eq!(alias.runs.len(), 2);

        let applied = reconcile_run(cook_id, false).expect("logical Cook apply");
        assert_eq!(applied.reconciled, 2, "{applied:?}");
        assert!(applied.runs.iter().all(|run| run.action == "reconciled"));
        for run_id in [cook_id, attempt_id.as_str()] {
            assert_eq!(
                agent_task_lifecycle::exact_record(run_id)
                    .expect("exact durable record")
                    .state,
                AgentTaskRunState::Cancelled,
                "{run_id} must be cancelled through its own exact record"
            );
        }
    });
}

#[test]
fn upgrade_admission_dedupes_linked_parent_and_attempt_recovery_commands() {
    with_isolated_home(|_| {
        let cook_id = "cook-upgrade-alias";
        let attempt_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_disconnected(),
        ));
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&attempt_id)).expect("attempt");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_id);
        })
        .expect("link parent");
        for run_id in [cook_id, attempt_id.as_str()] {
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
                if record.run_id == cook_id {
                    record.metadata["runner_pid"] = serde_json::json!(std::process::id());
                } else {
                    record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
                    record.updated_at = None;
                    record.metadata["cook_id"] = serde_json::json!(cook_id);
                    record.metadata["runner_id"] = serde_json::json!("lab");
                    record.metadata["runner_job_id"] = serde_json::json!("stale-job");
                }
            })
            .expect("live linked record");
        }
        index_cook_attempt(cook_id, &attempt_id);

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].run_id, cook_id);
        assert_eq!(
            admission.blockers[0].recovery_command,
            "homeboy runner reconcile lab"
        );
        assert_eq!(admission.blockers[0].owner, "runner_generations");
        assert_eq!(admission.blockers[0].action, "homeboy runner reconcile lab");
    });
}

#[test]
fn upgrade_admission_inspects_an_ambiguous_removed_runner_record_locally() {
    with_isolated_home(|_| {
        let cook_id = "concurrent-first-cook-run";
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::removed(),
        ));
        record_stale_accepted_lab_handoff(cook_id, "fixture-lab");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        let blocker = &admission.blockers[0];
        assert_eq!(blocker.run_id, cook_id);
        assert_eq!(blocker.owner, "durable_agent_tasks");
        assert_eq!(
            blocker.recovery_command,
            "homeboy --placement local agent-task reconcile concurrent-first-cook-run --dry-run"
        );
        assert!(!blocker.recovery_command.contains("cancel"));
    });
}

#[test]
fn upgrade_admission_keeps_configured_disconnected_runner_ownership() {
    with_isolated_home(|_| {
        let cook_id = "configured-offline-cook-run";
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_disconnected(),
        ));
        record_stale_accepted_lab_handoff(cook_id, "fixture-lab");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].owner, "runner_generations");
        assert_eq!(
            admission.blockers[0].recovery_command,
            "homeboy runner reconcile fixture-lab"
        );
        let report = reconcile_run(cook_id, false).expect("configured runner remains fail-closed");
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.runs[0].action, "no-op");
        assert_eq!(
            lifecycle_status(cook_id)
                .expect("remote record retained")
                .state,
            AgentTaskRunState::Running
        );
    });
}

#[test]
fn upgrade_admission_repairs_ownerless_queued_runner_record_after_zero_live_reconciliation() {
    with_isolated_home(|_| {
        let cook_id = "cook-queued-after-runner-reconcile";
        let run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_idle(),
        ));
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&run_id)).expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test(&run_id, |record| {
            record.metadata["cook_id"] = serde_json::json!(cook_id);
            record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
            record.metadata["runner_job_id"] = serde_json::json!("stale-zero-live-job");
            record.metadata["provider_executions_consumed"] = serde_json::json!(0);
            record
                .metadata
                .as_object_mut()
                .expect("metadata")
                .remove("runner_pid");
        })
        .expect("record ownerless queued runner child");

        let queued = agent_task_lifecycle::exact_record(&run_id).expect("queued record");
        assert!(queued.is_ownerless_zero_artifact_queued_runner_record());
        assert!(queued.is_locally_reconcilable_after_runner_idle());
        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let blocked =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(blocked.blockers.len(), 1);
        assert_eq!(blocked.blockers[0].run_id, run_id);
        assert_eq!(blocked.blockers[0].owner, "durable_agent_tasks");
        assert_eq!(
            blocked.blockers[0].reason,
            "ownerless_queued_after_runner_reconciliation"
        );
        assert_eq!(
            blocked.blockers[0].recovery_command,
            format!("homeboy --placement local agent-task reconcile {run_id} --apply")
        );
        assert!(!blocked.blockers[0]
            .recovery_command
            .contains("runner reconcile"));

        let repaired = reconcile_run(&run_id, false).expect("bounded agent-task repair");
        assert_eq!(repaired.reconciled, 1, "{repaired:?}");
        assert_eq!(repaired.runs[0].action, "reconciled");
        assert_eq!(
            agent_task_lifecycle::exact_record(&run_id)
                .expect("terminal repaired record")
                .state,
            AgentTaskRunState::Cancelled
        );
        assert!(
            !discover_runs(AgentTaskDiscoveryFilter::Active)
                .expect("fresh active discovery")
                .runs
                .iter()
                .any(|run| run.run_id == run_id),
            "a successful runner-owned reconciliation must not survive rediscovery"
        );

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admitted =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert!(admitted.allows_controller_replacement(), "{admitted:?}");
        assert!(admitted.blockers.is_empty());
    });
}

#[test]
fn control_plane_reconciliation_retains_its_claim_across_runner_terminal_projection() {
    with_isolated_home(|_| {
        let run_id = "cook-runner-reconcile-claim-attempt-1-transport-retry";
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_idle(),
        ));
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        let execution_context =
            homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(
                Some(run_id),
                "homeboy-lab",
                "00000000-0000-4000-8000-000000000001",
                "homeboy",
                "reservation-1",
            )
            .expect("accepted runner execution context");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
            record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
            record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
            record.metadata["runner_job_id"] = serde_json::json!(execution_context.runner_job_id());
            record.metadata["runner_execution_context"] = execution_context
                .evidence_record()
                .expect("execution context evidence");
            record.metadata["provider_executions_consumed"] = serde_json::json!(0);
            record
                .metadata
                .as_object_mut()
                .expect("metadata")
                .remove("runner_pid");
        })
        .expect("ownerless runner record");
        let request = homeboy_control_plane_contract::ControlPlaneActionRequest {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: homeboy_control_plane_contract::ControlPlaneAction::Reconcile,
            idempotency_key: "reconcile-runner-claim-1".to_string(),
            actor: "test".to_string(),
            expected_updated_at: None,
            parameters: homeboy_control_plane_contract::ControlPlaneActionPayload::empty(),
            confirmed: true,
        };

        let first = crate::orchestration::execute_action_from_current_environment(run_id, &request)
            .expect("reconciliation action");
        assert_eq!(
            first.outcome,
            homeboy_control_plane_contract::ControlPlaneActionOutcome::Succeeded
        );
        assert_eq!(
            agent_task_lifecycle::exact_record(run_id)
                .expect("terminal record")
                .state,
            AgentTaskRunState::Cancelled
        );
        let operation_key = format!("control-plane-action:reconcile:{}", request.idempotency_key);
        assert_eq!(
            agent_task_lifecycle::operation_claim(run_id, &operation_key)
                .expect("operation claim")
                .expect("completed operation claim")
                .state,
            agent_task_lifecycle::ClaimState::Completed
        );
        assert_eq!(
            crate::orchestration::execute_action_from_current_environment(run_id, &request)
                .expect("replayed reconciliation"),
            first
        );
    });
}

#[test]
fn record_scoped_reconciliation_stays_with_its_explicit_lifecycle_store() {
    with_isolated_home(|home| {
        let run_id = "queued-in-explicit-store";
        let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(
            home.path().join("explicit-lifecycle"),
        );
        agent_task_lifecycle::submit_plan_in_store(
            &lifecycle_store,
            &discovery_plan(),
            Some(run_id),
        )
        .expect("submitted in explicit store");
        lifecycle_store
            .mutate_record(run_id, |record| {
                record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
                record.updated_at = None;
                true
            })
            .expect("stale explicit record");

        let repaired = reconcile_run_in_store(&lifecycle_store, run_id, false)
            .expect("explicit-store reconciliation");
        assert_eq!(repaired.reconciled, 1, "{repaired:?}");
        assert_eq!(repaired.runs[0].action, "reconciled");
        assert_eq!(
            lifecycle_store
                .read_record(run_id)
                .expect("terminal explicit record")
                .state,
            AgentTaskRunState::Cancelled
        );
        assert!(agent_task_lifecycle::exact_record(run_id).is_err());
    });
}

#[test]
fn reconciliation_postcondition_names_an_unresolved_runner_projection() {
    with_isolated_home(|_| {
        let run_id = "queued-runner-projection-postcondition";
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Cancelled);
            record.tasks[0].state = AgentTaskState::Cancelled;
        })
        .expect("terminal controller projection");
        let record = agent_task_lifecycle::exact_record(run_id).expect("queued record");

        let error = super::reconcile::verify_reconciled_postcondition(&record, false, false)
            .expect_err("an unresolved runner projection cannot report reconciliation success");
        assert!(error.message.contains(run_id));
        assert!(error.message.contains("durable state is cancelled"));
        assert!(error.message.contains("on the runner"));
    });
}

#[test]
fn upgrade_admission_keeps_ownerless_queued_runner_record_on_runner_plane_without_idle_evidence() {
    with_isolated_home(|_| {
        let run_id = "queued-without-idle-runner-evidence";
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_disconnected(),
        ));
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
            record.metadata["runner_job_id"] = serde_json::json!("unverified-job");
            record.metadata["provider_executions_consumed"] = serde_json::json!(0);
            record
                .metadata
                .as_object_mut()
                .expect("metadata")
                .remove("runner_pid");
        })
        .expect("record ownerless queued runner child");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].owner, "runner_generations");
        assert_eq!(
            admission.blockers[0].recovery_command,
            "homeboy runner reconcile homeboy-lab"
        );
        let report = reconcile_run(run_id, false).expect("runner plane remains fail-closed");
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.runs[0].action, "no-op");
    });
}

#[test]
fn idle_runner_evidence_does_not_reclaim_live_or_ambiguous_queued_records() {
    with_isolated_home(|_| {
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_idle(),
        ));
        let now = chrono::Utc::now();
        for run_id in ["queued-without-job", "queued-planned", "queued-supervised"] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
                record.metadata["runner_job_id"] = serde_json::json!("stale-job");
                match run_id {
                    "queued-without-job" => {
                        record
                            .metadata
                            .as_object_mut()
                            .expect("metadata")
                            .remove("runner_job_id");
                    }
                    "queued-planned" => {
                        record.metadata["runner_execution_record"] = serde_json::json!({
                            "status": "planned",
                            "agent_task_run_id": run_id,
                            "runner_id": "homeboy-lab",
                        });
                    }
                    "queued-supervised" => {
                        record.metadata["cook_id"] = serde_json::json!("supervised-cook");
                        record.metadata["local_cook_supervisor"] = serde_json::json!({
                            "state": "supervising",
                            "pinned_run_id": run_id,
                            "lease_started_at": now.to_rfc3339(),
                            "lease_expires_at": (now
                                + chrono::Duration::seconds(
                                    agent_task_lifecycle::LOCAL_COOK_SUPERVISOR_LEASE_SECONDS
                                ))
                            .to_rfc3339(),
                        });
                    }
                    _ => unreachable!(),
                }
            })
            .expect("record queued runner state");

            let record = agent_task_lifecycle::exact_record(run_id).expect("queued record");
            assert!(!record.is_ownerless_zero_artifact_queued_runner_record());
            assert!(!record.is_locally_reconcilable_after_runner_idle());
        }

        record_stale_accepted_lab_handoff("accepted-idle-runner", "homeboy-lab");
        let accepted = agent_task_lifecycle::exact_record("accepted-idle-runner")
            .expect("accepted runner record");
        assert!(!accepted.is_ownerless_zero_artifact_queued_runner_record());
        assert!(!accepted.is_locally_reconcilable_after_runner_idle());
    });
}

#[test]
fn upgrade_admission_keeps_provider_unavailable_runner_ownership() {
    with_isolated_home(|_| {
        let cook_id = "unknown-offline-cook-run";
        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::unknown(),
        ));
        record_stale_accepted_lab_handoff(cook_id, "fixture-lab");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].owner, "runner_generations");
        assert_eq!(
            admission.blockers[0].recovery_command,
            "homeboy runner reconcile fixture-lab"
        );
        let report = reconcile_run(cook_id, false).expect("unknown runner remains fail-closed");
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.runs[0].action, "no-op");
        assert_eq!(
            lifecycle_status(cook_id)
                .expect("remote record retained")
                .state,
            AgentTaskRunState::Running
        );
    });
}

#[test]
fn scoped_reconcile_rejects_a_parent_child_link_with_disagreeing_cook_identity() {
    with_isolated_home(|_| {
        let cook_id = "cook-reconcile-parent";
        let attempt_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&attempt_id)).expect("attempt");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_id);
        })
        .expect("link parent");
        agent_task_lifecycle::rewrite_record_for_test(&attempt_id, |record| {
            record.metadata["cook_id"] = serde_json::json!("other-cook");
        })
        .expect("write disagreement");
        index_cook_attempt(cook_id, &attempt_id);

        let error = reconcile_run(cook_id, true).expect_err("reject mismatched child identity");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("does not belong"));
    });
}

#[test]
fn scoped_reconcile_rejects_a_parent_child_link_without_cook_index_authority() {
    with_isolated_home(|_| {
        let cook_id = "cook-reconcile-unindexed";
        let attempt_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&attempt_id)).expect("attempt");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_id);
        })
        .expect("link parent");
        agent_task_lifecycle::rewrite_record_for_test(&attempt_id, |record| {
            record.metadata["cook_id"] = serde_json::json!(cook_id);
        })
        .expect("write matching Cook identity");

        let error = reconcile_run(cook_id, true).expect_err("reject unindexed child link");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("no Cook index authority"));
    });
}

#[test]
fn upgrade_admission_keeps_an_unindexed_handoff_child_independent_with_executable_remediation() {
    with_isolated_home(|_| {
        let cook_id = "cook-upgrade-unindexed";
        let attempt_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(&attempt_id)).expect("attempt");
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_id);
        })
        .expect("unindexed parent link");
        agent_task_lifecycle::rewrite_record_for_test(&attempt_id, |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
            record.metadata["cook_id"] = serde_json::json!(cook_id);
        })
        .expect("unindexed child");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 2);
        assert!(admission.blockers.iter().any(|blocker| {
            blocker.run_id == cook_id
                && blocker.recovery_command == "homeboy agent-task reconcile-records --dry-run"
        }));
        assert!(admission.blockers.iter().any(|blocker| {
            blocker.run_id == attempt_id
                && blocker.recovery_command
                    == format!("homeboy --placement local agent-task status {attempt_id}")
        }));
        assert!(agent_task_lifecycle::reconcile_record_health_in_store(
            &test_lifecycle_store(),
            true
        )
        .is_ok());
    });
}

#[test]
fn scoped_reconcile_uses_validated_handoff_child_not_later_index_attempt() {
    with_isolated_home(|_| {
        let cook_id = "cook-reconcile-accepted-child";
        let attempt_one = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        let attempt_two = agent_task_lifecycle::cook_attempt_run_id(cook_id, 2);
        agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
            &test_lifecycle_store(),
            cook_id,
        )
        .expect("parent");
        for run_id in [&attempt_one, &attempt_two] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("attempt");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["cook_id"] = serde_json::json!(cook_id);
            })
            .expect("record Cook identity");
        }
        agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
            record.metadata["detached_cook_handoff"]["attempt_run_id"] =
                serde_json::json!(&attempt_one);
        })
        .expect("bind accepted child");
        index_cook_attempts(cook_id, &attempt_one, &attempt_two);

        let scope = agent_task_lifecycle::reconcile_scope_run_ids_in_store(
            &agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store"),
            cook_id,
        )
        .expect("scope");
        assert_eq!(scope, vec![cook_id.to_string(), attempt_one]);
        assert!(!scope.contains(&attempt_two));
    });
}

#[test]
fn scoped_reconcile_is_a_no_op_when_the_owner_becomes_live_after_preview() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-owner-changed"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-owner-changed", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = serde_json::json!({ "runner_pid": u32::MAX });
        })
        .expect("stale record stored");

        assert_eq!(
            reconcile_run("run-owner-changed", true)
                .expect("preview")
                .runs[0]
                .action,
            "would-reconcile"
        );
        agent_task_lifecycle::rewrite_record_for_test("run-owner-changed", |record| {
            record.metadata = serde_json::json!({ "runner_pid": std::process::id() });
        })
        .expect("live owner stored");

        let report = reconcile_run("run-owner-changed", false).expect("apply after owner change");
        assert_eq!(report.reconciled, 0);
        assert_eq!(report.runs[0].action, "no-op");
        assert_eq!(
            lifecycle_status("run-owner-changed")
                .expect("owner-changed record")
                .state,
            AgentTaskRunState::Running
        );
    });
}

#[test]
fn dead_owner_process_run_is_classified_stale_and_reconciled() {
    // Regression for #9718: a `running` record whose owner process is dead but
    // whose `runner_pid` is merely PRESENT (and heartbeat not yet age-stale)
    // was classified Active by discovery and so was never terminalized by
    // `active --reconcile`. It must now classify Stale and reconcile to
    // Cancelled without a manual `cancel`.
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-dead-owner"))
            .expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test("run-dead-owner", |record| {
            agent_task_lifecycle::set_run_state(record, AgentTaskRunState::Running);
            record.tasks[0].state = AgentTaskState::Running;
            // Fresh update timestamp + a present-but-dead owner pid, and NO
            // runner job (local controller-owned run). A very large PID is not
            // a live process on this host.
            record.metadata = serde_json::json!({
                "runner_pid": i32::MAX as u32,
            });
        })
        .expect("dead-owner record stored");

        let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
        let ghost = report
            .runs
            .iter()
            .find(|run| run.run_id == "run-dead-owner")
            .expect("ghost listed");
        assert_eq!(ghost.liveness, Some(AgentTaskLiveness::Stale));

        let reconciled = reconcile_stale_active_runs(false).expect("reconciled");
        assert_eq!(reconciled.reconciled, 1);
        assert_eq!(reconciled.failed, 0);

        let record = lifecycle_status("run-dead-owner").expect("status");
        assert_eq!(record.state, AgentTaskRunState::Cancelled);
    });
}

#[test]
fn concurrent_provider_children_keep_liveness_attributed_to_their_own_processes() {
    with_isolated_home(|_| {
        let mut child_a = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("start first provider child");
        let mut child_b = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("start second provider child");

        let result = (|| {
            for (run_id, pid) in [("run-child-a", child_a.id()), ("run-child-b", child_b.id())] {
                let plan = discovery_plan();
                agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit child");
                agent_task_lifecycle::mark_running(run_id).expect("mark child running");
                agent_task_lifecycle::reserve_provider_execution_in_store(
                    &test_lifecycle_store(),
                    run_id,
                    &plan.tasks[0],
                    1,
                )
                .expect("reserve provider execution");
                agent_task_lifecycle::record_provider_execution_process(
                    run_id,
                    &plan.tasks[0].task_id,
                    1,
                    pid,
                )
                .expect("bind provider process");
            }

            let active =
                discover_runs(AgentTaskDiscoveryFilter::Active).expect("discover children");
            assert!(active
                .runs
                .iter()
                .all(|run| run.liveness == Some(AgentTaskLiveness::Active)));

            child_a.kill().expect("kill first provider child");
            child_a.wait().expect("reap first provider child");
            let observed =
                discover_runs(AgentTaskDiscoveryFilter::Active).expect("rediscover children");
            assert_eq!(
                observed
                    .runs
                    .iter()
                    .find(|run| run.run_id == "run-child-a")
                    .expect("first child")
                    .liveness,
                Some(AgentTaskLiveness::Stale)
            );
            assert_eq!(
                observed
                    .runs
                    .iter()
                    .find(|run| run.run_id == "run-child-a")
                    .expect("first child")
                    .stale_reason
                    .as_deref(),
                Some("owner_process_not_running")
            );
            assert_eq!(
                observed
                    .runs
                    .iter()
                    .find(|run| run.run_id == "run-child-b")
                    .expect("second child")
                    .liveness,
                Some(AgentTaskLiveness::Active)
            );
        })();

        let _ = child_b.kill();
        let _ = child_b.wait();
        result
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
            AgentTaskDiscoveryOptions {
                limit: Some(2),
                ..Default::default()
            },
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
fn discovery_rejects_zero_limit_to_preserve_pagination_progress() {
    with_isolated_home(|_| {
        let error = discover_runs_with_options(
            AgentTaskDiscoveryFilter::Active,
            AgentTaskDiscoveryOptions {
                limit: Some(0),
                ..Default::default()
            },
        )
        .expect_err("zero limit is invalid");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("greater than zero"));
    });
}

#[test]
fn discovery_998_record_history_and_active_pages_do_not_repeat_or_lose_records() {
    with_isolated_home(|_| {
        for index in 0..998 {
            agent_task_lifecycle::submit_plan(
                &discovery_plan(),
                Some(&format!("run-page-{index:04}")),
            )
            .expect("submitted");
        }

        let first = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions {
                limit: Some(20),
                state: Some("queued".to_string()),
                ..Default::default()
            },
        )
        .expect("first page");
        let second = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions {
                limit: Some(20),
                cursor: first.next_cursor.expect("continuation"),
                state: Some("queued".to_string()),
                ..Default::default()
            },
        )
        .expect("second page");
        let exhaustive = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions {
                state: Some("queued".to_string()),
                ..Default::default()
            },
        )
        .expect("full history");
        let active_first = discover_runs_with_options(
            AgentTaskDiscoveryFilter::Active,
            AgentTaskDiscoveryOptions {
                limit: Some(20),
                ..Default::default()
            },
        )
        .expect("first active page");
        let active_second = discover_runs_with_options(
            AgentTaskDiscoveryFilter::Active,
            AgentTaskDiscoveryOptions {
                limit: Some(20),
                cursor: active_first.next_cursor.expect("active continuation"),
                ..Default::default()
            },
        )
        .expect("second active page");

        assert_eq!(first.total, 998);
        assert_eq!(first.count, 20);
        assert_eq!(first.next_cursor, Some(20));
        assert!(first.truncated);
        assert_eq!(second.total, 998);
        assert_eq!(second.count, 20);
        assert_ne!(first.runs[0].run_id, second.runs[0].run_id);
        assert_eq!(exhaustive.count, 998);
        assert!(!exhaustive.truncated);
        assert_eq!(exhaustive.next_cursor, None);
        assert_eq!(active_first.total, 998);
        assert_eq!(active_first.next_cursor, Some(20));
        assert_eq!(active_second.count, 20);
        assert_ne!(active_first.runs[0].run_id, active_second.runs[0].run_id);
    });
}

#[test]
fn discovery_filters_by_cook_identity_and_classifies_only_live_queued_records_as_active() {
    with_isolated_home(|_| {
        let mut matching = discovery_plan();
        matching.group_key = Some("matching-repo".to_string());
        matching.tasks[0].workspace.root = Some("/work/matching".to_string());
        matching.tasks[0].workspace.task_url =
            Some("https://example.test/issues/11086".to_string());
        matching.tasks[0].parent_plan_id = Some("batch-11086".to_string());
        agent_task_lifecycle::submit_plan(&matching, Some("run-matching")).expect("matching run");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-other")).expect("other run");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-queued-ghost"))
            .expect("ghost run");
        agent_task_lifecycle::rewrite_record_for_test("run-queued-ghost", |record| {
            record.tasks[0].state = AgentTaskState::Succeeded;
            record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("age ghost");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-no-update-ghost"))
            .expect("no-update ghost run");
        agent_task_lifecycle::rewrite_record_for_test("run-no-update-ghost", |record| {
            record.tasks[0].state = AgentTaskState::Succeeded;
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
            record.updated_at = None;
        })
        .expect("remove ghost heartbeat");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-ancient-queued-task"))
            .expect("ancient queued task run");
        agent_task_lifecycle::rewrite_record_for_test("run-ancient-queued-task", |record| {
            // Keep the serialized queued task to prove it alone is not ownership.
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
            record.updated_at = None;
        })
        .expect("age queued task without ownership");
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-fresh-queued-task"))
            .expect("fresh queued task run");

        let filtered = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions {
                repo: Some("matching-repo".to_string()),
                workspace: Some("/work/matching".to_string()),
                task_url: Some("https://example.test/issues/11086".to_string()),
                parent_id: Some("batch-11086".to_string()),
                ..Default::default()
            },
        )
        .expect("filtered discovery");
        let active = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active discovery");

        assert_eq!(filtered.runs.len(), 1);
        assert_eq!(filtered.runs[0].run_id, "run-matching");
        let ghost = active
            .runs
            .iter()
            .find(|run| run.run_id == "run-queued-ghost")
            .expect("stale ghost remains reconcilable");
        assert_eq!(ghost.liveness, Some(AgentTaskLiveness::Stale));
        let no_update_ghost = active
            .runs
            .iter()
            .find(|run| run.run_id == "run-no-update-ghost")
            .expect("no-update ghost remains reconcilable");
        assert_eq!(no_update_ghost.liveness, Some(AgentTaskLiveness::Stale));
        let ancient_queued_task = active
            .runs
            .iter()
            .find(|run| run.run_id == "run-ancient-queued-task")
            .expect("ancient queued task remains reconcilable");
        assert_eq!(ancient_queued_task.counts.queued, 1);
        assert_eq!(ancient_queued_task.liveness, Some(AgentTaskLiveness::Stale));
        let fresh_queued_task = active
            .runs
            .iter()
            .find(|run| run.run_id == "run-fresh-queued-task")
            .expect("ownerless queued task remains reconcilable");
        assert_eq!(fresh_queued_task.liveness, Some(AgentTaskLiveness::Stale));
        assert_eq!(active.liveness_summary.expect("summary").stale, 6);

        let applied = reconcile_run("run-fresh-queued-task", false)
            .expect("ownerless queued task reconciles");
        assert_eq!(applied.reconciled, 1);
        assert_eq!(applied.runs[0].action, "reconciled");
        assert_eq!(
            lifecycle_status("run-fresh-queued-task")
                .expect("reconciled queued task")
                .state,
            AgentTaskRunState::Cancelled
        );
    });
}

#[test]
fn upgrade_admission_ignores_terminal_records_with_stale_owner_metadata() {
    with_isolated_home(|_| {
        for (run_id, state) in [
            ("terminal-succeeded", AgentTaskRunState::Succeeded),
            ("terminal-failed", AgentTaskRunState::Failed),
        ] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                agent_task_lifecycle::set_run_state(record, state);
                record.metadata = serde_json::json!({ "runner_pid": u32::MAX });
            })
            .expect("terminal record stored");
        }

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert!(admission.allows_controller_replacement());
        assert!(admission.blockers.is_empty());
    });
}

#[test]
fn controller_upgrade_admission_uses_liveness_and_bounded_record_health() {
    with_isolated_home(|_| {
        for run_id in ["stale-local", "live-owner", "unverified-runner"] {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
        }
        agent_task_lifecycle::rewrite_record_for_test("stale-local", |record| {
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
            record.updated_at = None;
        })
        .expect("age local record");
        agent_task_lifecycle::mark_running("live-owner").expect("running");
        agent_task_lifecycle::rewrite_record_for_test("live-owner", |record| {
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
        })
        .expect("record live owner");
        agent_task_lifecycle::rewrite_record_for_test("unverified-runner", |record| {
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
            record.updated_at = None;
            record.metadata["runner_id"] = serde_json::json!("lab");
            record.metadata["runner_job_id"] = serde_json::json!("old-job");
        })
        .expect("record unverified runner");

        let store = homeboy_core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        for index in 0..crate::agent_task_lifecycle::HEALTH_SAMPLE_LIMIT * 3 {
            store
                .upsert_imported_run(&homeboy_core::observation::RunRecord {
                    id: format!("malformed-upgrade-{index}"),
                    kind: "agent-task".to_string(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    status: "running".to_string(),
                    metadata_json: serde_json::json!({}),
                    ..Default::default()
                })
                .expect("insert malformed record");
        }

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("read");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());

        assert_eq!(admission.stale, 2);
        assert_eq!(admission.active, 1);
        assert_eq!(admission.blockers.len(), 2);
        assert!(admission.blockers.iter().any(|blocker| {
            blocker.run_id == "live-owner"
                && blocker.recovery_command
                    == "homeboy --placement local agent-task status live-owner"
        }));
        let runner = admission
            .blockers
            .iter()
            .find(|blocker| blocker.run_id == "unverified-runner")
            .expect("unverified runner blocks");
        assert_eq!(runner.reason, "runner_job_unverified_after_daemon_restart");
        assert_eq!(runner.recovery_command, "homeboy runner reconcile lab");
        assert!(!admission
            .blockers
            .iter()
            .any(|blocker| blocker.run_id == "stale-local"));
        assert_eq!(admission.record_health["malformed"], 60);
        assert!(
            admission.record_health["samples"].as_array().unwrap().len()
                <= crate::agent_task_lifecycle::HEALTH_SAMPLE_LIMIT
        );
    });
}

#[test]
fn upgrade_admission_keeps_a_live_provider_owner_active_despite_heartbeat_lag() {
    with_isolated_home(|_| {
        let run_id = "lagging-heartbeat-advancing-provider";
        let plan = discovery_plan();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
        agent_task_lifecycle::mark_running(run_id).expect("running");
        agent_task_lifecycle::reserve_provider_execution_in_store(
            &test_lifecycle_store(),
            run_id,
            &plan.tasks[0],
            1,
        )
        .expect("provider reserved");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            // Simulate a lagging lifecycle heartbeat after the provider owner
            // refreshed its durable execution ownership.
            record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
            record.metadata[agent_task_lifecycle::METADATA_KEY_STALE_RUNNING] =
                serde_json::json!(true);
        })
        .expect("lagging heartbeat fixture");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());

        assert_eq!(admission.active, 1);
        assert_eq!(admission.stale, 0);
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].liveness, "active");
        assert_eq!(
            admission.blockers[0].recovery_command,
            format!("homeboy --placement local agent-task status {run_id}")
        );
        assert!(!admission.blockers[0].recovery_command.contains("cancel"));
    });
}

#[test]
fn upgrade_admission_keeps_ambiguous_provider_evidence_fail_closed_and_read_only() {
    with_isolated_home(|_| {
        let run_id = "ambiguous-provider-owner";
        let plan = discovery_plan();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
        agent_task_lifecycle::mark_running(run_id).expect("running");
        agent_task_lifecycle::reserve_provider_execution_in_store(
            &test_lifecycle_store(),
            run_id,
            &plan.tasks[0],
            1,
        )
        .expect("provider reserved");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
            // Zero is not a process identity and must not be treated as a dead
            // owner merely because the heartbeat is old.
            record.metadata["provider_executions"][0]["owner_pid"] = serde_json::json!(0);
            record.metadata[agent_task_lifecycle::METADATA_KEY_STALE_RUNNING] =
                serde_json::json!(true);
        })
        .expect("ambiguous owner fixture");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());

        assert_eq!(admission.unreconciled, 1);
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].liveness, "unreconciled");
        assert_eq!(
            admission.blockers[0].recovery_command,
            format!("homeboy --placement local agent-task reconcile {run_id} --dry-run")
        );
        assert!(!admission.blockers[0].recovery_command.contains("cancel"));
    });
}

#[cfg(unix)]
#[test]
fn upgrade_admission_classifies_a_proven_dead_provider_owner_as_stale() {
    with_isolated_home(|_| {
        let run_id = "dead-provider-owner";
        let plan = discovery_plan();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
        agent_task_lifecycle::mark_running(run_id).expect("running");
        agent_task_lifecycle::reserve_provider_execution_in_store(
            &test_lifecycle_store(),
            run_id,
            &plan.tasks[0],
            1,
        )
        .expect("provider reserved");
        let mut owner = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("owner process");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
            record.metadata["provider_executions"][0]["owner_pid"] = serde_json::json!(owner.id());
            // The process is reaped below, so no start-time guess is needed to
            // prove it dead. Live identities remain checked by the lifecycle.
            record.metadata["provider_executions"][0]["owner_linux_starttime_ticks"] =
                serde_json::Value::Null;
        })
        .expect("dead owner fixture");
        owner.kill().expect("stop owner");
        owner.wait().expect("reap owner");

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());

        assert_eq!(admission.stale, 1);
        assert!(admission.blockers.is_empty());
        assert!(admission.allows_controller_replacement());
    });
}

#[test]
fn fixture_runner_records_are_quarantined_without_hiding_unknown_runner_ownership() {
    with_isolated_home(|_| {
        let fixture_run_id = "concurrent-first-cook-run";
        let mut fixture_plan = discovery_plan();
        fixture_plan.tasks[0].executor.backend = "fixture".to_string();
        agent_task_lifecycle::submit_plan(&fixture_plan, Some(fixture_run_id))
            .expect("persist leaked concurrent Cook fixture");
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: fixture_run_id,
            runner_id: "fixture-lab",
            runner_job_id: "accepted-daemon-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("attach fixture runner record");

        let unknown_run_id = "unknown-runner-control";
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(unknown_run_id))
            .expect("persist unknown runner control");
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: unknown_run_id,
            runner_id: "offline-unknown-runner",
            runner_job_id: "unknown-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("attach unknown runner control");

        for run_id in [fixture_run_id, unknown_run_id] {
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
                record.updated_at = None;
            })
            .expect("age runner record");
        }

        let discovery = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active discovery");
        assert_eq!(discovery.total, 1);
        assert_eq!(discovery.runs[0].run_id, unknown_run_id);

        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].run_id, unknown_run_id);
        assert_eq!(
            admission.blockers[0].recovery_command,
            "homeboy runner reconcile offline-unknown-runner"
        );

        let preview =
            agent_task_lifecycle::reconcile_record_health_in_store(&test_lifecycle_store(), true)
                .expect("preview repair");
        assert_eq!(preview.considered, 1);
        assert_eq!(preview.quarantined, 0);
        assert_eq!(preview.records[0].run_id, fixture_run_id);
        assert_eq!(
            preview.records[0].reason,
            agent_task_lifecycle::AgentTaskRecordHealthReason::FixtureRunnerProvenance
        );
        assert_eq!(preview.records[0].action, "would-quarantine");

        let repaired =
            agent_task_lifecycle::reconcile_record_health_in_store(&test_lifecycle_store(), false)
                .expect("apply repair");
        assert_eq!(repaired.quarantined, 1);
        let health = agent_task_lifecycle::record_health_summary_in_store(&test_lifecycle_store())
            .expect("quarantine health");
        assert_eq!(health.fixture, 1);
        assert_eq!(health.quarantined, 1);
    });
}

#[test]
fn verified_target_bootstrap_recovers_only_fixture_residue_and_keeps_live_or_unknown_owners_blocked(
) {
    with_isolated_home(|_| {
        let fixture_run_id = "concurrent-first-cook-run";
        let mut fixture_plan = discovery_plan();
        fixture_plan.tasks[0].executor.backend = "fixture".to_string();
        agent_task_lifecycle::submit_plan(&fixture_plan, Some(fixture_run_id))
            .expect("persist previous-release fixture residue");
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: fixture_run_id,
            runner_id: "fixture-lab",
            runner_job_id: "accepted-daemon-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("attach fixture runner record");

        let live_run_id = "live-owner-control";
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(live_run_id))
            .expect("persist live control");
        agent_task_lifecycle::mark_running(live_run_id).expect("mark live");
        agent_task_lifecycle::rewrite_record_for_test(live_run_id, |record| {
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
        })
        .expect("record live owner");

        let unknown_run_id = "unverifiable-owner-control";
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some(unknown_run_id))
            .expect("persist unknown control");
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id: unknown_run_id,
            runner_id: "unknown-runner",
            runner_job_id: "unknown-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .expect("attach unknown runner record");
        for run_id in [fixture_run_id, unknown_run_id] {
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
                record.updated_at = None;
            })
            .expect("age previous-release ownership");
        }

        // The target-only mutation has a narrow proof: fixture executor plan +
        // accepted runner handoff. It cannot touch either control record.
        assert_eq!(
            agent_task_lifecycle::quarantine_verified_fixture_runner_records()
                .expect("target recovery"),
            1
        );
        let (records, health) = agent_task_lifecycle::read_records_with_health().expect("records");
        let admission =
            controller_upgrade_admission_for_records(&records, health, chrono::Utc::now());
        assert_eq!(admission.blockers.len(), 2);
        assert!(admission
            .blockers
            .iter()
            .any(|blocker| blocker.run_id == live_run_id));
        assert!(admission.blockers.iter().any(|blocker| {
            blocker.run_id == unknown_run_id
                && blocker.recovery_command == "homeboy runner reconcile unknown-runner"
        }));
        assert_eq!(
            agent_task_lifecycle::record_health_summary_in_store(&test_lifecycle_store(),)
                .expect("health")
                .quarantined,
            1
        );
    });
}

#[test]
fn discovery_rejects_invalid_submitted_after_timestamps() {
    with_isolated_home(|_| {
        agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-invalid-timestamp-filter"))
            .expect("submitted");

        let error = discover_runs_with_options(
            AgentTaskDiscoveryFilter::All,
            AgentTaskDiscoveryOptions {
                submitted_after: Some("yesterday".to_string()),
                ..Default::default()
            },
        )
        .expect_err("invalid timestamp is rejected");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("RFC3339"));
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
            AgentTaskDiscoveryOptions {
                limit: Some(5),
                ..Default::default()
            },
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
            "homeboy runner exec homeboy-lab -- homeboy agent-task status run-runner-commands"
        );
        assert_eq!(
            run.commands.logs,
            "homeboy runner exec homeboy-lab -- homeboy agent-task logs run-runner-commands"
        );
        assert_eq!(
            run.commands.review,
            "homeboy runner exec homeboy-lab -- homeboy agent-task review run-runner-commands"
        );
        assert_eq!(
            run.commands.reconcile,
            "homeboy runner exec homeboy-lab -- homeboy agent-task reconcile run-runner-commands --dry-run"
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
            "homeboy --placement local agent-task status controller-handoff-reconnect"
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
            "homeboy --placement local agent-task status controller-handoff-reconnect"
        );
        assert_eq!(
            run.commands.logs,
            "homeboy --placement local agent-task logs controller-handoff-reconnect"
        );
        assert!(agent_task_lifecycle::reconcile_status(&run.run_id).is_ok());
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
        let submission_request: homeboy_core::api_jobs::RemoteRunnerJobRequest =
            serde_json::from_value(serde_json::json!({
                "runner_id": "homeboy-lab",
                "command": command,
                "metadata": { "submission_key": "controller-handoff-unaccepted" }
            }))
            .expect("fixture runner submission request");
        agent_task_lifecycle::record_lab_offload_submission_request(
            "controller-handoff-unaccepted",
            &submission_request,
        )
        .expect("persist complete pending handoff request");
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
            "homeboy --placement local agent-task status controller-handoff-unaccepted"
        );

        let _runner = agent_task_lifecycle::RunnerContinuationTestGuard::install(Box::new(
            RunnerAuthorityFixture::configured_disconnected(),
        ));
        let terminal = lifecycle_status("controller-handoff-unaccepted")
            .expect("terminal controller record after confirmed absence");
        assert_eq!(terminal.state, AgentTaskRunState::Cancelled);
        assert_eq!(terminal.metadata["provider_executions_consumed"], 0);
        assert_eq!(terminal.metadata["retryable"], true);
    });
}

fn record_stale_accepted_lab_handoff(run_id: &str, runner_id: &str) {
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id)).expect("submitted");
    agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
        run_id,
        runner_id,
        runner_job_id: "accepted-daemon-job",
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
    })
    .expect("accepted Lab handoff");
    agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
        record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
        record.updated_at = Some("2000-01-01T00:00:00+00:00".to_string());
        record
            .metadata
            .as_object_mut()
            .expect("record metadata")
            .remove("detached_cook_handoff");
    })
    .expect("age accepted handoff");
}

struct RunnerAuthorityFixture {
    authority: agent_task_lifecycle::RunnerAuthority,
    connected: bool,
    live_job_authority: agent_task_lifecycle::RunnerLiveJobAuthority,
}

impl RunnerAuthorityFixture {
    fn configured_disconnected() -> Self {
        Self {
            authority: agent_task_lifecycle::RunnerAuthority::Configured,
            connected: false,
            live_job_authority: agent_task_lifecycle::RunnerLiveJobAuthority::Unknown,
        }
    }

    fn configured_idle() -> Self {
        Self {
            authority: agent_task_lifecycle::RunnerAuthority::Configured,
            connected: true,
            live_job_authority: agent_task_lifecycle::RunnerLiveJobAuthority::Idle,
        }
    }

    fn removed() -> Self {
        Self {
            authority: agent_task_lifecycle::RunnerAuthority::Removed,
            connected: false,
            live_job_authority: agent_task_lifecycle::RunnerLiveJobAuthority::Unknown,
        }
    }

    fn unknown() -> Self {
        Self {
            authority: agent_task_lifecycle::RunnerAuthority::Unknown,
            connected: false,
            live_job_authority: agent_task_lifecycle::RunnerLiveJobAuthority::Unknown,
        }
    }
}

impl agent_task_lifecycle::RunnerContinuationProvider for RunnerAuthorityFixture {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> homeboy_core::Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        Err(homeboy_core::Error::internal_unexpected(
            "unused in fixture",
        ))
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        self.connected
    }

    fn runner_authority(&self, _runner_id: &str) -> agent_task_lifecycle::RunnerAuthority {
        self.authority
    }

    fn runner_live_job_authority(
        &self,
        _runner_id: &str,
    ) -> agent_task_lifecycle::RunnerLiveJobAuthority {
        self.live_job_authority
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> homeboy_core::Result<i32> {
        Err(homeboy_core::Error::internal_unexpected(
            "unused in fixture",
        ))
    }

    fn submit_runner_api_request(
        &self,
        _runner_id: &str,
        _submission: crate::agent_task_lifecycle::RunnerContinuationSubmission,
    ) -> homeboy_core::Result<homeboy_core::api_jobs::Job> {
        Err(homeboy_core::Error::internal_unexpected(
            "unused in fixture",
        ))
    }

    fn lookup_reverse_broker_submission(
        &self,
        _runner_id: &str,
        _submission_key: &str,
    ) -> homeboy_core::Result<homeboy_core::api_jobs::RemoteRunnerSubmissionLookup> {
        Ok(homeboy_core::api_jobs::RemoteRunnerSubmissionLookup::Absent)
    }
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
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            ..Default::default()
        }
    }
}

struct LocalBoundaryExecutor {
    run_id: String,
    observed: Arc<Mutex<Option<Value>>>,
}

impl AgentTaskExecutorAdapter for LocalBoundaryExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let record = lifecycle_status(&self.run_id).expect("local provider record");
        *self.observed.lock().expect("provider boundary") =
            Some(record.metadata["provider_executions"][0]["state"].clone());
        SucceedingExecutor.execute(request, context)
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
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some("provider exceeded timeout_ms=50".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
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
            ..Default::default()
        }
    }
}

struct TimeoutAfterWritingPatchExecutor;

impl AgentTaskExecutorAdapter for TimeoutAfterWritingPatchExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let workspace = request.workspace.root.expect("attempt workspace");
        std::fs::write(
            Path::new(&workspace).join("timeout-candidate.txt"),
            "recovered after timeout\n",
        )
        .expect("write candidate");
        std::thread::sleep(std::time::Duration::from_millis(25));
        AgentTaskOutcome {
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("provider completed after its deadline".to_string()),
            ..Default::default()
        }
    }
}

struct CapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

#[derive(Clone)]
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
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            ..Default::default()
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
            task_id: request.task_id,
            status: if call == 0 {
                AgentTaskOutcomeStatus::ProviderError
            } else {
                AgentTaskOutcomeStatus::Succeeded
            },
            failure_classification: (call == 0).then_some(AgentTaskFailureClassification::Provider),
            ..Default::default()
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
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            ..Default::default()
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

fn aggregate_with_outcome(metadata: Value) -> AgentTaskAggregate {
    AgentTaskAggregate {
        schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: "service-plan".to_string(),
        status: AgentTaskAggregateStatus::Succeeded,
        totals: AgentTaskAggregateTotals {
            succeeded: 1,
            ..Default::default()
        },
        outcomes: vec![AgentTaskOutcome {
            task_id: "service-task".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            metadata,
            ..Default::default()
        }],
        events: Vec::new(),
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    }
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

fn index_cook_attempt(cook_id: &str, run_id: &str) {
    index_cook_attempts(cook_id, run_id, run_id);
}

fn index_cook_attempts(cook_id: &str, first_run_id: &str, latest_run_id: &str) {
    agent_task_lifecycle::replace_cook_index_for_test(&agent_task_lifecycle::AgentTaskCookIndex {
        schema: "homeboy/agent-task-cook-index/v1".to_string(),
        cook_id: cook_id.to_string(),
        latest_run_id: latest_run_id.to_string(),
        latest_substantive_candidate: None,
        cancellation_fence: None,
        attempts: [first_run_id, latest_run_id]
            .into_iter()
            .enumerate()
            .map(
                |(index, run_id)| agent_task_lifecycle::AgentTaskCookIndexAttempt {
                    attempt: (index + 1) as u32,
                    run_id: run_id.to_string(),
                    recorded_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .collect(),
    })
    .expect("index Cook attempt");
}
