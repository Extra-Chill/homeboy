//! Tests for the cook orchestration service (`super::cook`). Split from the
//! cook god file via #[path]; logically remains `cook::tests` so `super::`
//! paths are unchanged.

use super::super::cook_adoption::{
    adopt_cook_candidate, adopt_cook_candidate_with_dispatcher_and_backend,
    candidate_adoption_source, concrete_adoption_ai_model, resolve_adoption_target,
    resolve_adoption_target_with_attempt,
};
use super::super::cook_baseline::git_output;
use super::super::cook_promotion::{
    cook_report, finalize_cook_pr_with_backend, finalize_or_load_cook_pr_with_backend,
    moving_base_recovery_for_run, moving_base_recovery_from_promotion, moving_base_recovery_report,
    next_moving_base_recovery, persisted_promotion_for_attempt, recover_cook_pr_with_backend,
    recover_moving_base_cook_candidate, refreshed_moving_base_recovery, selected_candidate_task_id,
    MovingBaseCookRecovery,
};
use super::super::cook_recipe::persist_initial_recipe;
use super::*;
use crate::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
};
use crate::agent_task_finalization::{
    AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
    AgentTaskPublicationBinding, AgentTaskPublicationGitTracking,
    RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_scheduler::AgentTaskState;
use homeboy_core::run_lifecycle_record::{
    ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
    RunLifecycleRecord,
};
use sha2::Digest;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar};

/// Seed a terminal aggregate whose last outcome carries a valid AI-authored
/// review form under `outputs["review_form"]`, so cook finalization (which now
/// sources reviewer prose from the form) can proceed.
/// A valid AI-authored review form, for tests whose cook flow reaches
/// finalization (which now sources reviewer prose from the form).
fn test_review_form() -> crate::agent_task_review_dossier::AiFilledReviewForm {
    crate::agent_task_review_dossier::AiFilledReviewForm {
        summary: "Close the issue by guarding the reload path.".to_string(),
        what_changed: vec!["Add a null guard in the render path.".to_string()],
        compatibility: "Internal-only change; no compatibility impact.".to_string(),
        used_for: "Reproduced the failure, isolated the reload path, added a guard, and verified with the recorded deterministic gate before finalizing.".to_string(),
    }
}

/// The `outputs` object carrying a valid review form under `review_form`.
fn test_review_form_outputs() -> Value {
    serde_json::json!({ "review_form": test_review_form() })
}

fn seed_review_form_aggregate(run_id: &str, plan: &AgentTaskPlan) {
    use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };
    let form = test_review_form();
    let task = plan.tasks.first().expect("review form plan has one task");
    agent_task_lifecycle::record_run_aggregate(
        run_id,
        plan,
        &AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: task.task_id.clone(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("provider dispatched once".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: serde_json::json!({ "review_form": form }),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({ "model": task.executor.model() }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        },
    )
    .unwrap();
}

fn seed_missing_review_form_aggregate(run_id: &str, plan: &AgentTaskPlan) {
    use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    let task = plan.tasks.first().expect("candidate plan has one task");
    agent_task_lifecycle::record_run_aggregate(
        run_id,
        plan,
        &AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: task.task_id.clone(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("candidate prepared without review form".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({ "model": task.executor.model() }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        },
    )
    .unwrap();
}

fn seed_substantive_candidate_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    patch_path: &std::path::Path,
    patch: &str,
) {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    std::fs::write(patch_path, patch).expect("write candidate patch");
    let task = plan.tasks.first().expect("candidate plan has one task");
    agent_task_lifecycle::record_run_aggregate(
        run_id,
        plan,
        &AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: task.task_id.clone(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: None,
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    id: "candidate".to_string(),
                    kind: "patch".to_string(),
                    path: Some(patch_path.display().to_string()),
                    size_bytes: Some(patch.len() as u64),
                    sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                        patch.as_bytes(),
                    )),
                    ..Default::default()
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        },
    )
    .expect("persist candidate aggregate");
}

#[test]
fn candidate_selection_uses_the_winner_for_review_form_and_status_projection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut plan = AgentTaskPlan::new(
            "selected-candidate",
            vec![
                AgentTaskRequest {
                    task_id: "winner".to_string(),
                    group_key: Some("candidate-group".to_string()),
                    ..batch_cook_options(
                        "selected-candidate-template",
                        Arc::new(AcceptedDetachedAttemptDispatcher),
                    )
                    .initial_plan
                    .tasks[0]
                        .clone()
                },
                AgentTaskRequest {
                    task_id: "late-sibling".to_string(),
                    group_key: Some("candidate-group".to_string()),
                    ..batch_cook_options(
                        "selected-candidate-template-two",
                        Arc::new(AcceptedDetachedAttemptDispatcher),
                    )
                    .initial_plan
                    .tasks[0]
                        .clone()
                },
            ],
        );
        plan.group_key = Some("candidate-group".to_string());
        plan.options.candidate_completion =
            crate::agent_task_scheduler::AgentTaskCandidateCompletionPolicy::FirstGreen;
        let run_id = "selected-candidate-run";
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &plan,
            &crate::agent_task_scheduler::AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: crate::agent_task_scheduler::AgentTaskAggregateStatus::PartialRecoverable,
                totals: crate::agent_task_scheduler::AgentTaskAggregateTotals {
                    succeeded: 1,
                    cancelled: 1,
                    ..Default::default()
                },
                outcomes: vec![
                    crate::agent_task::AgentTaskOutcome {
                        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                        task_id: "winner".to_string(),
                        status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                        summary: None,
                        failure_classification: None,
                        artifacts: Vec::new(),
                        typed_artifacts: Vec::new(),
                        evidence_refs: Vec::new(),
                        diagnostics: Vec::new(),
                        outputs: test_review_form_outputs(),
                        workflow: None,
                        follow_up: None,
                        metadata: serde_json::json!({ "candidate_selection": {
                            "policy": "first_green",
                            "selected_task_id": "winner",
                            "promotion_action": "promote_selected_candidate_only"
                        }}),
                    },
                    crate::agent_task::AgentTaskOutcome {
                        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                        task_id: "late-sibling".to_string(),
                        status: crate::agent_task::AgentTaskOutcomeStatus::Cancelled,
                        summary: None,
                        failure_classification: None,
                        artifacts: Vec::new(),
                        typed_artifacts: Vec::new(),
                        evidence_refs: Vec::new(),
                        diagnostics: Vec::new(),
                        outputs: Value::Null,
                        workflow: None,
                        follow_up: None,
                        metadata: Value::Null,
                    },
                ],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();

        assert_eq!(
            review_form_from_aggregate(&agent_task_lifecycle::read_aggregate(run_id).unwrap())
                .unwrap(),
            Some(test_review_form())
        );
        assert_eq!(
            selected_candidate_task_id(run_id).unwrap(),
            Some("winner".to_string())
        );
        let status = agent_task_lifecycle::run_status(run_id, None).unwrap();
        let candidate = status
            .candidate
            .as_ref()
            .expect("candidate status projection");
        assert_eq!(candidate.policy, plan.options.candidate_completion);
        assert_eq!(candidate.selected_task_id.as_deref(), Some("winner"));
        assert_eq!(candidate.candidates.len(), 2);
        assert_eq!(
            candidate.cancellation_supervision,
            "scheduler_deferred_cleanup"
        );
        assert_eq!(
            candidate.promotion_action.as_deref(),
            Some("promote_selected_candidate_only")
        );
        let serialized = serde_json::to_value(&status).unwrap();
        assert_eq!(serialized["candidate"]["policy"], "first_green");
        assert_eq!(serialized["candidate"]["deadline_timeout_ms"], Value::Null);
        let mut legacy_json = serialized;
        legacy_json
            .as_object_mut()
            .expect("status object")
            .remove("candidate");
        let legacy: crate::agent_task_lifecycle::AgentTaskRunStatus =
            serde_json::from_value(legacy_json).unwrap();
        assert!(legacy.candidate.is_none());
    });
}

#[test]
fn candidate_group_preflight_rejects_ambiguous_plan_before_execution() {
    let mut plan = batch_cook_options(
        "ambiguous-candidates",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    )
    .initial_plan;
    let mut sibling = plan.tasks[0].clone();
    sibling.task_id = "sibling".to_string();
    plan.tasks.push(sibling);
    plan.options.candidate_completion =
        crate::agent_task_scheduler::AgentTaskCandidateCompletionPolicy::FirstGreen;

    let error = validate_cook_candidate_group(&plan).expect_err("shared group is required");

    assert_eq!(error.details["field"], "group_key");
    assert!(error.message.contains("explicit shared group"));
}

#[test]
fn pre_artifact_interruption_classifies_provider_ledger_without_phantom_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "cook-pre-artifact-phases",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        let run_id = options.initial_run_id.clone();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
        agent_task_lifecycle::cancel_run(&run_id, Some("controller interrupted")).unwrap();

        let before = agent_task_lifecycle::status(&run_id).unwrap();
        assert!(before.aggregate_path.is_none());
        assert_eq!(
            pre_artifact_interruption_phase(&before),
            PreArtifactInterruptionPhase::BeforeProviderStart
        );
        agent_task_lifecycle::rewrite_record_for_test(&run_id, |record| {
            record.metadata["provider_executions"] = serde_json::json!([{
                "key": "provider:1", "state": "running"
            }]);
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        assert_eq!(
            pre_artifact_interruption_phase(&agent_task_lifecycle::status(&run_id).unwrap()),
            PreArtifactInterruptionPhase::DuringProviderExecution
        );
        agent_task_lifecycle::rewrite_record_for_test(&run_id, |record| {
            record.metadata["provider_executions"][0]["state"] = serde_json::json!("failed");
            record.metadata["provider_executions"][0]["finished_at"] =
                serde_json::json!("2026-07-24T23:42:06Z");
        })
        .unwrap();
        assert_eq!(
            pre_artifact_interruption_phase(&agent_task_lifecycle::status(&run_id).unwrap()),
            PreArtifactInterruptionPhase::AfterProviderReturn
        );
        assert!(agent_task_lifecycle::read_aggregate(&run_id).is_err());
    });
}

#[test]
fn pre_artifact_interruption_does_not_bypass_an_authoritative_aggregate_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "cook-pre-artifact-aggregate-authority",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        let run_id = options.initial_run_id.clone();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
        agent_task_lifecycle::cancel_run(&run_id, Some("controller interrupted")).unwrap();
        agent_task_lifecycle::rewrite_record_for_test(&run_id, |record| {
            record.aggregate_path = Some("authoritative/aggregate.json".to_string());
        })
        .unwrap();

        let record = agent_task_lifecycle::status(&run_id).unwrap();
        assert!(record.state.is_terminal());
        assert!(record.aggregate_path.is_some());
        assert!(agent_task_lifecycle::read_aggregate(&run_id).is_err());
    });
}

#[test]
fn pre_artifact_interruption_claim_is_restart_and_concurrent_controller_idempotent() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut options = batch_cook_options(
            "cook-pre-artifact-claim",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.max_attempts = 2;
        let run_id = options.initial_run_id.clone();
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
        agent_task_lifecycle::cancel_run(&run_id, Some("controller interrupted")).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let mut controllers = Vec::new();
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            let cook_id = options.cook_id.clone();
            let run_id = run_id.clone();
            let plan = options.initial_plan.clone();
            controllers.push(std::thread::spawn(move || {
                barrier.wait();
                claim_pre_artifact_interruption_retry(&cook_id, 1, &run_id, &plan)
            }));
        }
        let results = controllers
            .into_iter()
            .map(|controller| controller.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        // A concurrent observer may see the owner's fresh lease before it has
        // appended the immutable recipe entry. A restart converges through the
        // completed claim without creating a second attempt.
        let resumed = claim_pre_artifact_interruption_retry(
            &options.cook_id,
            1,
            &run_id,
            &options.initial_plan,
        )
        .unwrap()
        .unwrap();
        assert!(results.iter().flatten().all(|result| result == &resumed));
        assert_eq!(resumed.0, 2);
        let recipe = super::super::load_recipe(&options.cook_id).unwrap();
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].run_id, resumed.1);
        assert!(agent_task_lifecycle::read_aggregate(&run_id).is_err());
    });
}

#[test]
fn cook_service_retry_uses_the_same_passed_context_after_ambient_mutation() {
    let _env_lock = homeboy_core::test_support::env_lock();
    let prior = std::env::var_os(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV);
    let context = crate::agent_task_scheduler::HarvestExecutionContext::default();
    let first_attempt = cook_attempt_harvest_context(&context);
    std::env::set_var(
        homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
        "ambient state must not affect a passed cook context",
    );
    let retry_attempt = cook_attempt_harvest_context(&context);
    match prior {
        Some(value) => std::env::set_var(
            homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            value,
        ),
        None => std::env::remove_var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV),
    }

    assert_eq!(format!("{first_attempt:?}"), format!("{retry_attempt:?}"));
    assert_eq!(
        format!("{retry_attempt:?}"),
        "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"
    );
}

#[test]
fn fresh_cook_review_form_has_bounded_budget_independent_of_code_execution() {
    let mut follow_up_request = batch_cook_options(
        "fresh-review-budget",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    )
    .initial_plan
    .tasks
    .remove(0);
    follow_up_request.inputs["cook_loop"]["review_form_required"] = serde_json::json!(true);
    let mut source_request = follow_up_request.clone();
    source_request.inputs = Value::Null;
    let scope = follow_up_budget_scope(&source_request, &follow_up_request);
    assert_eq!(scope, CookFollowUpBudgetScope::FreshCookReview);

    let code_budget = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 0);
    let consumed_code_budget = ExecutionBudgetUsage {
        executions: 1,
        ..Default::default()
    };
    assert!(budget_remaining(&code_budget, consumed_code_budget).is_none());

    let (review_budget, review_usage) =
        scoped_follow_up_budget(scope, &code_budget, consumed_code_budget);
    assert_eq!(
        review_budget,
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(2, 1, 0)
    );
    assert_eq!(review_usage.executions, 0);
    assert_eq!(
        budget_remaining(&review_budget, review_usage),
        Some(crate::agent_task_scheduler::AgentTaskExecutionBudget::new(
            2, 1, 0
        ))
    );

    source_request.inputs["cook_loop"]["execution_budget_authority"] = serde_json::json!({
        "kind": "fresh_cook_review",
        "max_provider_executions": 2,
    });
    assert_eq!(
        follow_up_budget_scope(&source_request, &follow_up_request),
        CookFollowUpBudgetScope::Cook,
        "a review-only retry cannot mint another review allowance"
    );

    follow_up_request.inputs["cook_loop"]["review_form_required"] = serde_json::json!(false);
    assert_eq!(
        follow_up_budget_scope(&source_request, &follow_up_request),
        CookFollowUpBudgetScope::Cook
    );
}

#[test]
fn moving_base_recovery_report_retains_typed_evidence_and_exact_continuation() {
    let recovery = MovingBaseCookRecovery {
        schema: "homeboy/agent-task-cook-moving-base-recovery/v1".to_string(),
        cook_id: "cook-9267".to_string(),
        run_id: "run-9267".to_string(),
        promotion: promotion("run-9267"),
        prior_verified_base: "a".repeat(40),
        passed_gates: serde_json::json!([{"status": "passed"}]),
        blocker: "HEAD is behind or diverged from resolved base".to_string(),
        continuation: "homeboy agent-task cook-continue run-9267".to_string(),
        base_movements: 0,
    };
    let report =
        moving_base_recovery_report("cook-9267".to_string(), Vec::new(), recovery, true, None);

    assert_eq!(report.value.status, "candidate_recoverable");
    let recovery = report
        .value
        .moving_base_recovery
        .expect("typed recovery state");
    assert_eq!(recovery.run_id, "run-9267");
    assert_eq!(
        recovery.continuation,
        "homeboy agent-task cook-continue run-9267"
    );
    assert_eq!(recovery.prior_verified_base, "a".repeat(40));
    assert!(report
        .value
        .stop_reason
        .unwrap()
        .contains("without provider dispatch"));
}

#[test]
fn moving_base_recovery_persists_across_restart_without_provider_replay() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "moving-base-restart";
        let mut options =
            batch_cook_options("cook-restart", Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt("cook-restart", 1, run_id).unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        let mut recovery =
            moving_base_recovery_from_promotion("cook-restart", run_id, promotion(run_id));
        // Historical recovery records emitted the global scheduler command.
        recovery.continuation = "homeboy agent-task run-next".to_string();

        agent_task_lifecycle::record_cook_moving_base_recovery(
            run_id,
            serde_json::to_value(&recovery).unwrap(),
        )
        .unwrap();

        let restarted = moving_base_recovery_for_run(run_id)
            .unwrap()
            .expect("durable recovery");
        let record = agent_task_lifecycle::status(run_id).unwrap();
        assert_eq!(restarted.cook_id, "cook-restart");
        assert_eq!(restarted.run_id, run_id);
        assert_eq!(
            restarted.continuation,
            format!("homeboy agent-task cook-continue {run_id}")
        );
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
    });
}

#[test]
fn moving_base_recovery_refreshes_authenticated_candidate_before_retrying_finalization() {
    let mut original = promotion("moving-base-refresh");
    original.provenance["candidate"] =
        serde_json::json!({"kind": "git", "fingerprint": {"tree": "old"}});
    let recovery =
        moving_base_recovery_from_promotion("cook-refresh", "moving-base-refresh", original);
    let mut refreshed = promotion("moving-base-refresh");
    refreshed.verified_base.as_mut().unwrap().sha = "fresh-base".to_string();
    refreshed.provenance["candidate"] =
        serde_json::json!({"kind": "git", "fingerprint": {"tree": "rebased"}});

    let refreshed = refreshed_moving_base_recovery(recovery, &refreshed);

    assert_eq!(refreshed.prior_verified_base, "fresh-base");
    assert_eq!(
        refreshed.promotion.provenance["candidate"]["fingerprint"]["tree"],
        "rebased"
    );
    assert_eq!(refreshed.base_movements, 0);
}

#[test]
fn divergent_destination_and_repeated_base_movement_are_terminalized() {
    let recovery = moving_base_recovery_from_promotion(
        "cook-bound",
        "moving-base-bound",
        promotion("moving-base-bound"),
    );
    let divergent = next_moving_base_recovery(
        recovery.clone(),
        "moving-base recovery destination differs from the exact promoted candidate".to_string(),
    );
    assert_eq!(divergent.base_movements, 3);
    assert!(moving_base_recovery_report(
        "cook-bound".to_string(),
        Vec::new(),
        divergent,
        false,
        None
    )
    .value
    .stop_reason
    .unwrap()
    .contains("exhausted"));

    let first = next_moving_base_recovery(recovery, "base advanced".to_string());
    let second = next_moving_base_recovery(first, "base advanced again".to_string());
    let exhausted = next_moving_base_recovery(second, "base advanced a third time".to_string());
    assert_eq!(exhausted.base_movements, 3);
}

#[test]
fn moving_base_continuation_finalizes_without_a_second_provider_dispatch() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
        use crate::agent_task_scheduler::{
            AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
            AgentTaskProgressEvent,
        };

        let run_id = "cook-9267-attempt-1";
        let mut options = batch_cook_options(
            "cook-9267",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::new(AtomicUsize::new(0)),
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.no_finalize = false;
        options.provider_command = Some("fixture-provider".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["public gate".to_string()],
            private_verify: vec!["private gate".to_string()],
            private_gate_reveal: crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
            ..Default::default()
        };
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &options.initial_plan,
            &AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.initial_plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Succeeded,
                totals: AgentTaskAggregateTotals {
                    succeeded: 1,
                    ..Default::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("provider dispatched once".to_string()),
                    failure_classification: None,
                    artifacts: Vec::new(),
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: test_review_form_outputs(),
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: vec![AgentTaskProgressEvent {
                    task_id: "provider".to_string(),
                    state: AgentTaskState::Succeeded,
                    attempt: 1,
                    message: None,
                }],
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        let mut applied = promotion(run_id);
        applied.provenance["candidate"] =
            serde_json::json!({"kind":"git","fingerprint":{"tree":"before"}});
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&applied).unwrap())
            .unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();

        let first = run_cook_with_finalizer(options.clone(), UnusedExecutor, |_, _, _| {
            Err(Error::validation_invalid_argument(
                "base",
                "HEAD is behind or diverged from resolved base `main`",
                None,
                None,
            ))
        })
        .unwrap();
        assert_eq!(first.value.status, "candidate_recoverable");
        let claim = crate::agent_task_service::claim_continuation()
            .unwrap()
            .expect("run-next continuation");
        let rebase_count = Arc::new(AtomicUsize::new(0));
        let finalization_count = Arc::new(AtomicUsize::new(0));
        let rebase_count_for_recover = Arc::clone(&rebase_count);
        let finalization_count_for_finalize = Arc::clone(&finalization_count);
        let second = run_cook_with_boundaries(
            options.clone(),
            UnusedExecutor,
            TestCookSideEffects::new(
                move |_: &_, _: &_, promotion: &AgentTaskPromotionReport| {
                    finalization_count_for_finalize.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(
                        promotion.verified_base.as_ref().unwrap().sha,
                        "pinned-refreshed-base"
                    );
                    Ok(serde_json::json!({"status":"review_ready", "run_id": run_id}))
                },
                move |options: &AgentTaskCookServiceOptions, recovery: &MovingBaseCookRecovery| {
                    rebase_count_for_recover.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(
                        options.gates.private_gate_reveal,
                        crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence
                    );
                    let mut refreshed = recovery.promotion.clone();
                    refreshed.verified_base.as_mut().unwrap().sha =
                        "pinned-refreshed-base".to_string();
                    refreshed.provenance["candidate"] =
                        serde_json::json!({"kind":"git","fingerprint":{"tree":"rebased"}});
                    Ok(refreshed)
                },
            ),
        )
        .unwrap();
        claim.complete().unwrap();
        assert_eq!(second.value.status, "review_ready");
        assert_eq!(rebase_count.load(Ordering::SeqCst), 1);
        assert_eq!(finalization_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            agent_task_lifecycle::status(run_id).unwrap().metadata["provider_executions_consumed"],
            1
        );
        assert!(crate::agent_task_service::claim_continuation()
            .unwrap()
            .is_none());
        assert!(second.value.moving_base_recovery.is_none());

        // A rebase must not turn a failed declared gate into an attempted
        // finalization or a second provider dispatch.
        agent_task_lifecycle::record_cook_moving_base_recovery(
            run_id,
            serde_json::to_value(moving_base_recovery_from_promotion(
                "cook-9267",
                run_id,
                applied,
            ))
            .unwrap(),
        )
        .unwrap();
        let third = run_cook_with_boundaries(
            options,
            UnusedExecutor,
            TestCookSideEffects::new(
                |_: &_, _: &_, _: &_| panic!("failed rebased gates must not finalize"),
                |_: &AgentTaskCookServiceOptions, recovery: &MovingBaseCookRecovery| {
                    let mut failed = recovery.promotion.clone();
                    failed.status = AgentTaskPromotionStatus::GateFailed;
                    Ok(failed)
                },
            ),
        )
        .unwrap();
        assert_eq!(third.value.status, "candidate_recoverable");
        assert!(third
            .value
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("finalization was not attempted")));
        assert_eq!(
            moving_base_recovery_for_run(run_id)
                .unwrap()
                .expect("failed gate recovery remains durable")
                .promotion
                .status,
            AgentTaskPromotionStatus::GateFailed
        );
        assert!(crate::agent_task_service::claim_continuation()
            .unwrap()
            .is_none());
    });
}

#[test]
fn moving_base_recovery_rebases_real_authenticated_candidate_and_refuses_divergence() {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary repositories");
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let destination = temp.path().join("destination");
        let advance = temp.path().join("advance");
        let git = |path: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]);
        std::fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.com"]);
        std::fs::create_dir(seed.join("src")).unwrap();
        std::fs::write(seed.join("src/lib.rs"), "old\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "base"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);

        std::fs::create_dir(&destination).unwrap();
        git(
            temp.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        git(&destination, &["config", "user.name", "Test"]);
        git(&destination, &["config", "user.email", "test@example.com"]);
        std::fs::write(destination.join("src/lib.rs"), "new\n").unwrap();
        let patch = temp.path().join("candidate.patch");
        let patch_output = Command::new("git")
            .args(["diff", "--binary"])
            .current_dir(&destination)
            .output()
            .unwrap();
        assert!(patch_output.status.success());
        std::fs::write(&patch, patch_output.stdout).unwrap();
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(destination.to_str().unwrap())
                .unwrap();
        let verified_base = git_output(&destination, &["rev-parse", "HEAD"]).unwrap();

        std::fs::create_dir(&advance).unwrap();
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), advance.to_str().unwrap()],
        );
        git(&advance, &["config", "user.name", "Test"]);
        git(&advance, &["config", "user.email", "test@example.com"]);
        for revision in 1..=4 {
            std::fs::write(
                advance.join(format!("base-advanced-{revision}.txt")),
                format!("advanced {revision}\n"),
            )
            .unwrap();
            git(&advance, &["add", "."]);
            git(
                &advance,
                &["commit", "-m", &format!("advance base {revision}")],
            );
        }
        git(&advance, &["push", "origin", "main"]);
        let advanced_base = git_output(&advance, &["rev-parse", "HEAD"]).unwrap();

        let run_id = "moving-base-real-git";
        let mut options = batch_cook_options(
            "moving-base-real-git",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::new(AtomicUsize::new(0)),
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.no_finalize = false;
        options.gates = VerifyGateOptions {
            verify: vec![
                "test -f base-advanced-4.txt && test \"$(cat src/lib.rs)\" = new".to_string(),
            ],
            ..Default::default()
        };
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &options.initial_plan,
            &AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.initial_plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Succeeded,
                totals: AgentTaskAggregateTotals {
                    succeeded: 1,
                    ..Default::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: None,
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        id: "candidate".to_string(),
                        kind: "patch".to_string(),
                        path: Some(patch.display().to_string()),
                        ..Default::default()
                    }],
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: test_review_form_outputs(),
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        let applied: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "applied",
                "source": {"kind": "aggregate", "task_id": "provider", "run_id": run_id},
                "to_worktree": options.to_worktree,
                "target": {"worktree": options.to_worktree, "path": destination},
                "patch_artifact": {"id": "candidate", "kind": "patch", "path": patch},
                "changed_files": ["src/lib.rs"],
                "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-lc", "true"], "exit_code": 0}],
                "gate_results": [{"id": "gate", "name": "true", "kind": "command", "status": "passed"}],
                "verified_base": {"base": "main", "sha": verified_base},
                "provenance": {"worktree_path": destination, "candidate": candidate},
                "operator_notification": {"status": "completed", "message": "green"}
            }))
            .unwrap();
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&applied).unwrap())
            .unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        let first = run_cook_with_finalizer(options.clone(), UnusedExecutor, |_, _, _| {
            Err(Error::validation_invalid_argument(
                "base",
                "HEAD is behind or diverged from resolved base `main`",
                None,
                None,
            ))
        })
        .unwrap();
        assert_eq!(first.value.status, "candidate_recoverable");
        assert!(moving_base_recovery_for_run(run_id).unwrap().is_some());
        let claim = crate::agent_task_service::claim_continuation()
            .unwrap()
            .expect("durable moving-base continuation");
        let finalization_calls = Arc::new(AtomicUsize::new(0));
        let finalization_calls_for_finalizer = Arc::clone(&finalization_calls);
        let expected_base = advanced_base.clone();
        let second =
            run_cook_with_finalizer(options.clone(), UnusedExecutor, move |_, _, recovered| {
                finalization_calls_for_finalizer.fetch_add(1, Ordering::SeqCst);
                assert_eq!(recovered.status, AgentTaskPromotionStatus::Applied);
                assert_eq!(recovered.verified_base.as_ref().unwrap().sha, expected_base);
                Ok(serde_json::json!({"status": "review_ready"}))
            })
            .unwrap();
        assert_eq!(second.value.status, "review_ready");
        claim.complete().unwrap();
        assert_eq!(finalization_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            git_output(&destination, &["diff", "--name-only", &advanced_base]).unwrap(),
            "src/lib.rs",
            "only the original candidate file may remain after rebasing across four base commits"
        );
        assert_eq!(
            git_output(&destination, &["status", "--porcelain"]).unwrap(),
            "M src/lib.rs",
            "base-only files must not be projected into the candidate worktree"
        );
        assert_eq!(
            git_output(
                &destination,
                &["merge-base", "--is-ancestor", &advanced_base, "HEAD"]
            )
            .unwrap(),
            ""
        );
        assert_eq!(
            agent_task_lifecycle::status(run_id).unwrap().metadata["provider_executions_consumed"],
            1
        );
        assert!(moving_base_recovery_for_run(run_id).unwrap().is_none());
        assert!(crate::agent_task_service::claim_continuation()
            .unwrap()
            .is_none());

        std::fs::write(destination.join("divergent.txt"), "not authorized\n").unwrap();
        let rebased_promotion = persisted_promotion_for_attempt(run_id).unwrap().unwrap();
        let rebased_recovery =
            moving_base_recovery_from_promotion("moving-base-real-git", run_id, rebased_promotion);
        let error = recover_moving_base_cook_candidate(&options, &rebased_recovery).unwrap_err();
        assert!(error
            .message
            .contains("differs from the exact promoted candidate"));
    });
}

#[derive(Debug)]
struct AcceptedDetachedAttemptDispatcher;

impl AgentTaskCookAttemptDispatcher for AcceptedDetachedAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-detached" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "accepted-daemon-job",
                remote_workspace: "/runner/workspace",
                remote_command: &["homeboy".to_string(), "agent-task".to_string()],
            },
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct RecordingDetachedAttemptDispatcher {
    dispatches: Arc<AtomicUsize>,
}

impl AgentTaskCookAttemptDispatcher for RecordingDetachedAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-recording-detached" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "recording-daemon-job",
                remote_workspace: "/runner/workspace",
                remote_command: &["homeboy".to_string(), "agent-task".to_string()],
            },
        )?;
        Ok(())
    }
}

#[derive(Clone)]
struct UnusedExecutor;

impl AgentTaskExecutorAdapter for UnusedExecutor {
    fn execute(
        &self,
        _request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        panic!("accepted detached attempts must remain daemon-owned")
    }
}

#[derive(Clone)]
struct SucceedingExecutor;

impl AgentTaskExecutorAdapter for SucceedingExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        let root = std::path::PathBuf::from(
            request
                .workspace
                .root
                .as_deref()
                .expect("provider receives attempt workspace"),
        );
        std::fs::write(root.join("provider.txt"), "completed\n").expect("write provider change");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("run provider git")
                .success());
        };
        git(&["add", "provider.txt"]);
        git(&[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit",
            "-m",
            "provider change",
        ]);
        crate::agent_task::AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("fixture provider succeeded".to_string()),
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

#[derive(Clone)]
struct ReviewFormOnlyExecutor;

impl AgentTaskExecutorAdapter for ReviewFormOnlyExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        crate::agent_task::AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("review form emitted without modifying the candidate".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: test_review_form_outputs(),
            workflow: None,
            follow_up: None,
            metadata: serde_json::json!({ "model": request.executor.model() }),
        }
    }
}

#[derive(Clone)]
struct ProviderMissingExecutor;

impl AgentTaskExecutorAdapter for ProviderMissingExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        crate::agent_task::AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
            summary: Some("no extension agent-task provider found".to_string()),
            failure_classification: Some(
                crate::agent_task::AgentTaskFailureClassification::CapabilityMissing,
            ),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: vec![crate::agent_task::AgentTaskDiagnostic {
                class: "agent_task.provider_missing".to_string(),
                message: "no extension agent-task provider found".to_string(),
                data: Value::Null,
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

#[derive(Debug)]
struct ProviderDiscoveryReplayDispatcher {
    dispatches: AtomicUsize,
    provider_missing_before_success: usize,
}

impl AgentTaskCookAttemptDispatcher for ProviderDiscoveryReplayDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-provider-discovery-replay" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        let dispatch = self.dispatches.fetch_add(1, Ordering::SeqCst);
        let provider_missing = dispatch < self.provider_missing_before_success;
        let result = if provider_missing {
            run_loaded_plan_with_derived_cook_baseline(
                plan,
                Some(run_id),
                ProviderMissingExecutor,
                derived_cook_baseline,
                None,
            )?
        } else {
            run_loaded_plan_with_derived_cook_baseline(
                plan,
                Some(run_id),
                ReviewFormOnlyExecutor,
                derived_cook_baseline,
                None,
            )?
        };
        if provider_missing {
            assert_eq!(result.exit_code, 1);
            return Err(Error::internal_unexpected(
                "fixture runner reported provider discovery failure",
            ));
        }
        assert_eq!(result.exit_code, 0);
        Ok(())
    }
}

#[derive(Debug)]
struct BatchAttemptDispatcher {
    barrier: Arc<Barrier>,
    entered: Arc<AtomicUsize>,
    fail: bool,
}

/// Records the notification route observed from inside the batch worker
/// thread, so a lost thread-local binding is caught at the fanout boundary
/// rather than only at the primitive.
#[derive(Debug)]
struct RouteObservingAttemptDispatcher {
    observed: Arc<Mutex<Vec<Option<String>>>>,
}

impl AgentTaskCookAttemptDispatcher for RouteObservingAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-route-observer" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.observed
            .lock()
            .expect("observed routes")
            .push(homeboy_core::notification_route::current().map(|route| route.route));
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "fixture-job",
                remote_workspace: "/runner/workspace",
                remote_command: &["homeboy".to_string(), "agent-task".to_string()],
            },
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct AdmissionFailingAttemptDispatcher {
    message: &'static str,
}

#[derive(Debug)]
struct RetryableTransportFailingAttemptDispatcher {
    dispatches: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct FlakyPreparationDispatcher {
    failures_remaining: AtomicUsize,
}

#[derive(Debug)]
struct QueuedPreparationDispatcher {
    barrier: Arc<Barrier>,
    state: Arc<(Mutex<(bool, bool)>, Condvar)>,
    connections: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct PinOrderingDispatcher {
    observed_pin_during_preparation: Arc<AtomicBool>,
}

impl AgentTaskCookAttemptDispatcher for PinOrderingDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-pin-ordering" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        let pins = homeboy_core::paths::runtime_promotion_dir()?.join("pins");
        let pin_exists = pins.exists()
            && std::fs::read_dir(pins)
                .map(|entries| entries.flatten().next().is_some())
                .unwrap_or(false);
        self.observed_pin_during_preparation
            .store(pin_exists, Ordering::SeqCst);
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id,
            runner_id: "fixture-lab",
            runner_job_id: "accepted-daemon-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .map(|_| ())
    }
}

impl AgentTaskCookAttemptDispatcher for FlakyPreparationDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-flaky-preparation" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        if self.failures_remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Err(Error::validation_invalid_argument(
                "runner",
                "fixture runner is unavailable",
                None,
                None,
            )
            .with_retryable(true));
        }
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::record_detached_lab_run(agent_task_lifecycle::DetachedLabRunRecord {
            run_id,
            runner_id: "fixture-lab",
            runner_job_id: "accepted-daemon-job",
            remote_workspace: "/runner/workspace",
            remote_command: &["homeboy".to_string(), "agent-task".to_string()],
        })
        .map(|_| ())
    }
}

impl AgentTaskCookAttemptDispatcher for QueuedPreparationDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-queued-preparation" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        self.barrier.wait();
        let (state_mutex, ready) = &*self.state;
        let mut state = state_mutex.lock().expect("queued preparation state");
        if state.1 {
            return Ok(());
        }
        if state.0 {
            while !state.1 {
                state = ready.wait(state).expect("queued preparation wait");
            }
            return Ok(());
        }
        state.0 = true;
        drop(state);

        self.connections.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut state = state_mutex.lock().expect("queued preparation owner state");
        state.1 = true;
        ready.notify_all();
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        panic!("transport preparation test does not dispatch a provider attempt")
    }
}

impl AgentTaskCookAttemptDispatcher for AdmissionFailingAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-admission-failure" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        agent_task_lifecycle::submit_plan_with_runtime_admission(&plan, Some(run_id), |_| {
            Err::<Value, _>(Error::validation_invalid_argument(
                "controller_admission",
                self.message,
                Some("fixture controller diagnostics".to_string()),
                None,
            ))
        })?;
        Ok(())
    }
}

impl AgentTaskCookAttemptDispatcher for RetryableTransportFailingAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-retryable-transport-failure" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Err(Error::new(
            homeboy_core::error::ErrorCode::RunnerLabTransportFailure,
            "fixture transport disconnected",
            serde_json::json!({ "phase": "lab_handoff" }),
        )
        .with_retryable(true))
    }
}

impl AgentTaskCookAttemptDispatcher for BatchAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-batch" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.barrier.wait();
        if self.fail {
            return Err(Error::validation_invalid_argument(
                "dispatch",
                "fixture dispatch failure",
                None,
                None,
            ));
        }
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "fixture-job",
                remote_workspace: "/runner/workspace",
                remote_command: &["homeboy".to_string(), "agent-task".to_string()],
            },
        )?;
        Ok(())
    }
}

fn batch_cook_options(
    cook_id: &str,
    dispatcher: Arc<dyn AgentTaskCookAttemptDispatcher>,
) -> AgentTaskCookServiceOptions {
    AgentTaskCookServiceOptions {
        cook_id: cook_id.to_string(),
        initial_run_id: format!("{cook_id}-run"),
        initial_plan: AgentTaskPlan::new(
            cook_id,
            vec![AgentTaskRequest {
                schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "provider".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "fixture".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "complete the task".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                metadata: Value::Null,
            }],
        ),
        to_worktree: format!("fixture@{cook_id}"),
        source_worktree_path: None,
        provider_command: None,
        provider_invocation: None,
        gates: VerifyGateOptions::default(),
        max_attempts: 1,
        no_finalize: true,
        base: "main".to_string(),
        task_base_sha: None,
        head: None,
        title: "Batch cook".to_string(),
        commit_message: "test".to_string(),
        source_refs: Vec::new(),
        protected_branches: Vec::new(),
        ai_tool: "test".to_string(),
        ai_model: None,
        ai_used_for: "test".to_string(),
        attempt_dispatcher: Some(dispatcher),
        harvest_context: Default::default(),
    }
}

#[test]
fn initial_finalizing_provider_request_projects_complete_review_form_dossier() {
    let mut options = batch_cook_options(
        "initial-review-form-contract",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    );
    options.no_finalize = false;

    project_initial_finalizing_review_form_contract(&mut options);

    let request = &options.initial_plan.tasks[0];
    let declaration = request
        .output_declarations
        .iter()
        .find(|declaration| declaration.name == "review_form")
        .expect("review form declaration");
    assert!(declaration.required);
    assert_eq!(declaration.schema, "homeboy/agent-task-review-form/v1");
    assert_eq!(
        declaration.structural_schema["required"],
        serde_json::json!(["summary", "what_changed", "compatibility", "used_for"])
    );
    assert!(request.instructions.contains("reviewer-facing PR dossier"));
    assert!(request.instructions.contains("A successful response"));

    project_initial_finalizing_review_form_contract(&mut options);
    assert_eq!(
        options.initial_plan.tasks[0]
            .output_declarations
            .iter()
            .filter(|declaration| declaration.name == "review_form")
            .count(),
        1
    );
}

#[test]
fn no_finalize_initial_request_preserves_its_existing_contract() {
    let mut options = batch_cook_options(
        "no-finalize-review-form-contract",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    );
    let original = options.initial_plan.tasks[0].clone();

    project_initial_finalizing_review_form_contract(&mut options);

    assert_eq!(options.initial_plan.tasks[0], original);
}

/// A complete externally-prepared candidate: the original cook failed before a
/// provider was accepted, while a separate immutable source commit is ready to
/// be promoted and adopted.
struct CandidateAdoptionFixture {
    _temp: tempfile::TempDir,
    _source: std::path::PathBuf,
    target: std::path::PathBuf,
    candidate: String,
    cook_id: String,
    run_id: String,
    options: AgentTaskCookServiceOptions,
}

impl CandidateAdoptionFixture {
    fn new(
        cook_id: &str,
        max_attempts: u32,
        max_same_provider_retries: u32,
        no_finalize: bool,
        dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
    ) -> Self {
        Self::new_with_execution_budget(
            cook_id,
            max_attempts,
            max_attempts,
            max_same_provider_retries,
            no_finalize,
            dispatcher,
        )
    }

    fn new_with_execution_budget(
        cook_id: &str,
        max_attempts: u32,
        max_provider_executions: u32,
        max_same_provider_retries: u32,
        no_finalize: bool,
        dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
    ) -> Self {
        let temp = tempfile::tempdir().expect("temporary adoption repositories");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source repository");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let git_output = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("read git output");
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "agent@example.test"]);
        git(&source, &["config", "user.name", "Agent"]);
        std::fs::write(source.join("lib.rs"), "base\n").unwrap();
        git(&source, &["add", "lib.rs"]);
        git(&source, &["commit", "-m", "base"]);
        let base = git_output(&source, &["rev-parse", "HEAD"]);
        git(
            &source,
            &["remote", "add", "origin", source.to_str().unwrap()],
        );
        git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "fixture-candidate",
                target.to_str().unwrap(),
                "HEAD",
            ],
        );
        homeboy_core::worktree::adopt(homeboy_core::worktree::WorktreeAdoptOptions {
            handle: format!("fixture@{cook_id}"),
            path: target.display().to_string(),
            kind: Some("test-fixture".to_string()),
            provenance: None,
        })
        .expect("register adopted candidate target workspace");
        std::fs::write(source.join("lib.rs"), "candidate\n").unwrap();
        git(&source, &["commit", "-am", "candidate"]);
        let candidate = git_output(&source, &["rev-parse", "HEAD"]);
        let provider = temp.path().join("promotion-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\ncat >/dev/null\ngit -C {target} fetch origin {candidate}\ngit -C {target} reset --hard --quiet FETCH_HEAD\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                target = target.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).unwrap();
        }

        let run_id = format!("{cook_id}-attempt-1");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.attempt_dispatcher = dispatcher;
        options.initial_run_id = run_id.clone();
        options.source_worktree_path = Some(source.clone());
        options.task_base_sha = Some(base.clone());
        options.provider_command = Some(provider.display().to_string());
        options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
        options.max_attempts = max_attempts;
        options.initial_plan.options.execution_budget =
            crate::agent_task_scheduler::AgentTaskExecutionBudget::new(
                max_provider_executions,
                max_same_provider_retries,
                0,
            );
        options.no_finalize = no_finalize;
        options.head = Some("fix/8058".to_string());
        options.ai_model = Some("openai/gpt-5.6-terra".to_string());
        super::super::persist_initial_recipe(&options).unwrap();

        let mut fixture = Self {
            _temp: temp,
            _source: source,
            target,
            candidate,
            cook_id: cook_id.to_string(),
            run_id,
            options,
        };
        fixture.authenticate_pre_provider_recovery();
        fixture
    }

    fn authenticate_pre_provider_recovery(&mut self) {
        agent_task_lifecycle::record_lab_offload_phase(
            &self.run_id,
            "fixture-lab",
            "lab_handoff_preacceptance",
            None,
            None,
            None,
            Some(&self.options.initial_plan),
        )
        .unwrap();
        let attempt = super::super::load_recipe(&self.cook_id)
            .unwrap()
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == self.run_id)
            .unwrap()
            .attempt;
        agent_task_lifecycle::record_cook_attempt(&self.cook_id, attempt, &self.run_id).unwrap();
        agent_task_lifecycle::record_pre_execution_failure(
            &self.run_id,
            &self.options.initial_plan,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected("fixture pre-provider transport failure"),
        )
        .unwrap();
        let record = agent_task_lifecycle::status(&self.run_id).unwrap();
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert!(candidate_adoption_source(&record, &self.options.initial_plan.tasks[0]).is_ok());
    }

    fn append_adoptable_attempt(&mut self, attempt: u32) {
        assert!(attempt > 1);
        let run_id = agent_task_lifecycle::cook_attempt_run_id(&self.cook_id, attempt);
        super::super::record_recipe_attempt(
            &self.cook_id,
            attempt,
            &run_id,
            &self.options.initial_plan,
        )
        .unwrap();
        self.run_id = run_id;
        self.options.initial_run_id = self.run_id.clone();
        self.authenticate_pre_provider_recovery();
    }

    fn adopt<E: AgentTaskExecutorAdapter + Clone>(
        &self,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        executor: E,
        backend: &mut CaptureBackend,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        self.adopt_run(&self.run_id, dispatcher, executor, backend)
    }

    fn adopt_run<E: AgentTaskExecutorAdapter + Clone>(
        &self,
        run_id: &str,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        executor: E,
        backend: &mut CaptureBackend,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        adopt_cook_candidate_with_dispatcher_and_backend(
            run_id,
            &self.candidate,
            AgentTaskCandidateAdoptionOptions {
                ai_model: Some("openai/gpt-5.6-terra".to_string()),
                replace_interrupted: false,
            },
            dispatcher,
            executor,
            backend,
        )
    }
}

#[test]
fn cook_persists_controller_admission_timeout_before_provider_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-admission-timeout";
        let run_id = "cook-admission-timeout-attempt-1";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "timed out waiting for controller generation admission",
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        let result = run_cook(
            AgentTaskCookServiceOptions {
                initial_run_id: run_id.to_string(),
                ..options
            },
            UnusedExecutor,
        )
        .expect("cook returns the persisted dispatch failure");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.latest_run_id.as_deref(), Some(run_id));
        assert_eq!(result.value.history_run_ids, vec![run_id]);
        let record = agent_task_lifecycle::status(run_id).expect("returned attempt is resolvable");
        let logs = agent_task_lifecycle::logs(run_id).expect("failed attempt logs are resolvable");
        let retry = agent_task_lifecycle::retry(run_id, Some("cook-admission-timeout-retry"))
            .expect("failed admission attempt is retryable");

        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert!(record.provider_handles.is_empty());
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "controller_admission"
        );
        assert_eq!(
            record.metadata["pre_execution_failure"]["failure_code"],
            "controller_admission"
        );
        assert!(record.metadata["pre_execution_failure"]["message"]
            .as_str()
            .expect("failure message")
            .contains("timed out waiting for controller generation admission"));
        assert_eq!(
            record.metadata["pre_execution_failure"]["details"]["id"],
            "fixture controller diagnostics"
        );
        assert_eq!(
            record.metadata["pre_execution_failure"]["provider_executions_consumed"],
            0
        );
        assert_eq!(
            logs.events.last().map(|event| event.status),
            Some(AgentTaskState::Failed)
        );
        assert_eq!(retry.metadata["retry_of"], run_id);
        assert_eq!(
            retry.metadata["retry_origin"]["pre_execution_failure"]["phase"],
            "controller_admission"
        );
    });
}

#[test]
fn active_cooks_on_the_same_canonical_worktree_record_a_nonblocking_warning() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let workspace = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let mut options = batch_cook_options(
            "cook-worktree-warning",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.to_worktree = workspace.display().to_string();
        options.source_worktree_path = Some(workspace.clone());
        options.initial_plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("submit current Cook");

        for run_id in ["active-cook-z", "active-cook-a"] {
            let mut plan = options.initial_plan.clone();
            plan.plan_id = run_id.to_string();
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit active Cook");
            agent_task_lifecycle::record_cook_attempt(run_id, 1, run_id).expect("mark active Cook");
        }

        super::record_active_cook_worktree_warning(&options)
            .expect("active worktree warning must not block Cook");

        let current = agent_task_lifecycle::status(&options.initial_run_id)
            .expect("current Cook remains inspectable");
        assert_eq!(
            current.state,
            agent_task_lifecycle::AgentTaskRunState::Queued
        );
        assert_eq!(
            current.metadata["cook_active_worktree_warning"],
            serde_json::json!({
                "schema": "homeboy/cook-active-worktree-warning/v1",
                "canonical_worktree": workspace,
                "active_run_ids": ["active-cook-a", "active-cook-z"],
                "status_commands": [
                    "homeboy agent-task status active-cook-a",
                    "homeboy agent-task status active-cook-z"
                ],
            })
        );
    });
}

#[test]
fn retryable_pre_provider_retry_stays_attached_to_its_cook() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-pre-provider-retry";
        let options = retryable_pre_provider_cook(cook_id, 2);
        let first_run_id = options.initial_run_id.as_str();

        let retry = crate::agent_task_service::retry(first_run_id, None, false, false)
            .expect("corrected environment retry remains Cook-owned");
        let retry_run_id = retry.record.run_id.clone();
        let completed = crate::agent_task_service::execution::run_submitted(
            retry_run_id.clone(),
            ReviewFormOnlyExecutor,
        )
        .expect("corrected environment retry succeeds");

        assert_eq!(completed.exit_code, 0);
        assert!(retry_run_id.starts_with(&format!("{cook_id}-attempt-2-")));
        assert_eq!(retry.record.metadata["retry_of"], first_run_id);
        assert_eq!(retry.record.metadata["cook_id"], cook_id);
        assert_eq!(retry.record.metadata["cook_attempt"], 2);
        let recipe = super::super::load_recipe(cook_id).expect("updated Cook recipe");
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].attempt, 2);
        assert_eq!(recipe.attempts[1].run_id, retry_run_id);
        assert_eq!(recipe.attempts[1].plan, options.initial_plan);
        let index = agent_task_lifecycle::cook_index(cook_id).expect("updated Cook index");
        assert_eq!(index.latest_run_id, retry_run_id);
        assert_eq!(index.attempts.len(), 2);
        assert_eq!(
            agent_task_lifecycle::status(cook_id)
                .expect("Cook alias resolves successful retry")
                .state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn retryable_pre_provider_retry_propagates_force_after_terminal_successor() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-forced-terminal-retry", 3);
        let first_retry =
            crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
                .expect("reserve second attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            &first_retry.record.run_id,
            &options.initial_plan,
            "lab_handoff",
            &Error::internal_io(
                "File name too long",
                Some("finalize staged artifact".to_string()),
            )
            .with_retryable(true),
        )
        .expect("terminalize second attempt");

        let forced =
            crate::agent_task_service::retry(&first_retry.record.run_id, None, false, true)
                .expect("force reserves third attempt");

        assert_eq!(forced.record.metadata["cook_attempt"], 3);
        assert_eq!(
            forced.record.metadata["retry_of"],
            first_retry.record.run_id
        );
    });
}

#[test]
fn retryable_pre_provider_retry_accepts_runner_rebound_workspace_identity() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-runner-rebound-retry";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = format!("{cook_id}-attempt-1");
        options.max_attempts = 3;
        let controller_identity = serde_json::json!({
            "canonical_path": "/controller/worktree",
            "git_representation": "pointer_file",
        });
        options.initial_plan.tasks[0].metadata["cook_workspace_identity"] =
            controller_identity.clone();
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        super::super::materialize_initial_cook_attempt(&options)
            .expect("materialize first attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            &options.initial_run_id,
            &options.initial_plan,
            "lab_handoff",
            &Error::internal_io("projection failed", None).with_retryable(true),
        )
        .expect("fail first attempt");

        let second = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect("reserve second attempt");
        let mut runner_plan = options.initial_plan.clone();
        runner_plan.tasks[0].metadata["cook_workspace_identity"] = serde_json::json!({
            "canonical_path": "/runner/worktree",
            "git_representation": "directory",
        });
        runner_plan.tasks[0].metadata["cook_workspace_identity_predecessor"] = controller_identity;
        agent_task_lifecycle::record_pre_execution_failure(
            &second.record.run_id,
            &runner_plan,
            "lab_handoff",
            &Error::internal_io("projection failed", None).with_retryable(true),
        )
        .expect("fail runner-bound second attempt");

        let third = crate::agent_task_service::retry(&second.record.run_id, None, false, true)
            .expect("runner-bound plan retains Cook retry ownership");
        assert_eq!(third.record.metadata["cook_attempt"], 3);

        runner_plan.tasks[0].executor.model = Some("different/model".to_string());
        assert!(!super::super::execution::cook_retry_plans_match(
            &options.initial_plan,
            &runner_plan,
        ));
    });
}

#[test]
fn retryable_pre_provider_retry_without_a_recipe_uses_legacy_lifecycle_retry() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "cook-legacy-retry",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        let run_id = "cook-legacy-retry-run";
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist legacy run");
        agent_task_lifecycle::record_cook_attempt("cook-legacy-retry", 1, run_id)
            .expect("mark legacy Cook metadata without a recipe");
        agent_task_lifecycle::record_pre_execution_failure(
            run_id,
            &options.initial_plan,
            "gate_environment.preserve",
            &Error::validation_invalid_argument("CARGO_HOME", "unavailable", None, None)
                .with_retryable(true),
        )
        .expect("record retryable legacy failure");

        let retry = crate::agent_task_service::retry(run_id, Some("legacy-retry"), false, false)
            .expect("legacy retry remains supported");

        assert_eq!(retry.record.run_id, "legacy-retry");
        assert_eq!(retry.record.metadata["retry_of"], run_id);
        assert!(retry.record.metadata["cook_id"].is_null());
    });
}

fn retryable_pre_provider_cook(cook_id: &str, max_attempts: u32) -> AgentTaskCookServiceOptions {
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = format!("{cook_id}-attempt-1");
    options.max_attempts = max_attempts;
    super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
    super::super::materialize_initial_cook_attempt(&options).expect("materialize first attempt");
    agent_task_lifecycle::record_pre_execution_failure(
        &options.initial_run_id,
        &options.initial_plan,
        "gate_environment.preserve",
        &Error::validation_invalid_argument(
            "CARGO_HOME",
            "required environment capability is unavailable",
            None,
            None,
        )
        .with_retryable(true),
    )
    .expect("record retryable environment failure");
    options
}

#[test]
fn retryable_pre_provider_retry_rejects_a_lifecycle_reservation_collision_without_recipe_mutation()
{
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-collision", 2);
        let collision_id = "cook-retry-reservation-collision";
        let unrelated = AgentTaskPlan::new("unrelated-plan", Vec::new());
        agent_task_lifecycle::submit_plan(&unrelated, Some(collision_id))
            .expect("submit unrelated run");

        let error = crate::agent_task_service::retry(
            &options.initial_run_id,
            Some(collision_id),
            false,
            false,
        )
        .expect_err("colliding lifecycle reservation is rejected before Cook mutation");

        assert_eq!(error.details["field"], "new_run_id");
        assert_eq!(
            agent_task_lifecycle::load_plan(collision_id).expect("unrelated plan remains intact"),
            unrelated
        );
        assert_eq!(
            super::super::load_recipe(&options.cook_id)
                .expect("recipe remains intact")
                .attempts
                .len(),
            1
        );
        assert_eq!(
            agent_task_lifecycle::cook_index(&options.cook_id)
                .expect("index remains intact")
                .attempts
                .len(),
            1
        );
    });
}

#[test]
fn retryable_pre_provider_retry_enforces_the_persisted_attempt_budget() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-budget", 1);

        let error = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect_err("retry cannot exceed the persisted Cook budget");

        assert_eq!(
            error.details["field"],
            "cook_recipe.retry_budget.max_attempts"
        );
        assert_eq!(
            super::super::load_recipe(&options.cook_id)
                .expect("recipe remains intact")
                .attempts
                .len(),
            1
        );
        assert_eq!(
            agent_task_lifecycle::cook_index(&options.cook_id)
                .expect("index remains intact")
                .attempts
                .len(),
            1
        );
    });
}

#[test]
fn retryable_pre_provider_retry_repairs_lifecycle_reserved_attempts_idempotently() {
    homeboy_core::test_support::with_isolated_home(|_| {
        // Simulate a lifecycle retry that crashed after its durable retry proof
        // was written but before Cook metadata/index binding.
        let options = retryable_pre_provider_cook("cook-retry-submitted-repair", 2);
        let submitted_run_id = agent_task_lifecycle::cook_attempt_run_id(&options.cook_id, 2);
        agent_task_lifecycle::retry(&options.initial_run_id, Some(&submitted_run_id))
            .expect("submit retry before Cook metadata binding");

        let repaired =
            crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
                .expect("repair submitted retry");
        assert_eq!(repaired.record.run_id, submitted_run_id);
        assert_eq!(repaired.record.metadata["cook_id"], options.cook_id);
        assert_eq!(repaired.record.metadata["cook_attempt"], 2);
        assert_eq!(
            super::super::load_recipe(&options.cook_id)
                .expect("no duplicate recipe attempt")
                .attempts
                .len(),
            2
        );
        assert_eq!(
            agent_task_lifecycle::cook_index(&options.cook_id)
                .expect("repaired Cook index")
                .attempts
                .len(),
            2
        );
        assert_eq!(
            agent_task_lifecycle::list_records()
                .expect("durable lifecycle records")
                .len(),
            2,
            "repair binds the indexed reservation instead of orphaning another run"
        );
    });
}

#[test]
fn retryable_pre_provider_retry_concurrently_claims_one_successor() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-concurrent", 2);
        let barrier = Arc::new(Barrier::new(4));
        let retries = (0..4)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let run_id = options.initial_run_id.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    crate::agent_task_service::retry(&run_id, None, false, false)
                        .expect("concurrent retry converges")
                        .record
                        .run_id
                })
            })
            .collect::<Vec<_>>();
        let run_ids = retries
            .into_iter()
            .map(|retry| retry.join().expect("retry thread"))
            .collect::<Vec<_>>();

        assert!(run_ids.iter().all(|run_id| run_id == &run_ids[0]));
        let recipe = super::super::load_recipe(&options.cook_id).expect("bound recipe");
        let index = agent_task_lifecycle::cook_index(&options.cook_id).expect("bound index");
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].run_id, run_ids[0]);
        assert_eq!(index.attempts.len(), 2);
        assert_eq!(index.latest_run_id, run_ids[0]);
        assert_eq!(
            agent_task_lifecycle::list_records()
                .expect("durable lifecycle records")
                .len(),
            2,
            "the source and one bound successor are the only durable runs"
        );
    });
}

#[test]
fn retryable_pre_provider_retry_concurrently_claims_one_successor_across_processes() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-process", 2);
        let test_binary = std::env::current_exe().expect("test binary path");
        let mut workers = (0..2)
            .map(|_| {
                Command::new(&test_binary)
                    .args([
                        "--exact",
                        "agent_task_service::cook::tests::retryable_pre_provider_retry_process_worker",
                    ])
                    .env("HOMEBOY_RETRY_PROCESS_WORKER", &options.initial_run_id)
                    .spawn()
                    .expect("start retry worker")
            })
            .collect::<Vec<_>>();
        assert!(workers
            .iter_mut()
            .all(|worker| worker.wait().expect("wait for retry worker").success()));

        let recipe = super::super::load_recipe(&options.cook_id).expect("bound recipe");
        let index = agent_task_lifecycle::cook_index(&options.cook_id).expect("bound index");
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(index.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].run_id, index.latest_run_id);
        assert_eq!(
            agent_task_lifecycle::list_records()
                .expect("durable lifecycle records")
                .len(),
            2,
            "the source and one bound successor are the only durable runs"
        );
    });
}

#[test]
fn retryable_pre_provider_retry_process_worker() {
    let Ok(run_id) = std::env::var("HOMEBOY_RETRY_PROCESS_WORKER") else {
        return;
    };
    crate::agent_task_service::retry(&run_id, None, false, false).expect("process retry converges");
}

#[test]
fn retryable_pre_provider_retry_rejects_unowned_same_plan_collision_without_retry_proof() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-unowned-collision", 2);
        let pending_run_id = agent_task_lifecycle::cook_attempt_run_id(&options.cook_id, 2);
        super::super::record_recipe_attempt(
            &options.cook_id,
            2,
            &pending_run_id,
            &options.initial_plan,
        )
        .expect("reserve retry in recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&pending_run_id))
            .expect("occupy pending attempt with same plan");

        let error = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect_err("unowned same-plan run is not repaired without retry proof");

        assert!(error
            .to_string()
            .contains("not the durable retry of its source attempt"));
        let record = agent_task_lifecycle::exact_record(&pending_run_id)
            .expect("unowned collision remains intact");
        assert!(record.metadata["cook_id"].is_null());
        assert!(record.metadata["retry_of"].is_null());
    });
}

#[test]
fn retryable_pre_provider_retry_returns_terminal_successor_without_reexecution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-terminal-success", 2);
        let successor_run_id = agent_task_lifecycle::cook_attempt_run_id(&options.cook_id, 2);
        super::super::record_recipe_attempt(
            &options.cook_id,
            2,
            &successor_run_id,
            &options.initial_plan,
        )
        .expect("reserve retry in recipe");
        agent_task_lifecycle::retry(&options.initial_run_id, Some(&successor_run_id))
            .expect("persist successor retry proof");
        crate::agent_task_service::execution::run_submitted(
            successor_run_id.clone(),
            ReviewFormOnlyExecutor,
        )
        .expect("complete successor successfully");

        let retry = crate::agent_task_service::retry(&options.initial_run_id, None, true, false)
            .expect("terminal successor is authoritative");

        assert_eq!(retry.record.run_id, successor_run_id);
        assert_eq!(
            retry.record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
        assert!(!retry.run);
        assert_eq!(
            agent_task_lifecycle::cook_index(&options.cook_id)
                .expect("initial Cook index remains intact")
                .attempts
                .len(),
            2
        );
    });
}

#[test]
fn retryable_pre_provider_retry_rejects_pending_attempt_owned_by_another_cook() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = retryable_pre_provider_cook("cook-retry-owner-collision", 2);
        let pending_run_id = agent_task_lifecycle::cook_attempt_run_id(&options.cook_id, 2);
        super::super::record_recipe_attempt(
            &options.cook_id,
            2,
            &pending_run_id,
            &options.initial_plan,
        )
        .expect("reserve retry in recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&pending_run_id))
            .expect("occupy pending attempt");
        agent_task_lifecycle::record_cook_attempt("another-cook", 1, &pending_run_id)
            .expect("mark conflicting Cook ownership");

        let error = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect_err("conflicting Cook owner must not be rebound");

        assert!(error
            .to_string()
            .contains("not the durable retry of its source attempt"));
        let record = agent_task_lifecycle::exact_record(&pending_run_id)
            .expect("conflicting pending record remains intact");
        assert_eq!(record.metadata["cook_id"], "another-cook");
        assert_eq!(record.metadata["cook_attempt"], 1);
    });
}

#[test]
fn cook_repairs_initial_alias_after_submit_before_index_interruption() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-repair-initial-alias";
        let run_id = "cook-repair-initial-alias-run";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        super::super::persist_initial_recipe(&options).expect("persist durable recipe");

        // Simulate a controller crash after plan submission and before the
        // subsequent Cook attempt index write.
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist initial run");
        assert!(agent_task_lifecycle::cook_index(cook_id).is_err());

        let observed = Arc::new(Mutex::new(Vec::new()));
        let observer_records = observed.clone();
        let result =
            run_cook_with_durable_observer(options, UnusedExecutor, &move |phase, cook, run| {
                let status =
                    agent_task_lifecycle::status(cook).expect("Cook alias resolves in observer");
                let logs =
                    agent_task_lifecycle::logs(cook).expect("Cook alias logs resolve in observer");
                assert_eq!(status.run_id, run);
                assert!(!logs.events.is_empty());
                observer_records
                    .lock()
                    .expect("observer records lock")
                    .push((phase.to_string(), cook.to_string(), run.to_string()));
                Ok(())
            })
            .expect("restart repairs the Cook alias");

        assert_eq!(result.value.latest_run_id.as_deref(), Some(run_id));
        assert_eq!(
            observed.lock().expect("observer records lock").as_slice(),
            &[
                (
                    "durable_identity".to_string(),
                    cook_id.to_string(),
                    run_id.to_string()
                ),
                (
                    "provider_ready".to_string(),
                    cook_id.to_string(),
                    run_id.to_string()
                ),
                (
                    "provider_start".to_string(),
                    cook_id.to_string(),
                    run_id.to_string()
                ),
                (
                    "in_flight".to_string(),
                    cook_id.to_string(),
                    run_id.to_string()
                ),
            ]
        );
        assert_eq!(
            agent_task_lifecycle::status(run_id)
                .expect("durable progress")
                .metadata["cook_progress"]["phase"],
            "in_flight"
        );
        let index = agent_task_lifecycle::cook_index(cook_id).expect("repaired Cook index");
        assert_eq!(index.latest_run_id, run_id);
        assert_eq!(index.attempts.len(), 1);
        assert_eq!(index.attempts[0].attempt, 1);
        assert_eq!(index.attempts[0].run_id, run_id);
        assert_eq!(
            agent_task_lifecycle::status(cook_id)
                .expect("Cook alias status after restart")
                .run_id,
            run_id
        );
        assert!(agent_task_lifecycle::logs(cook_id).is_ok());
        assert!(
            agent_task_lifecycle::retry(cook_id, Some("cook-repair-initial-alias-retry")).is_ok()
        );
    });
}

/// Fails at the exact controller boundary where Lab materialization begins,
/// after recording whether the durable run record was already addressable by
/// id at that moment. Models the caller-timeout case in #9163/#10419.
#[derive(Debug)]
struct MaterializationInterruptingDispatcher {
    run_id: String,
    state_at_materialization: Arc<Mutex<Option<String>>>,
}

impl AgentTaskCookAttemptDispatcher for MaterializationInterruptingDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-materialization-interruption" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        // Controller-side Lab materialization starts here. The durable record
        // must already resolve by id, or an interruption from this point on
        // strands an unnamed reservation.
        let record = agent_task_lifecycle::status(&self.run_id)?;
        *self
            .state_at_materialization
            .lock()
            .expect("materialization state") = Some(format!("{:?}", record.state));
        Err(Error::internal_unexpected(
            "fixture caller was interrupted during Lab materialization",
        ))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        panic!("an interrupted materialization must never reach provider dispatch")
    }
}

#[test]
fn cook_publishes_durable_identity_before_materialization_and_survives_interruption() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-durable-identity-first";
        let run_id = "cook-durable-identity-first-run";
        let state_at_materialization = Arc::new(Mutex::new(None));
        let options = batch_cook_options(
            cook_id,
            Arc::new(MaterializationInterruptingDispatcher {
                run_id: run_id.to_string(),
                state_at_materialization: state_at_materialization.clone(),
            }),
        );

        let observed = Arc::new(Mutex::new(Vec::new()));
        let observer_records = observed.clone();
        let result =
            run_cook_with_durable_observer(options, UnusedExecutor, &move |phase, cook, run| {
                observer_records
                    .lock()
                    .expect("observer records lock")
                    .push((phase.to_string(), run.to_string()));
                // Every identity-bearing event must be immediately actionable:
                // the caller can look the run up the moment it is told about it.
                assert_eq!(
                    agent_task_lifecycle::status(cook)
                        .expect("Cook alias resolves in observer")
                        .run_id,
                    run
                );
                Ok(())
            })
            .expect("interrupted materialization returns a durable report");

        // 1. Identity is published before any materialization work.
        assert_eq!(
            observed
                .lock()
                .expect("observer records lock")
                .first()
                .cloned(),
            Some(("durable_identity".to_string(), run_id.to_string()))
        );

        // 2. The record was findable by id at the instant materialization began.
        assert_eq!(
            state_at_materialization
                .lock()
                .expect("materialization state")
                .as_deref(),
            Some("Queued")
        );

        // 3. The interruption left a named, recoverable record — not an
        //    anonymous reservation an operator has to hunt for.
        assert_eq!(result.value.status, "durable_failure");
        let record = agent_task_lifecycle::status(run_id).expect("record survives interruption");
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            serde_json::json!("cook_pre_execution")
        );
        assert_eq!(
            agent_task_lifecycle::status(cook_id)
                .expect("Cook alias resolves after interruption")
                .run_id,
            run_id
        );
        assert!(agent_task_lifecycle::logs(run_id).is_ok());
        // Recoverable, not merely observable: the operator can act on the id.
        assert!(
            agent_task_lifecycle::retry(cook_id, Some("cook-durable-identity-first-retry")).is_ok()
        );
    });
}

#[test]
fn cook_continue_adopts_recipe_bound_retry_missing_run_and_index() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary destination");
        let repository = temp.path().join("repository");
        let destination = temp.path().join("destination");
        std::fs::create_dir(&repository).expect("create repository");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("run git")
                .success());
        };
        git(&repository, &["init", "-b", "main"]);
        std::fs::write(repository.join("fixture.txt"), "base\n").expect("write base");
        git(&repository, &["add", "fixture.txt"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "base",
            ],
        );
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "fixture-candidate",
                destination.to_str().expect("destination path"),
                "HEAD",
            ],
        );
        let cook_id = "cook-repair-recipe-only-retry";
        let first_run_id = "cook-repair-recipe-only-retry-attempt-1";
        let stranded_run_id = "cook-repair-recipe-only-retry-attempt-2-stranded";
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.initial_run_id = first_run_id.to_string();
        options.max_attempts = 2;
        options.source_worktree_path = Some(destination.clone());
        options.initial_plan.tasks[0].workspace.root = Some(destination.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": options.to_worktree.clone(),
            "root": destination,
            "branch": "fixture-candidate",
        });
        homeboy_core::worktree::adopt(homeboy_core::worktree::WorktreeAdoptOptions {
            handle: options.to_worktree.clone(),
            path: destination.display().to_string(),
            kind: Some("test-fixture".to_string()),
            provenance: None,
        })
        .expect("register destination workspace");
        super::super::persist_initial_recipe(&options).expect("persist durable recipe");
        super::super::materialize_initial_cook_attempt(&options)
            .expect("materialize initial attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            first_run_id,
            &options.initial_plan,
            "lab_handoff_preacceptance",
            &Error::new(
                homeboy_core::error::ErrorCode::RunnerLabTransportFailure,
                "fixture runner identity mismatch",
                serde_json::json!({ "phase": "lab_handoff_preacceptance" }),
            )
            .with_retryable(true),
        )
        .expect("record retryable pre-acceptance failure");
        super::super::record_recipe_attempt(cook_id, 2, stranded_run_id, &options.initial_plan)
            .expect("persist recipe-only retry");

        assert!(!agent_task_lifecycle::run_record_exists(stranded_run_id).unwrap());
        assert_eq!(
            agent_task_lifecycle::cook_index(cook_id)
                .expect("initial Cook index")
                .latest_run_id,
            first_run_id
        );

        let result = run_cook_with_boundaries_observed_inner(
            options.clone(),
            UnusedExecutor,
            DefaultCookSideEffects::new(|_, _, _| Ok(serde_json::json!({}))),
            None,
            false,
        )
        .expect("continuation repairs and dispatches recipe-bound retry");
        let repeated = run_cook_with_boundaries_observed_inner(
            options,
            UnusedExecutor,
            DefaultCookSideEffects::new(|_, _, _| Ok(serde_json::json!({}))),
            None,
            false,
        )
        .expect("repeated continuation remains idempotent");

        assert_eq!(result.value.status, "in_flight", "{result:#?}");
        assert_eq!(result.value.latest_run_id.as_deref(), Some(stranded_run_id));
        assert_eq!(
            repeated.value.latest_run_id.as_deref(),
            Some(stranded_run_id)
        );
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        let index = agent_task_lifecycle::cook_index(cook_id).expect("repaired Cook index");
        assert_eq!(index.attempts.len(), 2);
        assert_eq!(index.latest_run_id, stranded_run_id);
        assert_eq!(
            agent_task_lifecycle::status(stranded_run_id)
                .expect("repaired retry record")
                .runner_job_id(),
            Some("recording-daemon-job")
        );
        assert_eq!(
            super::super::load_recipe(cook_id)
                .expect("stable recipe")
                .attempts
                .iter()
                .filter(|attempt| attempt.attempt == 2)
                .count(),
            1
        );
    });
}

#[test]
fn retry_after_admission_failure_restores_managed_workspace_after_baseline_cleanup() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temp source root");
        let source = temp.path().join("source");
        let managed = temp.path().join("managed");
        std::fs::create_dir(&source).expect("create source");
        std::fs::create_dir(&managed).expect("create managed workspace");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git")
                .success());
        };
        git(&["init"]);
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(source.join("fixture.txt"), "base\n").expect("write base");
        git(&["add", "fixture.txt"]);
        git(&["commit", "-m", "base"]);
        std::fs::write(source.join("fixture.txt"), "dirty candidate\n")
            .expect("write dirty candidate");
        assert!(Command::new("git")
            .args(["init"])
            .current_dir(&managed)
            .status()
            .expect("initialize managed workspace")
            .success());

        let run_id = "cook-admission-retry-attempt-1";
        let mut options = batch_cook_options(
            "cook-admission-retry",
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "controller generation is held by another cook",
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(source.clone());
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_plan.tasks[0].workspace.root = Some(managed.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": "managed@cook-admission-retry",
            "root": managed,
            "branch": "fix/cook-admission-retry",
        });

        run_cook(options, UnusedExecutor).expect("persist admission failure");
        let failed_plan = agent_task_lifecycle::load_plan(run_id).expect("failed plan");
        let transient_root = std::path::PathBuf::from(
            failed_plan.tasks[0]
                .workspace
                .root
                .as_deref()
                .expect("baseline root"),
        );
        assert!(!transient_root.exists(), "initial baseline was cleaned up");
        assert_eq!(
            failed_plan.tasks[0].metadata["cook_continuation_workspace"]["candidate_source_root"],
            serde_json::json!(source),
            "the persisted dispatch plan retains the dirty candidate source"
        );
        assert_eq!(
            failed_plan.tasks[0].metadata["cook_continuation_workspace"]["task_workspace"]["root"],
            serde_json::json!(managed),
            "the managed task workspace remains available for routing metadata"
        );

        // Retry reloads the persisted plan after the original controller and
        // its temporary baseline have gone away.
        let retry = agent_task_lifecycle::retry(run_id, Some("cook-admission-retry-2"))
            .expect("retry rematerializes source workspace");
        let retry_plan = agent_task_lifecycle::load_plan(&retry.run_id).expect("retry plan");
        assert_eq!(
            retry_plan.tasks[0].workspace.root.as_deref(),
            Some(source.to_str().expect("UTF-8 source path"))
        );

        let result =
            crate::agent_task_service::execution::run_submitted(retry.run_id, SucceedingExecutor)
                .expect("retry reaches a real Git workspace");
        assert_eq!(result.exit_code, 0, "{:#?}", result.value);
    });
}

#[test]
fn retry_reports_missing_candidate_source_as_retryable_recovery() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temp source root");
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("create source");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git")
                .success());
        };
        git(&["init"]);
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(source.join("fixture.txt"), "base\n").expect("write base");
        git(&["add", "fixture.txt"]);
        git(&["commit", "-m", "base"]);
        std::fs::write(source.join("fixture.txt"), "dirty candidate\n")
            .expect("write dirty candidate");

        let run_id = "cook-missing-worktree-attempt-1";
        let mut options = batch_cook_options(
            "cook-missing-worktree",
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "controller generation is held by another cook",
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(source.clone());
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": "source@cook-missing-worktree",
            "root": source,
        });

        run_cook(options, UnusedExecutor).expect("persist admission failure");
        std::fs::remove_dir_all(&source).expect("remove managed worktree");

        let error = agent_task_lifecycle::retry(run_id, Some("cook-missing-worktree-retry"))
            .expect_err("missing candidate source requires recovery");

        assert_eq!(error.retryable, Some(true));
        assert!(error.message.contains("candidate source workspace"));
        assert!(error.hints.iter().any(|hint| hint
            .message
            .contains("Restore the recorded candidate source workspace")));
    });
}

#[test]
fn cook_transport_preparation_failure_is_durable_and_resumes_after_runner_recovery() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-runner-unavailable";
        let first_run_id = "cook-runner-unavailable-attempt-1";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(FlakyPreparationDispatcher {
                failures_remaining: AtomicUsize::new(1),
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = first_run_id.to_string();
        options.max_attempts = 2;

        let error = run_cook(options.clone(), UnusedExecutor)
            .expect_err("transport preparation is outside the provider-attempt loop");

        assert!(error.message.contains("fixture runner is unavailable"));
        let blocked = agent_task_lifecycle::status(cook_id)
            .expect("cook alias exposes the preflight-blocked attempt");
        assert_eq!(blocked.run_id, first_run_id);
        assert_eq!(
            blocked.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(
            blocked.metadata["pre_execution_failure"]["retryable"],
            Value::Bool(true)
        );

        let resumed = run_cook(options, UnusedExecutor)
            .expect("repaired runner resumes the immutable cook attempt");
        assert_eq!(resumed.value.status, "in_flight");
        assert_eq!(
            agent_task_lifecycle::status(cook_id)
                .expect("resumed cook alias")
                .runner_job_id(),
            Some("accepted-daemon-job")
        );
    });
}

#[test]
fn cook_persists_materialization_failure_without_provider_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temp source root");
        let cook_id = "cook-materialization-failure";
        let run_id = "cook-materialization-failure-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(temp.path().to_path_buf());
        options.max_attempts = 3;

        let result =
            run_cook(options, UnusedExecutor).expect("cook records materialization failure");

        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(result.value.attempts.len(), 1);
        assert_eq!(
            result.value.terminal_phase.as_deref(),
            Some("materialize_initial_candidate_baseline")
        );
        assert_eq!(
            result.value.terminal_failure_classification.as_deref(),
            Some("invalid_input")
        );
        let record = agent_task_lifecycle::status(cook_id).expect("cook alias resolves failure");
        assert_eq!(record.run_id, run_id);
        assert!(record.provider_handles.is_empty());
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
    });
}

#[cfg(unix)]
#[test]
fn cook_claims_its_durable_attempt_before_slow_baseline_materialization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp source root");
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("create source repository");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "agent@example.test"],
            vec!["config", "user.name", "Agent"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git")
                .success());
        }
        std::fs::write(source.join("lib.rs"), "base\n").expect("write base");
        for args in [vec!["add", "lib.rs"], vec!["commit", "-m", "base"]] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git")
                .success());
        }
        std::fs::write(source.join("lib.rs"), "candidate\n").expect("dirty candidate");

        let entered = temp.path().join("baseline-entered");
        let release = temp.path().join("baseline-release");
        let wrapper = temp.path().join("git");
        std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nif test \"$1\" = status; then touch \"{}\"; while ! test -f \"{}\"; do sleep 0.01; done; fi\nexec /usr/bin/git \"$@\"\n",
                    entered.display(),
                    release.display(),
                ),
            )
            .expect("write slow git wrapper");
        let mut permissions = std::fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");
        let previous_path = std::env::var_os("PATH");
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                temp.path().display(),
                previous_path
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            "cook-slow-baseline",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.initial_run_id = "cook-slow-baseline-attempt-1".to_string();
        options.provider_command = Some("fixture-provider".to_string());
        options.source_worktree_path = Some(source);
        let resume_options = options.clone();
        let controller = std::thread::spawn(move || run_cook(options, UnusedExecutor));
        let entered_staging = (0..500).any(|_| {
            if entered.exists() {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            }
        });
        let durable = entered_staging.then(|| {
            agent_task_lifecycle::status("cook-slow-baseline-attempt-1")
                .expect("staging attempt is durable before controller completion")
        });
        std::fs::write(&release, "release").expect("release baseline staging");
        let result = controller
            .join()
            .expect("controller thread")
            .expect("accepted detached attempt");
        match previous_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }

        assert!(entered_staging, "baseline materialization did not block");
        let durable = durable.expect("durable record while staging was blocked");
        assert_eq!(
            durable.state,
            agent_task_lifecycle::AgentTaskRunState::Queued
        );
        assert!(agent_task_lifecycle::load_plan(&durable.run_id).is_ok());
        assert_eq!(result.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);

        let resumed = run_cook(resume_options, UnusedExecutor).expect("resume accepted handoff");
        assert_eq!(resumed.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn cook_transport_preparation_failure_does_not_exhaust_cook_retries() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-runner-exhaustion";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(FlakyPreparationDispatcher {
                failures_remaining: AtomicUsize::new(usize::MAX),
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = "cook-runner-exhaustion-attempt-1".to_string();
        options.max_attempts = 2;

        let error = run_cook(options, UnusedExecutor)
            .expect_err("transport preparation remains outside cook retries");

        assert!(error.message.contains("fixture runner is unavailable"));
        let record = agent_task_lifecycle::status("cook-runner-exhaustion")
            .expect("transport failure remains inspectable");
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
    });
}

#[test]
fn concurrent_cooks_share_transport_readiness_before_first_provider_attempt() {
    const COOKS: usize = 6;
    let connections = Arc::new(AtomicUsize::new(0));
    let dispatcher = Arc::new(QueuedPreparationDispatcher {
        barrier: Arc::new(Barrier::new(COOKS)),
        state: Arc::new((Mutex::new((false, false)), Condvar::new())),
        connections: Arc::clone(&connections),
    });
    let preparations = (0..COOKS)
        .map(|_| {
            let dispatcher = Arc::clone(&dispatcher);
            std::thread::spawn(move || dispatcher.prepare_for_cook())
        })
        .collect::<Vec<_>>();

    for preparation in preparations {
        preparation
            .join()
            .expect("cook preparation thread")
            .expect("shared transport becomes ready");
    }
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[test]
fn cook_prepares_transport_before_pinning_runtime_generation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let observed_pin = Arc::new(AtomicBool::new(false));
        let mut options = batch_cook_options(
            "cook-pin-ordering",
            Arc::new(PinOrderingDispatcher {
                observed_pin_during_preparation: Arc::clone(&observed_pin),
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = "cook-pin-ordering-attempt-1".to_string();

        let result = run_cook(options, UnusedExecutor).expect("cook accepts detached handoff");

        assert_eq!(result.value.status, "in_flight");
        assert!(
                !observed_pin.load(Ordering::SeqCst),
                "transport readiness must complete before the cook generation pin can block a reconnect"
            );
    });
}

#[test]
fn cook_persists_controller_runtime_mismatch_before_provider_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-runtime-mismatch-attempt-1";
        let mut options = batch_cook_options(
                "cook-runtime-mismatch",
                Arc::new(AdmissionFailingAttemptDispatcher {
                    message: "pinned controller executable hash mismatch: expected fixture, found replacement",
                }),
            );
        options.provider_command = Some("fixture-provider".to_string());
        let result = run_cook(
            AgentTaskCookServiceOptions {
                initial_run_id: run_id.to_string(),
                ..options
            },
            UnusedExecutor,
        )
        .expect("cook returns the persisted runtime mismatch");

        let record = agent_task_lifecycle::status(run_id).expect("runtime mismatch attempt exists");
        assert_eq!(result.exit_code, 1);
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert!(record.provider_handles.is_empty());
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert!(record.metadata["pre_execution_failure"]["message"]
            .as_str()
            .expect("failure message")
            .contains("hash mismatch"));
    });
}

#[test]
fn cook_does_not_retry_deterministic_pre_provider_input_failures() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-invalid-input-attempt-1";
        let mut options = batch_cook_options(
            "cook-invalid-input",
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "invalid controller-owned Lab handoff input",
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = run_id.to_string();
        options.max_attempts = 2;

        let result =
            run_cook(options, UnusedExecutor).expect("cook returns the persisted input failure");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(result.value.attempts.len(), 1);
        assert_eq!(result.value.history_run_ids, vec![run_id]);
        assert_eq!(
            result.value.terminal_phase.as_deref(),
            Some("controller_admission")
        );
        assert_eq!(
            result.value.terminal_failure_classification.as_deref(),
            Some("invalid_input")
        );
        let record = agent_task_lifecycle::status(run_id).expect("attempt exists");
        assert!(record.provider_handles.is_empty());
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
    });
}

#[test]
fn cook_retries_retryable_pre_provider_transport_failures_within_attempt_budget() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let cook_id = "cook-retryable-transport";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RetryableTransportFailingAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = "cook-retryable-transport-attempt-1".to_string();
        options.max_attempts = 2;

        let result = run_cook(options, UnusedExecutor).expect("cook records transport retries");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "retries_exhausted");
        assert_eq!(result.value.attempts.len(), 2);
        assert_eq!(dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(result.value.history_run_ids.len(), 2);
        assert_eq!(
            result.value.history_run_ids[0],
            "cook-retryable-transport-attempt-1"
        );
        assert!(result.value.history_run_ids[1].starts_with("cook-retryable-transport-attempt-2-"));
        for run_id in &result.value.history_run_ids {
            let record = agent_task_lifecycle::status(run_id).expect("retry attempt exists");
            assert!(record.provider_handles.is_empty());
            assert_eq!(record.metadata["provider_executions_consumed"], 0);
            assert_eq!(record.metadata["pre_execution_failure"]["retryable"], true);
            assert_eq!(
                record.metadata["pre_execution_failure"]["failure_classification"],
                "transient"
            );
        }
    });
}

#[test]
fn cook_batch_preserves_order_concurrency_and_failure_isolation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
            .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                "agent_task_service::cook::tests::cook_batch_preserves_order_concurrency_and_failure_isolation_process",
            ])
            .status()
            .expect("run process-isolated cook batch");
    assert!(status.success());
}

#[test]
fn cook_batch_children_inherit_the_callers_notification_route() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
            .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                "agent_task_service::cook::tests::cook_batch_children_inherit_the_callers_notification_route_process",
            ])
            .status()
            .expect("run process-isolated cook batch route propagation");
    assert!(status.success());
}

#[test]
#[ignore = "invoked by cook_batch_children_inherit_the_callers_notification_route"]
fn cook_batch_children_inherit_the_callers_notification_route_process() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let cooks = ["first", "second"]
        .into_iter()
        .map(|cook_id| {
            batch_cook_options(
                cook_id,
                Arc::new(RouteObservingAttemptDispatcher {
                    observed: Arc::clone(&observed),
                }),
            )
        })
        .collect::<Vec<_>>();
    for cook in &cooks {
        agent_task_lifecycle::submit_plan(&cook.initial_plan, Some(&cook.initial_run_id))
            .expect("submit attempt");
    }

    let route = homeboy_core::notification_route::NotificationRoute::new(
        "extension",
        "opaque-fanout-route",
    )
    .expect("route");
    homeboy_core::notification_route::with_current(Some(route), || {
        run_cook_batch(
            AgentTaskCookBatchOptions {
                batch_id: "fixture-route-batch".to_string(),
                cooks,
                max_concurrency: 2,
            },
            UnusedExecutor,
        )
        .expect("batch completes")
    });

    // Every worker runs on its own thread; without propagation each would
    // observe None and the originating destination would never be notified.
    let observed = observed.lock().expect("observed routes").clone();
    assert_eq!(observed.len(), 2);
    for route in observed {
        assert_eq!(route.as_deref(), Some("opaque-fanout-route"));
    }
}

#[test]
#[ignore = "invoked by cook_batch_preserves_order_concurrency_and_failure_isolation"]
fn cook_batch_preserves_order_concurrency_and_failure_isolation_process() {
    let barrier = Arc::new(Barrier::new(2));
    let entered = Arc::new(AtomicUsize::new(0));
    let first = batch_cook_options(
        "first",
        Arc::new(BatchAttemptDispatcher {
            barrier: Arc::clone(&barrier),
            entered: Arc::clone(&entered),
            fail: true,
        }),
    );
    let second = batch_cook_options(
        "second",
        Arc::new(BatchAttemptDispatcher {
            barrier,
            entered: Arc::clone(&entered),
            fail: false,
        }),
    );
    // The batch owns concurrent dispatch, not concurrent controller
    // admission; materialize both durable run identities first.
    agent_task_lifecycle::submit_plan(&first.initial_plan, Some(&first.initial_run_id))
        .expect("submit first attempt");
    agent_task_lifecycle::submit_plan(&second.initial_plan, Some(&second.initial_run_id))
        .expect("submit second attempt");
    let result = run_cook_batch(
        AgentTaskCookBatchOptions {
            batch_id: "fixture-batch".to_string(),
            cooks: vec![first, second],
            max_concurrency: 2,
        },
        UnusedExecutor,
    )
    .expect("batch completes despite an individual cook failure");

    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.value.status, "running");
    assert_eq!(result.value.total, 2);
    assert_eq!(result.value.succeeded, 0);
    assert_eq!(result.value.running, 1);
    assert_eq!(result.value.failed, 1);
    assert_eq!(result.value.cooks[0].cook_id, "first");
    assert_eq!(result.value.cooks[0].exit_code, 1);
    assert_eq!(
        result.value.cooks[0]
            .result
            .as_ref()
            .expect("failed cook report")
            .status,
        "pre_execution_failure"
    );
    assert_eq!(result.value.cooks[1].cook_id, "second");
    assert_eq!(result.value.cooks[1].exit_code, 0);
    assert_eq!(
        result.value.cooks[1]
            .result
            .as_ref()
            .expect("successful cook report")
            .status,
        "in_flight"
    );
}

#[test]
fn cook_batch_aggregate_outcome_matrix_distinguishes_success_partial_and_failure() {
    let cell = |id: &str, status: &str, exit_code| AgentTaskCookBatchCellReport {
        cook_id: id.to_string(),
        initial_run_id: format!("{id}-run"),
        status: status.to_string(),
        exit_code,
        result: None,
        error: (exit_code != 0).then(|| "infrastructure admission failed".to_string()),
    };

    for (name, cells, status, exit_code) in [
        (
            "all-success",
            vec![
                cell("one", "review_ready", 0),
                cell("two", "green_no_finalize", 0),
            ],
            "succeeded",
            0,
        ),
        (
            "mixed",
            vec![cell("one", "review_ready", 0), cell("two", "failed", 1)],
            "partial_failure",
            1,
        ),
        (
            "all-failed",
            vec![cell("one", "failed", 1), cell("two", "failed", 1)],
            "failed",
            1,
        ),
        (
            "all-cancelled",
            vec![cell("one", "cancelled", 1), cell("two", "cancelled", 1)],
            "cancelled",
            1,
        ),
        (
            "cancelled-and-success",
            vec![cell("one", "cancelled", 1), cell("two", "review_ready", 0)],
            "partial_failure",
            1,
        ),
        (
            "timed-out",
            vec![cell("one", "timed_out", 1)],
            "timed_out",
            1,
        ),
        ("in-flight", vec![cell("one", "in_flight", 0)], "running", 0),
        (
            "queued-with-terminal-child",
            vec![cell("one", "queued", 0), cell("two", "review_ready", 0)],
            "queued",
            0,
        ),
        (
            "running-with-terminal-child",
            vec![cell("one", "running", 0), cell("two", "failed", 1)],
            "running",
            0,
        ),
        (
            "infrastructure-error",
            vec![cell("one", "failed", 1)],
            "failed",
            1,
        ),
    ] {
        let result = cook_batch_result(name.to_string(), cells);
        assert_eq!(result.value.status, status, "{name}");
        assert_eq!(result.exit_code, exit_code, "{name}");
        assert_eq!(
            result.value.succeeded
                + result.value.failed
                + result.value.cancelled
                + result.value.timed_out
                + result.value.queued
                + result.value.running,
            result.value.total
        );
    }
}

#[test]
fn cook_returns_after_accepted_detached_attempt_without_waiting_for_daemon_completion() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-detached-attempt-1";
        let plan = AgentTaskPlan::new(
            "cook-detached",
            vec![AgentTaskRequest {
                schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "provider".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "fixture".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "complete the task".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                metadata: Value::Null,
            }],
        );
        let result = run_cook(
            AgentTaskCookServiceOptions {
                cook_id: "cook-detached".to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: plan,
                to_worktree: "fixture@detached".to_string(),
                source_worktree_path: None,
                // This test covers handoff only; an explicit transport
                // intentionally bypasses configured-provider preflight.
                provider_command: Some("fixture-promotion-provider".to_string()),
                provider_invocation: None,
                gates: VerifyGateOptions::default(),
                max_attempts: 1,
                no_finalize: true,
                base: "main".to_string(),
                task_base_sha: None,
                head: None,
                title: "Detached cook".to_string(),
                commit_message: "test".to_string(),
                source_refs: Vec::new(),
                protected_branches: Vec::new(),
                ai_tool: "test".to_string(),
                ai_model: None,
                ai_used_for: "test".to_string(),
                attempt_dispatcher: Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
                harvest_context: Default::default(),
            },
            UnusedExecutor,
        )
        .expect("accepted detached cook returns");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status, "in_flight");
        assert_eq!(result.value.attempts.len(), 1);
        assert_eq!(result.value.attempts[0].run_id, run_id);
        let record = agent_task_lifecycle::status(run_id).expect("detached attempt record");
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Running
        );
        assert_eq!(record.runner_id(), Some("fixture-lab"));
        assert_eq!(record.runner_job_id(), Some("accepted-daemon-job"));
    });
}

#[test]
fn orphaned_recipe_materializes_once_and_rejects_changed_inputs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-orphan-recovery";
        let run_id = "cook-orphan-recovery-attempt-1";
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.provider_command = Some("fixture-provider".to_string());

        // Simulate interruption after the immutable recipe commit and before
        // the dispatcher creates the first run record.
        super::super::persist_initial_recipe(&options).expect("persist orphaned recipe");
        assert!(!agent_task_lifecycle::run_record_exists(run_id).expect("check orphan"));

        let recovered = run_cook(options.clone(), UnusedExecutor).expect("recover orphan");
        assert_eq!(recovered.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        let record = agent_task_lifecycle::status(run_id).expect("materialized run record");
        assert_eq!(record.runner_job_id(), Some("recording-daemon-job"));

        let replayed = run_cook(options.clone(), UnusedExecutor).expect("idempotent replay");
        assert_eq!(replayed.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(agent_task_lifecycle::status(run_id).unwrap(), record);

        let mut changed = options;
        changed.title = "changed immutable finalization title".to_string();
        let error = run_cook(changed, UnusedExecutor).expect_err("changed recipe rejected");
        assert!(error
            .message
            .contains("durable cook recipe already exists with different execution inputs"));
    });
}

#[test]
fn adoption_by_cook_id_materializes_the_exact_orphaned_recipe_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-orphan";
        let run_id = "cook-adopt-orphan-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist orphaned recipe");

        let (record, recipe) =
            resolve_adoption_target(cook_id).expect("adoption resolves orphaned cook");

        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, run_id);
        assert_eq!(record.metadata["cook_id"], cook_id);
        assert_eq!(
            record.metadata["pre_execution_failure"]["candidate_adoption_recovery"]["reason"],
            "pre_provider_transport_failure"
        );
        assert!(agent_task_lifecycle::run_record_exists(run_id).expect("record exists"));
    });
}

#[test]
fn adoption_prefers_authenticated_preacceptance_recovery_over_failure_aggregate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-adopt-preacceptance-recovery";
        let options = batch_cook_options(
            "cook-adopt-preacceptance",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        let plan = options.initial_plan;
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            "homeboy-lab",
            "lab_handoff_preacceptance",
            None,
            None,
            None,
            Some(&plan),
        )
        .expect("record preacceptance phase");
        agent_task_lifecycle::record_pre_execution_failure(
            run_id,
            &plan,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected("Lab handoff JSON was truncated"),
        )
        .expect("record failed preacceptance attempt");
        let record = agent_task_lifecycle::status(run_id).expect("failed attempt");
        assert!(record.aggregate_path.is_some());

        let (_source, source_path, recovery) =
            candidate_adoption_source(&record, &plan.tasks[0]).expect("recovery source");

        assert!(source_path.is_none());
        assert_eq!(
            recovery.expect("recovery provenance")["reason"],
            "pre_provider_transport_failure"
        );
    });
}

#[test]
fn historical_orphan_recipe_adoption_uses_recorded_policy_without_provider_replay() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source repository");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("run git")
                .success());
        };
        let git_output = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("read git output");
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .expect("UTF-8 git output")
                .trim()
                .to_string()
        };
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "agent@example.test"]);
        git(&source, &["config", "user.name", "Agent"]);
        std::fs::write(source.join("lib.rs"), "base\n").expect("write base");
        git(&source, &["add", "lib.rs"]);
        git(&source, &["commit", "-m", "base"]);
        let base = git_output(&source, &["rev-parse", "HEAD"]);
        assert!(Command::new("git")
            .args(["clone", source.to_str().unwrap(), target.to_str().unwrap()])
            .status()
            .expect("clone target repository")
            .success());
        std::fs::write(source.join("lib.rs"), "candidate\n").expect("write candidate");
        git(&source, &["commit", "-am", "candidate"]);
        let candidate = git_output(&source, &["rev-parse", "HEAD"]);
        let provider = temp.path().join("promotion-provider.sh");
        let provider_started = temp.path().join("provider-started");
        let provider_release = temp.path().join("provider-release");
        std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncat >/dev/null\ntouch {provider_started}\nwhile ! test -f {provider_release}; do sleep 0.01; done\ngit -C {target} fetch origin {candidate}\ngit -C {target} checkout --detach FETCH_HEAD\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                    target = target.display(),
                    provider_started = provider_started.display(),
                    provider_release = provider_release.display(),
                ),
            )
            .expect("write promotion provider");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        }

        let cook_id = "cook-historical-adoption";
        let run_id = "cook-historical-adoption-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(source.clone());
        options.task_base_sha = Some(base.clone());
        options.provider_command = Some(provider.display().to_string());
        options.attempt_dispatcher = None;
        options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
        options.max_attempts = 2;
        options.initial_plan.options.execution_budget =
            crate::agent_task_scheduler::AgentTaskExecutionBudget::new(2, 1, 0);
        options.no_finalize = false;
        options.head = Some("fix/8058".to_string());
        options.ai_model = Some("openai/gpt-5.6-terra".to_string());
        let mut recipe = super::super::persist_initial_recipe(&options).expect("persist recipe");
        recipe.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();
        let recipe_path = homeboy_core::paths::homeboy_data()
            .expect("Homeboy data path")
            .join("agent-task-cooks")
            .join(cook_id)
            .join("recipe.json");
        std::fs::write(&recipe_path, serde_json::to_vec(&recipe).unwrap())
            .expect("persist historical runtime");

        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        agent_task_lifecycle::record_lab_offload_planned(
            agent_task_lifecycle::LabOffloadProxyPlan {
                run_id,
                runner_id: "fixture-lab",
                remote_workspace: "/runner/workspace",
                remote_command: &command,
                durable_plan: Some(&options.initial_plan),
            },
        )
        .expect("persist preacceptance handoff");
        let submission_request: homeboy_core::api_jobs::RemoteRunnerJobRequest =
            serde_json::from_value(serde_json::json!({
                "runner_id": "fixture-lab",
                "command": command,
                "metadata": { "submission_key": "historical-orphan-submission" }
            }))
            .expect("fixture runner submission request");
        agent_task_lifecycle::record_lab_offload_submission_request(run_id, &submission_request)
            .expect("persist pending handoff request");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link recipe attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            let handoff = record.lab_handoff.as_mut().expect("typed handoff");
            handoff.acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
            handoff.state = agent_task_lifecycle::AgentTaskLabHandoffState::Expired;
            handoff.expired_at = Some("2000-01-01T00:00:01+00:00".to_string());
            record.state = agent_task_lifecycle::AgentTaskRunState::Cancelled;
            record.metadata["phase"] = serde_json::json!("handoff_rejected");
            record.metadata["handoff_acceptance"] = serde_json::json!({
                "state": "expired",
                "reason": agent_task_lifecycle::EXPIRED_LAB_HANDOFF_REASON,
                "expired_at": "2000-01-01T00:00:01+00:00",
            });
        })
        .expect("expire handoff deadline");
        let expired = agent_task_lifecycle::status(run_id).expect("expire preacceptance handoff");
        assert_eq!(
            expired.state,
            agent_task_lifecycle::AgentTaskRunState::Cancelled
        );
        assert!(expired.aggregate_path.is_none());
        assert!(expired.artifact_refs.is_empty());
        assert_eq!(expired.metadata["provider_executions_consumed"], 0);

        let invalid =
            adopt_cook_candidate(cook_id, &base).expect_err("candidate validation remains active");
        assert!(invalid
            .message
            .contains("candidate revision must equal the recorded source worktree HEAD"));

        let candidate_for_thread = candidate.clone();
        let adoption = std::thread::spawn(move || {
            let mut backend = CaptureBackend {
                hydrate_run_id: Some(run_id.to_string()),
                ..Default::default()
            };
            let result = adopt_cook_candidate_with_dispatcher_and_backend(
                cook_id,
                &candidate_for_thread,
                AgentTaskCandidateAdoptionOptions {
                    ai_model: Some("openai/gpt-5.6-sol".to_string()),
                    replace_interrupted: false,
                },
                |_| Ok(None),
                ReviewFormOnlyExecutor,
                &mut backend,
            );
            (result, backend)
        });
        let provider_started_in_time = (0..500).any(|_| {
            if provider_started.exists() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        let running = provider_started_in_time
            .then(|| agent_task_lifecycle::status(run_id))
            .transpose();
        // Always release and join before asserting so a regression cannot
        // strand the fake provider and hang the test process.
        std::fs::write(&provider_release, "release").expect("release provider");
        let adoption_result = adoption.join();
        assert!(provider_started_in_time, "promotion provider did not start");
        let running = running
            .expect("blocked adoption status")
            .expect("provider started before status capture");
        let active = running.candidate_adoption.expect("active adoption attempt");
        assert_eq!(active.state, "verification_running");
        assert_eq!(active.phase, "verification");
        assert_eq!(active.active_gate, "test \"$(cat lib.rs)\" = candidate");
        assert_eq!(active.candidate_sha, candidate);
        assert_eq!(active.ai_model, "openai/gpt-5.6-sol");
        assert_eq!(active.owner_pid, std::process::id());
        assert!(!active.heartbeat_at.is_empty());
        let (result, backend) = adoption_result.expect("adoption thread completes");
        let result = result.expect("historical recipe adoption succeeds");

        assert_eq!(result.exit_code, 0, "{:?}", result.value);
        assert_eq!(result.value.status, "review_ready", "{:?}", result.value);
        assert_eq!(result.value.attempts.len(), 2);
        assert_eq!(
            result.value.attempts[0]
                .promotion
                .as_ref()
                .unwrap()
                .gate_results
                .len(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(target.join("lib.rs")).unwrap(),
            "candidate\n"
        );
        let promoted = agent_task_lifecycle::status(run_id).expect("adopted lifecycle record");
        assert_eq!(
            promoted.metadata["latest_promotion"]["provenance"]["adoption"]["candidate_ref"],
            candidate
        );
        assert_eq!(
            promoted.metadata["latest_promotion"]["provenance"]["adoption"]["recovery"]
                ["provider_executions_consumed"],
            0
        );
        assert_eq!(
            promoted.metadata["latest_promotion"]["provenance"]["adoption"]["ai_model"],
            "openai/gpt-5.6-sol"
        );
        assert_eq!(
            promoted.metadata["latest_promotion"]["provenance"]["adoption"]["ai_model_source"],
            "candidate_input"
        );
        let adoption = promoted
            .candidate_adoption
            .expect("terminal adoption status");
        assert_eq!(adoption.state, "completed");
        assert_eq!(adoption.candidate_sha, candidate);
        assert_eq!(adoption.ai_model, "openai/gpt-5.6-sol");
        assert!(backend.body.contains("- **Tool(s):** Homeboy (test)"));
        assert!(backend.body.contains("- **Model:** openai/gpt-5.6-sol"));
        assert!(backend.committed && backend.pushed && backend.created);
    });
}

#[test]
fn adoption_green_candidate_missing_review_form_runs_form_only_follow_up_and_finalizes() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary repositories");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source repository");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let git_output = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("read git output");
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.email", "agent@example.test"]);
        git(&source, &["config", "user.name", "Agent"]);
        std::fs::write(source.join("lib.rs"), "base\n").unwrap();
        git(&source, &["add", "lib.rs"]);
        git(&source, &["commit", "-m", "base"]);
        let base = git_output(&source, &["rev-parse", "HEAD"]);
        git(
            &source,
            &["remote", "add", "origin", source.to_str().unwrap()],
        );
        git(&source, &["fetch", "origin", "main"]);
        git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "fixture-target",
                target.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(target.join("lib.rs"), "candidate\n").unwrap();
        git(&target, &["commit", "-am", "candidate"]);
        let candidate = git_output(&target, &["rev-parse", "HEAD"]);
        let provider = temp.path().join("promotion-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\ncat >/dev/null\ngit -C {target} reset --hard --quiet {candidate}\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                target = target.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).unwrap();
        }

        let cook_id = "cook-adopt-review-form";
        let run_id = "cook-adopt-review-form-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(target.clone());
        options.task_base_sha = Some(base.clone());
        options.provider_command = Some(provider.display().to_string());
        options.attempt_dispatcher = None;
        options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
        options.max_attempts = 2;
        options.no_finalize = false;
        options.head = Some("fix/8058".to_string());
        options.ai_tool = "fixture-provider".to_string();
        options.ai_model = Some("fixture-model-implementation".to_string());
        options.initial_plan.tasks[0].executor.backend = "fixture-provider".to_string();
        options.initial_plan.tasks[0].executor.model =
            Some("fixture-model-implementation".to_string());
        let workspace_handle = format!("fixture@{cook_id}");
        homeboy_core::worktree::adopt(homeboy_core::worktree::WorktreeAdoptOptions {
            handle: workspace_handle.clone(),
            path: target.display().to_string(),
            kind: Some("test-fixture".to_string()),
            provenance: None,
        })
        .expect("register fixture destination workspace");
        options.to_worktree = workspace_handle.clone();
        super::super::persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        seed_missing_review_form_aggregate(run_id, &options.initial_plan);
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();

        let mut gate_proof = serde_json::to_value(promotion("fixture-gate-proof")).unwrap();
        gate_proof["to_worktree"] = serde_json::json!(workspace_handle);
        gate_proof["target"] = serde_json::json!({
            "worktree": workspace_handle,
            "path": target.display().to_string(),
        });
        gate_proof["changed_files"] = serde_json::json!(["lib.rs"]);
        gate_proof["verified_base"] = serde_json::json!({ "base": "main", "sha": base });
        let candidate_checkout = serde_json::json!({
            "schema": "homeboy/agent-task-gate-candidate-checkout/v1",
            "commit": candidate,
            "tree": "fixture-candidate-tree",
            "candidate_sha256": "fixture-candidate-sha",
        });
        gate_proof["deterministic_gates"][0]["command"] =
            serde_json::json!(["sh", "-lc", "test \"$(cat lib.rs)\" = candidate"]);
        gate_proof["deterministic_gates"][0]["candidate_checkout"] = candidate_checkout.clone();
        gate_proof["provenance"]["worktree_path"] = serde_json::json!(target.display().to_string());
        gate_proof["provenance"]["candidate_checkout"] = candidate_checkout;
        let mut backend = CaptureBackend {
            synthetic_gate_proof: Some(serde_json::from_value(gate_proof).unwrap()),
            ..Default::default()
        };
        let result = adopt_cook_candidate_with_dispatcher_and_backend(
            cook_id,
            &candidate,
            AgentTaskCandidateAdoptionOptions {
                ai_model: Some("fixture-model-review".to_string()),
                replace_interrupted: false,
            },
            |_| Ok(None),
            ReviewFormOnlyExecutor,
            &mut backend,
        )
        .expect("adoption retries only the missing review form");

        let follow_up_aggregate = result
            .value
            .latest_run_id
            .as_deref()
            .and_then(|run_id| agent_task_lifecycle::read_aggregate(run_id).ok());
        assert_eq!(
            result.exit_code, 0,
            "{:?}\nfollow-up aggregate: {follow_up_aggregate:#?}",
            result.value
        );
        assert_eq!(result.value.status, "review_ready");
        assert_eq!(result.value.attempts.len(), 2);
        assert_eq!(
            result.value.attempts[0].feedback.as_ref().unwrap().status,
            AgentTaskCookLoopStatus::RetryRequested
        );
        assert_eq!(
            result.value.attempts[1].feedback.as_ref().unwrap().status,
            AgentTaskCookLoopStatus::GreenCompleted
        );
        let follow_up_run_id = &result.value.attempts[1].run_id;
        let follow_up_promotion = persisted_promotion_for_attempt(follow_up_run_id)
            .unwrap()
            .expect("form-only continuation carries promoted candidate");
        let source_promotion = persisted_promotion_for_attempt(run_id)
            .unwrap()
            .expect("source attempt retains its normalized gate proof");
        let alias_promotion = persisted_promotion_for_attempt(cook_id)
            .unwrap()
            .expect("Cook alias carries the same promoted candidate");
        let replayed_alias_promotion = persisted_promotion_for_attempt(cook_id)
            .unwrap()
            .expect("Cook alias recovery is idempotent");
        assert_eq!(
            follow_up_promotion.provenance["cook_follow_up"]["kind"],
            "review_form_only"
        );
        assert_eq!(
            alias_promotion.source.run_id,
            follow_up_promotion.source.run_id
        );
        assert_eq!(
            alias_promotion.patch_artifact.id,
            follow_up_promotion.patch_artifact.id
        );
        assert_eq!(
            replayed_alias_promotion.source.run_id,
            follow_up_promotion.source.run_id
        );
        assert_eq!(source_promotion.target.worktree, workspace_handle);
        assert_eq!(
            source_promotion.target.path.as_deref(),
            Some(target.to_str().unwrap())
        );
        assert_eq!(
            source_promotion
                .verified_base
                .as_ref()
                .expect("source proof records the verified base")
                .sha,
            base
        );
        assert_eq!(
            source_promotion.provenance["adoption"]["candidate_ref"],
            candidate
        );
        assert_eq!(
            source_promotion.provenance["candidate"]["fingerprint"]["head"],
            candidate
        );
        assert_eq!(
            std::fs::read_to_string(target.join("lib.rs")).unwrap(),
            "candidate\n"
        );
        assert!(backend.body.contains("## Summary\ncomplete the task"));
        assert!(backend.body.contains(
            "1. Run `test \"$(cat lib.rs)\" = candidate`; expect passes as recorded by Cook's deterministic gate."
        ));
        assert!(backend.body.contains(
            "**Tool(s):** Implementation: Homeboy (fixture-provider); review form: Homeboy (fixture-provider)"
        ));
        assert!(backend
            .body
            .contains("**Model:** Implementation: fixture-model-implementation; review form: fixture-model-review"));
        assert!(backend.body.contains(
            "**Used for:** Implementation: Homeboy (fixture-provider) authored the delivered candidate changes"
        ));
        assert!(backend.body.contains(
            "Review form: Homeboy (fixture-provider) reviewed the validated candidate and supplied the reviewer metadata."
        ));
        assert!(backend.committed && backend.pushed && backend.created);
    });
}

#[test]
fn pre_provider_adoption_retries_only_the_missing_form_binds_model_and_reaches_review_ready() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fixture = CandidateAdoptionFixture::new("cook-9575-local", 2, 1, false, None);
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(fixture.run_id.clone()),
            ..Default::default()
        };

        let result = fixture
            .adopt(|_| Ok(None), ReviewFormOnlyExecutor, &mut backend)
            .expect("authenticated pre-provider adoption succeeds");

        let follow_up_aggregate = result
            .value
            .latest_run_id
            .as_deref()
            .and_then(|run_id| agent_task_lifecycle::read_aggregate(run_id).ok());
        assert_eq!(
            result.exit_code, 0,
            "{:#?}\nfollow-up aggregate: {follow_up_aggregate:#?}",
            result.value
        );
        assert_eq!(result.value.status, "review_ready");
        assert_eq!(result.value.attempts.len(), 2);
        assert_eq!(
            result.value.attempts[0].feedback.as_ref().unwrap().status,
            AgentTaskCookLoopStatus::RetryRequested
        );
        let adoption = agent_task_lifecycle::status(&fixture.run_id)
            .unwrap()
            .candidate_adoption
            .unwrap();
        assert_eq!(adoption.ai_model, "openai/gpt-5.6-terra");
        assert_eq!(adoption.candidate_sha, fixture.candidate);
        assert!(backend.body.contains("- **Model:** openai/gpt-5.6-terra"));
        assert!(backend.committed && backend.pushed && backend.created);
    });
}

#[test]
fn adoption_review_uses_one_bounded_execution_after_the_source_budget_is_consumed() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fixture = CandidateAdoptionFixture::new_with_execution_budget(
            "cook-9575-consumed-source-budget",
            2,
            1,
            0,
            false,
            None,
        );
        let mut aggregate = agent_task_lifecycle::read_aggregate(&fixture.run_id).unwrap();
        aggregate
            .events
            .push(crate::agent_task_scheduler::AgentTaskProgressEvent {
                task_id: "cook-homeboy".to_string(),
                state: AgentTaskState::Running,
                attempt: 1,
                message: Some("historical provider execution".to_string()),
            });
        let aggregate_path = agent_task_lifecycle::status(&fixture.run_id)
            .unwrap()
            .aggregate_path
            .expect("fixture aggregate path");
        std::fs::write(
            aggregate_path,
            serde_json::to_vec_pretty(&aggregate).unwrap(),
        )
        .unwrap();
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(fixture.run_id.clone()),
            ..Default::default()
        };

        let result = fixture
            .adopt(|_| Ok(None), ReviewFormOnlyExecutor, &mut backend)
            .expect("adoption review has a separate bounded execution allowance");

        assert_eq!(result.value.status, "review_ready");
        let follow_up_plan = agent_task_lifecycle::load_plan(
            result
                .value
                .latest_run_id
                .as_deref()
                .expect("follow-up run id"),
        )
        .unwrap();
        assert_eq!(
            follow_up_plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"]["kind"],
            "candidate_adoption_review"
        );
        assert_eq!(
            follow_up_plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"]
                ["max_provider_executions"],
            2
        );
        assert_eq!(
            follow_up_plan.options.execution_budget,
            crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 0)
        );
        assert!(backend.committed && backend.pushed && backend.created);
    });
}

#[test]
fn legacy_adoption_budget_failure_reenters_once_through_review_authority() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fixture = CandidateAdoptionFixture::new("cook-9847-legacy-budget", 2, 0, true, None);
        agent_task_lifecycle::start_candidate_adoption(
            &fixture.run_id,
            &fixture.candidate,
            "openai/gpt-5.6-terra",
            "historical gate",
        )
        .unwrap();
        agent_task_lifecycle::finish_candidate_adoption(
            &fixture.run_id,
            Some("candidate remediation budget exhausted".to_string()),
        )
        .unwrap();
        agent_task_lifecycle::record_candidate_adoption_result(
            &fixture.run_id,
            serde_json::json!({
                "status": "execution_budget_exhausted",
                "stop_reason": "provider execution stopped because max_provider_executions was exhausted",
            }),
        )
        .unwrap();
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt(|_| Ok(None), ReviewFormOnlyExecutor, &mut backend)
            .expect("legacy budget failure re-enters through repaired review authority");

        assert_eq!(
            result.value.status, "green_no_finalize",
            "continuation failure: {:#?}",
            result.value.failure_context,
        );
        let follow_up = result.value.latest_run_id.as_deref().unwrap();
        let follow_up_plan = agent_task_lifecycle::load_plan(follow_up).unwrap();
        assert_eq!(
            follow_up_plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"]["kind"],
            "candidate_adoption_review"
        );
        let record = agent_task_lifecycle::status(&fixture.run_id).unwrap();
        let replacements = record.metadata["candidate_adoption_replacements"]
            .as_array()
            .expect("legacy terminal adoption retained for audit");
        assert_eq!(replacements.len(), 1);
        assert_eq!(
            replacements[0]["result"]["status"],
            "execution_budget_exhausted"
        );
    });
}

#[test]
fn adoption_review_allowance_is_terminal_and_replay_does_not_dispatch() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fixture = CandidateAdoptionFixture::new("cook-9575-budget", 2, 0, true, None);
        let mut backend = CaptureBackend::default();

        let first = fixture
            .adopt(|_| Ok(None), ReviewFormOnlyExecutor, &mut backend)
            .expect("adoption review consumes its bounded allowance");
        let replay = fixture
            .adopt(
                |_| panic!("terminal adoption must not reconstruct a dispatcher"),
                UnusedExecutor,
                &mut backend,
            )
            .expect("identical adoption replays its terminal result");

        assert_eq!(first.exit_code, 0);
        assert_eq!(first.value.status, "green_no_finalize");
        assert_eq!(replay.exit_code, 0);
        assert_eq!(replay.value.status, "green_no_finalize");
        assert!(!backend.created);
        let record = agent_task_lifecycle::status(&fixture.run_id).unwrap();
        assert_eq!(record.candidate_adoption.unwrap().state, "completed");
        let attempts = agent_task_lifecycle::cook_index(&fixture.cook_id)
            .unwrap()
            .attempts;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].run_id, fixture.run_id);
        assert_eq!(attempts[1].run_id, first.value.latest_run_id.unwrap());
    });
}

#[test]
fn adoption_of_attempt_n_appends_n_plus_one_and_resumes_that_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut fixture = CandidateAdoptionFixture::new("cook-9575-n", 4, 1, true, None);
        fixture.append_adoptable_attempt(2);
        fixture.append_adoptable_attempt(3);
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt(|_| Ok(None), ReviewFormOnlyExecutor, &mut backend)
            .expect("attempt N adoption resumes through its form-only retry");

        assert_eq!(
            result.value.status, "green_no_finalize",
            "unexpected stop reason: {:?}",
            result.value.stop_reason
        );
        assert_eq!(result.value.attempts[0].attempt, 3);
        assert_eq!(result.value.attempts[1].attempt, 4);
        assert!(result.value.attempts[1]
            .run_id
            .starts_with("cook-9575-n-attempt-4"));
        let recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(recipe.attempts.len(), 4);
        assert_eq!(recipe.attempts[3].attempt, 4);
        assert_eq!(recipe.attempts[3].run_id, result.value.attempts[1].run_id);
    });
}

#[test]
fn adoption_of_historical_attempt_appends_after_the_latest_recipe_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut fixture = CandidateAdoptionFixture::new("cook-9575-historical", 3, 1, true, None);
        let historical_run_id = fixture.run_id.clone();
        fixture.append_adoptable_attempt(2);
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt_run(
                &historical_run_id,
                |_| Ok(None),
                ReviewFormOnlyExecutor,
                &mut backend,
            )
            .expect("historical attempt adoption allocates after the durable recipe tail");

        assert_eq!(result.value.status, "green_no_finalize");
        assert_eq!(result.value.attempts[0].attempt, 1);
        assert_eq!(result.value.attempts[1].attempt, 3);
        assert!(result.value.attempts[1]
            .run_id
            .starts_with("cook-9575-historical-attempt-3"));
        let recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(recipe.attempts.len(), 3);
        assert_eq!(recipe.attempts[2].attempt, 3);
        assert_eq!(recipe.attempts[2].run_id, result.value.attempts[1].run_id);
    });
}

#[test]
fn adoption_replays_provider_discovery_failure_in_the_same_recipe_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let dispatcher = Arc::new(ProviderDiscoveryReplayDispatcher {
            dispatches: AtomicUsize::new(0),
            provider_missing_before_success: 1,
        });
        let fixture = CandidateAdoptionFixture::new(
            "cook-9575-provider-replay",
            2,
            1,
            true,
            Some(dispatcher.clone()),
        );
        let mut backend = CaptureBackend::default();

        fixture
            .adopt(
                |_| Ok(Some(dispatcher.clone())),
                UnusedExecutor,
                &mut backend,
            )
            .expect_err("runner provider discovery failure interrupts adoption");
        let failed_recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(failed_recipe.attempts.len(), 2);
        let failed_run_id = failed_recipe.attempts[1].run_id.clone();
        assert!(retryable_provider_discovery_failure(&failed_run_id));
        let continuation_plan = agent_task_lifecycle::load_plan(&failed_run_id)
            .expect("provider replay persists its baseline-bound continuation plan");
        let baseline_root = continuation_plan.tasks[0]
            .workspace
            .root
            .clone()
            .expect("continuation plan has an authenticated baseline root");
        let mut reconstructed = super::super::reconstruct_adoption_options(&failed_recipe)
            .expect("reconstruct adoption policy");
        super::rebind_baseline_continuation_workspace(&mut reconstructed, &continuation_plan)
            .expect("restore the active task-worktree identity");
        assert_eq!(reconstructed.to_worktree, fixture.options.to_worktree);
        assert_eq!(
            std::fs::canonicalize(reconstructed.source_worktree_path.unwrap()).unwrap(),
            std::fs::canonicalize(&fixture.target).unwrap(),
            "continuation reconstruction resolves the recipe handle to the active target worktree"
        );
        assert_ne!(
            baseline_root,
            fixture.target.display().to_string(),
            "the persisted baseline is evidence, not the continuation workspace"
        );
        agent_task_lifecycle::rewrite_record_for_test(&fixture.run_id, |record| {
            record
                .candidate_adoption
                .as_mut()
                .expect("durable adoption owner")
                .owner_pid = u32::MAX;
        })
        .unwrap();
        assert_eq!(
            agent_task_lifecycle::status(&fixture.run_id)
                .unwrap()
                .candidate_adoption
                .unwrap()
                .state,
            "interrupted"
        );

        let result = fixture
            .adopt(
                |_| Ok(Some(dispatcher.clone())),
                UnusedExecutor,
                &mut backend,
            )
            .expect("provider discovery repair replays the durable attempt");

        assert_eq!(
            result.value.status, "green_no_finalize",
            "continuation failure: {:#?}",
            result.value.failure_context,
        );
        assert_eq!(dispatcher.dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(result.value.attempts[1].attempt, 2);
        assert_ne!(result.value.attempts[1].run_id, failed_run_id);
        let replayed_recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(replayed_recipe.attempts.len(), 3);
        assert_eq!(replayed_recipe.attempts[1].run_id, failed_run_id);
        assert_eq!(replayed_recipe.attempts[2].attempt, 2);
        assert_eq!(
            replayed_recipe.attempts[2].run_id,
            result.value.attempts[1].run_id
        );
        let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&fixture.cook_id, 3);
        super::super::record_recipe_attempt(
            &fixture.cook_id,
            3,
            &next_run_id,
            &replayed_recipe.attempts[2].plan,
        )
        .expect("replacement entries do not consume the next logical attempt number");
        let extended_recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(extended_recipe.attempts[3].attempt, 3);
    });
}

#[test]
fn repeated_provider_discovery_failures_exhaust_the_adoption_review_allowance() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let dispatcher = Arc::new(ProviderDiscoveryReplayDispatcher {
            dispatches: AtomicUsize::new(0),
            provider_missing_before_success: usize::MAX,
        });
        let fixture = CandidateAdoptionFixture::new(
            "cook-9575-provider-replay-budget",
            2,
            1,
            true,
            Some(dispatcher.clone()),
        );
        let mut backend = CaptureBackend::default();

        for expected_dispatches in 1..=2 {
            fixture
                .adopt(
                    |_| Ok(Some(dispatcher.clone())),
                    UnusedExecutor,
                    &mut backend,
                )
                .expect_err("provider discovery failure interrupts adoption");
            assert_eq!(
                dispatcher.dispatches.load(Ordering::SeqCst),
                expected_dispatches
            );
            agent_task_lifecycle::rewrite_record_for_test(&fixture.run_id, |record| {
                record
                    .candidate_adoption
                    .as_mut()
                    .expect("durable adoption owner")
                    .owner_pid = u32::MAX;
            })
            .unwrap();
            agent_task_lifecycle::status(&fixture.run_id).unwrap();
        }

        let exhausted = fixture
            .adopt(
                |_| Ok(Some(dispatcher.clone())),
                UnusedExecutor,
                &mut backend,
            )
            .expect("budget exhaustion is a durable Cook result");
        assert_eq!(exhausted.value.status, "execution_budget_exhausted");
        assert_eq!(dispatcher.dispatches.load(Ordering::SeqCst), 2);
        let recipe = super::super::load_recipe(&fixture.cook_id).unwrap();
        assert_eq!(recipe.attempts.len(), 3);
        assert_eq!(recipe.attempts[1].attempt, 2);
        assert_eq!(recipe.attempts[2].attempt, 2);
    });
}

#[test]
fn detached_adoption_follow_up_records_before_dispatch_then_finalizes_once_without_redispatch() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let dispatcher = Arc::new(RecordingDetachedAttemptDispatcher {
            dispatches: Arc::clone(&dispatches),
        });
        let fixture = CandidateAdoptionFixture::new(
            "cook-9575-detached",
            2,
            1,
            false,
            Some(dispatcher.clone()),
        );
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(fixture.run_id.clone()),
            ..Default::default()
        };

        let first = fixture
            .adopt(
                |_| Ok(Some(dispatcher.clone())),
                UnusedExecutor,
                &mut backend,
            )
            .expect("detached form retry is accepted");
        let follow_up = first
            .value
            .latest_run_id
            .as_deref()
            .expect("detached retry reports its durable run id");
        assert_eq!(first.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        assert!(agent_task_lifecycle::load_plan(follow_up).is_ok());
        assert!(persisted_promotion_for_attempt(follow_up)
            .unwrap()
            .is_some());

        let follow_up_plan = agent_task_lifecycle::load_plan(follow_up).unwrap();
        seed_review_form_aggregate(follow_up, &follow_up_plan);
        let terminal = agent_task_lifecycle::status(follow_up).unwrap();
        assert_eq!(
            terminal.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
        let claim = crate::agent_task_service::claim_continuation()
            .unwrap()
            .expect("terminal detached follow-up queues continuation");
        let mut resumed_backend = CaptureBackend {
            hydrate_run_id: Some(fixture.run_id.clone()),
            ..Default::default()
        };
        let exit_code = super::super::consume_claimed_with_dispatcher(
            claim,
            |_| Ok(Some(dispatcher.clone())),
            |options| {
                run_cook_with_finalizer(options, UnusedExecutor, |options, run_id, promotion| {
                    finalize_cook_pr_with_backend(options, run_id, promotion, &mut resumed_backend)
                })
                .map(|result| result.exit_code)
            },
        )
        .expect("continuation consumes terminal follow-up");

        assert_eq!(exit_code, 0);
        assert!(resumed_backend.created);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        let carried = persisted_promotion_for_attempt(follow_up).unwrap().unwrap();
        assert_eq!(carried.source.run_id.as_deref(), Some(follow_up));
        assert_eq!(
            carried.provenance["cook_follow_up"]["source_run_id"],
            fixture.run_id
        );
        assert!(crate::agent_task_service::claim_continuation()
            .unwrap()
            .is_none());
    });
}

#[test]
fn detached_adoption_follow_up_failure_stays_non_green_and_skips_finalization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let dispatcher = Arc::new(RecordingDetachedAttemptDispatcher {
            dispatches: Arc::clone(&dispatches),
        });
        let fixture = CandidateAdoptionFixture::new(
            "cook-9575-detached-red",
            2,
            1,
            false,
            Some(dispatcher.clone()),
        );
        let mut backend = CaptureBackend::default();
        let first = fixture
            .adopt(
                |_| Ok(Some(dispatcher.clone())),
                UnusedExecutor,
                &mut backend,
            )
            .expect("detached form retry is accepted");
        let follow_up = first
            .value
            .latest_run_id
            .as_deref()
            .expect("detached retry reports its durable run id");
        let mut plan = agent_task_lifecycle::load_plan(follow_up).unwrap();
        plan.tasks[0].instructions = "terminal failure".to_string();
        agent_task_lifecycle::record_run_aggregate(
            follow_up,
            &plan,
            &crate::agent_task_scheduler::AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed,
                totals: crate::agent_task_scheduler::AgentTaskAggregateTotals {
                    failed: 1,
                    ..Default::default()
                },
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        let terminal = agent_task_lifecycle::status(follow_up).unwrap();
        assert_eq!(
            terminal.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert!(crate::agent_task_service::claim_continuation()
            .unwrap()
            .is_none());
        assert!(!backend.created);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn adoption_rejects_missing_or_placeholder_candidate_model() {
    for model in ["", "not recorded", " unknown "] {
        let error = concrete_adoption_ai_model(model)
            .expect_err("adoption model must be a concrete identifier");
        assert_eq!(error.details["field"], "ai_model");
    }
}

#[test]
fn adoption_rejects_aggregate_free_cancelled_runs_without_pre_provider_evidence() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-cancelled-without-evidence";
        let run_id = "cook-adopt-cancelled-without-evidence-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link recipe attempt");
        let cancelled = agent_task_lifecycle::cancel_run(run_id, Some("fixture cancellation"))
            .expect("cancel attempt");
        assert!(cancelled.aggregate_path.is_none());

        let candidate = "a".repeat(40);
        let error = adopt_cook_candidate(cook_id, &candidate)
            .expect_err("cancelled run without recovery evidence is rejected");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
    });
}

#[test]
fn adoption_by_run_id_keeps_the_existing_lifecycle_record() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-existing-run";
        let run_id = "cook-adopt-existing-run-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link cook attempt");

        let (record, recipe) =
            resolve_adoption_target(run_id).expect("adoption resolves existing run");

        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, run_id);
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Queued
        );
    });
}

#[test]
fn adoption_by_cook_id_selects_the_existing_recipe_attempt_record() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-existing-attempt";
        let run_id = "cook-adopt-existing-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link cook attempt");
        agent_task_lifecycle::cancel(run_id).expect("cancel recorded attempt");

        let (record, recipe) =
            resolve_adoption_target(cook_id).expect("adoption resolves recorded cook attempt");

        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, run_id);
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Cancelled
        );
    });
}

#[test]
fn adoption_by_cook_id_uses_the_first_of_repeated_equivalent_recipe_attempts() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-equivalent-attempts";
        let first_run_id = "cook-adopt-equivalent-attempts-1";
        let second_run_id = "cook-adopt-equivalent-attempts-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = first_run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        super::super::record_recipe_attempt(cook_id, 2, second_run_id, &options.initial_plan)
            .expect("persist second recipe attempt");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(first_run_id))
            .expect("persist first lifecycle record");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(second_run_id))
            .expect("persist second lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, first_run_id)
            .expect("index first attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, second_run_id)
            .expect("make later failed attempt the mutable index target");

        let (record, recipe) = resolve_adoption_target(cook_id)
            .expect("equivalent attempts resolve deterministically");

        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, first_run_id);
    });
}

#[test]
fn adoption_by_cook_id_selects_the_latest_substantive_candidate_not_a_newer_empty_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-adopt-substantive-attempt";
        let first_run_id = "cook-adopt-substantive-attempt-1";
        let second_run_id = "cook-adopt-substantive-attempt-2";
        let third_run_id = "cook-adopt-substantive-attempt-3";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = first_run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        super::super::record_recipe_attempt(cook_id, 2, second_run_id, &options.initial_plan)
            .expect("persist second recipe attempt");
        super::super::record_recipe_attempt(cook_id, 3, third_run_id, &options.initial_plan)
            .expect("persist third recipe attempt");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(first_run_id))
            .expect("persist first lifecycle record");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(second_run_id))
            .expect("persist second lifecycle record");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(third_run_id))
            .expect("persist third lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, first_run_id)
            .expect("index first attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, second_run_id)
            .expect("index substantive attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 3, third_run_id)
            .expect("index unavailable later attempt");
        seed_substantive_candidate_aggregate(
            second_run_id,
            &options.initial_plan,
            &temp.path().join("second.patch"),
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        seed_substantive_candidate_aggregate(
            third_run_id,
            &options.initial_plan,
            &temp.path().join("third.patch"),
            "this is deliberately nonempty but not a unified diff\n",
        );

        let selection = agent_task_lifecycle::select_cook_candidate(cook_id)
            .expect("select persisted candidate");
        assert_eq!(selection.run_id, second_run_id);
        assert_eq!(selection.reason, "latest_substantive_candidate_pointer");
        assert!(selection.skipped_newer_run_ids.is_empty());
        assert_eq!(selection.latest_attempt_run_id, third_run_id);
        let (_, source_path) = super::super::promotion_source(cook_id)
            .expect("promotion reads the older readable candidate");
        assert_eq!(
            source_path,
            Some(
                agent_task_lifecycle::aggregate_source(second_run_id)
                    .unwrap()
                    .1
            )
        );

        let (record, _) = resolve_adoption_target(cook_id).expect("adoption follows selection");
        assert_eq!(record.run_id, second_run_id);
        assert_eq!(
            super::super::resolve_cook_continuation_run_id(cook_id)
                .expect("continuation follows selection"),
            second_run_id
        );
        let recipe = super::super::load_recipe(cook_id).expect("load recipe");
        assert_eq!(
            resumable_cook_run_id(&recipe, cook_id, first_run_id, 1, false),
            Some(second_run_id.to_string()),
            "continuation must keep the selected substantive attempt, not the latest alias"
        );
        std::fs::remove_file(
            homeboy_core::paths::homeboy_data()
                .unwrap()
                .join("agent-task-cooks")
                .join(cook_id)
                .join("index.json"),
        )
        .expect("remove legacy-missing Cook index fixture");
        assert_eq!(
            super::super::resolve_cook_continuation_run_id(cook_id)
                .expect("recipe attempts preserve substantive selection without an index"),
            second_run_id
        );
    });
}

#[test]
fn record_cook_attempt_recovers_an_aggregate_completed_before_attempt_registration() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-aggregate-before-registration";
        let run_id = "cook-aggregate-before-registration-1";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("persist lifecycle record");
        seed_substantive_candidate_aggregate(
            run_id,
            &options.initial_plan,
            &temp.path().join("candidate.patch"),
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );

        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
            .expect("register completed attempt");

        let pointer = agent_task_lifecycle::cook_index(cook_id)
            .expect("read index")
            .latest_substantive_candidate
            .expect("completed aggregate becomes the durable candidate");
        assert_eq!(pointer.run_id, run_id);
        assert_eq!(pointer.attempt, 1);
    });
}

#[test]
fn legacy_substantive_candidate_selection_is_explicitly_incomplete_after_64_newer_skips() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-substantive-bounded-window";
        let attempts = (1..=65)
            .map(
                |attempt| crate::agent_task_lifecycle::AgentTaskCookIndexAttempt {
                    attempt,
                    run_id: format!("{cook_id}-{attempt}"),
                    recorded_at: "2026-01-01T00:00:00Z".to_string(),
                },
            )
            .collect();
        agent_task_lifecycle::replace_cook_index_for_test(
            &crate::agent_task_lifecycle::AgentTaskCookIndex {
                schema: crate::agent_task_lifecycle::schemas::COOK_INDEX.to_string(),
                cook_id: cook_id.to_string(),
                latest_run_id: format!("{cook_id}-65"),
                latest_substantive_candidate: None,
                attempts,
            },
        )
        .expect("persist complete cook index once");
        let selection = agent_task_lifecycle::select_cook_candidate(cook_id)
            .expect("bounded selection returns an explicit result");
        assert!(selection.incomplete);
        assert_eq!(
            selection.reason,
            "selection_window_exhausted_without_promotable_candidate"
        );
        assert!(selection.run_id.is_empty());
        assert_eq!(selection.latest_attempt_run_id, format!("{cook_id}-65"));
        assert_eq!(selection.skipped_newer_run_ids.len(), 64);
        assert_eq!(selection.skipped_newer_attempts.len(), 64);
    });
}

#[test]
fn cook_index_serialization_remains_compatible_with_indexes_without_a_candidate_pointer() {
    let legacy = serde_json::json!({
        "schema": "homeboy/agent-task-cook-index/v1",
        "cook_id": "legacy-cook",
        "latest_run_id": "legacy-run",
        "attempts": [{"attempt": 1, "run_id": "legacy-run", "recorded_at": "2026-01-01T00:00:00Z"}]
    });
    let index: crate::agent_task_lifecycle::AgentTaskCookIndex =
        serde_json::from_value(legacy).expect("read legacy Cook index");
    assert!(index.latest_substantive_candidate.is_none());
    let serialized = serde_json::to_value(index).expect("serialize Cook index");
    assert!(serialized.get("latest_substantive_candidate").is_none());
}

#[test]
fn cook_report_emits_selected_candidate_provenance_without_redefining_latest_run_id() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-selected-candidate-provenance";
        let selected_run_id = "cook-selected-candidate-provenance-1";
        let latest_run_id = "cook-selected-candidate-provenance-2";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        for (attempt, run_id) in [(1, selected_run_id), (2, latest_run_id)] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id)
                .expect("persist attempt index entry");
        }
        seed_substantive_candidate_aggregate(
            selected_run_id,
            &options.initial_plan,
            &temp.path().join("selected.patch"),
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        seed_substantive_candidate_aggregate(
            latest_run_id,
            &options.initial_plan,
            &temp.path().join("malformed.patch"),
            "nonempty malformed newer artifact\n",
        );
        agent_task_lifecycle::rewrite_record_for_test(selected_run_id, |record| {
            record.metadata["latest_promotion"] = serde_json::json!({
                "patch_artifact": { "sha256": "candidate-sha" },
                "to_worktree": "fixture@destination",
                "provenance": { "candidate": "candidate-fingerprint" }
            });
        })
        .expect("persist promotion provenance");

        let report = cook_report(
            cook_id.to_string(),
            "completed",
            Vec::new(),
            None,
            None,
            0,
            Some(latest_run_id),
        );
        assert_eq!(report.value.latest_run_id.as_deref(), Some(latest_run_id));
        let provenance = report
            .value
            .selected_candidate
            .expect("selected candidate provenance");
        assert_eq!(provenance["latest_attempt_run_id"], latest_run_id);
        assert_eq!(provenance["run_id"], selected_run_id);
        assert_eq!(
            provenance["selected_task_id"],
            options.initial_plan.tasks[0].task_id
        );
        assert_eq!(provenance["selected_artifact_id"], "candidate");
        assert_eq!(
            provenance["skipped_newer_attempts"][0]["run_id"],
            latest_run_id
        );
        assert_eq!(provenance["applied_promotion"]["identity"], "candidate-sha");
        assert_eq!(
            provenance["applied_promotion"]["destination"],
            "fixture@destination"
        );
        assert_eq!(
            provenance["applied_promotion"]["fingerprint"],
            "candidate-fingerprint"
        );
    });
}

#[test]
fn resume_promoted_patch_guidance_keeps_the_exhausted_zero_byte_attempt_as_latest() {
    // #10156 acceptance: a gate-feedback retry can exhaust after producing no
    // patch without obscuring the earlier applied candidate or its review path.
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-10156-substantive-recovery";
        let candidate_run_id = "cook-10156-substantive-recovery-attempt-1";
        let exhausted_run_id = "cook-10156-substantive-recovery-attempt-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = candidate_run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        super::super::record_recipe_attempt(cook_id, 2, exhausted_run_id, &options.initial_plan)
            .expect("persist exhausted retry recipe entry");
        for (attempt, run_id) in [(1, candidate_run_id), (2, exhausted_run_id)] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id)
                .expect("persist attempt index entry");
        }

        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        seed_substantive_candidate_aggregate(
            candidate_run_id,
            &options.initial_plan,
            &temp.path().join("candidate.patch"),
            patch,
        );
        // The follow-up executor ran, but emitted a zero-byte patch before its
        // execution allowance was exhausted. It remains chronologically latest.
        seed_substantive_candidate_aggregate(
            exhausted_run_id,
            &options.initial_plan,
            &temp.path().join("zero-byte.patch"),
            "",
        );
        agent_task_lifecycle::rewrite_record_for_test(candidate_run_id, |record| {
            record.metadata["review_form"] = test_review_form_outputs();
            record.metadata["infrastructure"] = serde_json::json!({
                "runner_id": "fixture-lab",
                "provider_execution": 1,
            });
            record.metadata["latest_promotion"] = serde_json::json!({
                "patch_artifact": { "sha256": homeboy_engine_primitives::content_hash::sha256_hex(patch.as_bytes()) },
                "to_worktree": "fixture@destination",
                "command_evidence": [{
                    "command": ["git", "apply", "--reverse", "--check", "-"],
                    "exit_code": 0,
                }],
                "provenance": {
                    "candidate": { "kind": "git", "fingerprint": { "head": "candidate-head", "tree": "candidate-tree" } },
                    "resumed_post_apply_promotion": true,
                },
            });
        })
        .expect("persist candidate promotion and metadata");
        agent_task_lifecycle::rewrite_record_for_test(exhausted_run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(2);
            record.metadata["terminal"] = serde_json::json!("execution_budget_exhausted");
        })
        .expect("persist exhausted retry terminal state");
        let latest_empty_run_id = format!("{cook_id}-attempt-66");
        for attempt in 3..=66 {
            let run_id = format!("{cook_id}-attempt-{attempt}");
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id))
                .expect("persist empty follow-up lifecycle attempt");
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, &run_id)
                .expect("register empty follow-up attempt");
        }
        let index = agent_task_lifecycle::cook_index(cook_id).expect("read durable index");
        assert_eq!(index.latest_run_id, latest_empty_run_id);
        let pointer = index
            .latest_substantive_candidate
            .expect("first completed candidate remains durable authority");
        assert_eq!(pointer.run_id, candidate_run_id);
        assert_eq!(pointer.attempt, 1);

        let report = cook_report(
            cook_id.to_string(),
            "execution_budget_exhausted",
            Vec::new(),
            None,
            Some(
                "provider execution stopped because max_provider_executions was exhausted"
                    .to_string(),
            ),
            1,
            Some(&latest_empty_run_id),
        );

        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(latest_empty_run_id.as_str())
        );
        let selected = report
            .value
            .selected_candidate
            .expect("selected candidate provenance");
        assert_eq!(selected["run_id"], candidate_run_id);
        assert_eq!(selected["latest_attempt_run_id"], latest_empty_run_id);
        assert_eq!(
            selected["selected_task_id"],
            options.initial_plan.tasks[0].task_id
        );
        assert_eq!(selected["selected_artifact_id"], "candidate");
        let context = report.value.failure_context.expect("recovery context");
        assert_eq!(context.latest_run_id, latest_empty_run_id);
        assert_eq!(context.selected_run_id.as_deref(), Some(candidate_run_id));
        let actions = context.legal_actions;
        assert!(actions
            .iter()
            .all(|action| action.action != "promote_selected_candidate"));
        let review_action = actions
            .iter()
            .find(|action| action.action == "review_selected_candidate")
            .expect("selected-candidate review action");
        assert_eq!(
            review_action.command,
            format!(
                "homeboy agent-task review {candidate_run_id} --to-worktree fixture@destination"
            )
        );
        assert!(actions.iter().any(|action| {
            action.command == format!("homeboy agent-task finalize-pr --recover {candidate_run_id}")
        }));

        agent_task_lifecycle::rewrite_record_for_test(candidate_run_id, |record| {
            record.metadata["latest_promotion"]["command_evidence"][0]["exit_code"] =
                serde_json::json!(1);
        })
        .expect("remove destination proof");
        let unproven = cook_report(
            cook_id.to_string(),
            "execution_budget_exhausted",
            Vec::new(),
            None,
            None,
            1,
            Some(&latest_empty_run_id),
        )
        .value
        .failure_context
        .expect("unproven destination context");
        assert!(unproven.legal_actions.iter().any(|action| {
            action.command == format!(
                "homeboy agent-task promote {candidate_run_id} --to-worktree fixture@destination --task-id provider --artifact-id candidate"
            )
        }));

        let selected_record = agent_task_lifecycle::status(candidate_run_id).unwrap();
        let promotion = &selected_record.metadata["latest_promotion"];
        assert_eq!(
            promotion["command_evidence"][0]["command"],
            serde_json::json!(["git", "apply", "--reverse", "--check", "-"])
        );
        assert_eq!(
            promotion["provenance"]["candidate"]["fingerprint"]["head"],
            "candidate-head"
        );
        assert_eq!(
            promotion["provenance"]["resumed_post_apply_promotion"],
            true
        );
        assert_eq!(
            promotion["command_evidence"].as_array().unwrap().len(),
            1,
            "recovery verifies the destination without reapplying the patch"
        );
        assert_eq!(
            selected_record.metadata["review_form"],
            test_review_form_outputs()
        );
        assert_eq!(
            selected_record.metadata["infrastructure"]["runner_id"],
            "fixture-lab"
        );
    });
}

#[test]
fn substantive_candidate_selection_breaks_duplicate_attempt_ties_by_run_id() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-substantive-tie-break";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        for run_id in [
            "cook-substantive-tie-break-a",
            "cook-substantive-tie-break-b",
        ] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, 7, run_id)
                .expect("persist duplicate attempt index entry");
            seed_substantive_candidate_aggregate(
                run_id,
                &options.initial_plan,
                &temp.path().join(format!("{run_id}.patch")),
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            );
        }

        let selection = agent_task_lifecycle::select_cook_candidate(cook_id)
            .expect("select deterministic duplicate-attempt winner");
        assert_eq!(selection.run_id, "cook-substantive-tie-break-b");
        assert_eq!(selection.attempt, 7);
        assert_eq!(selection.reason, "latest_substantive_candidate_pointer");
    });
}

#[test]
fn first_attempt_named_as_cook_id_reads_its_own_substantive_aggregate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let cook_id = "cook-attempt-id-collision";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        for (attempt, run_id) in [(1, cook_id), (2, "cook-attempt-id-collision-2")] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id).unwrap();
        }
        seed_substantive_candidate_aggregate(
            cook_id,
            &options.initial_plan,
            &temp.path().join("first.patch"),
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
        );
        seed_missing_review_form_aggregate("cook-attempt-id-collision-2", &options.initial_plan);
        assert_eq!(
            super::super::selected_candidate_task_id(cook_id).unwrap(),
            Some(options.initial_plan.tasks[0].task_id.clone())
        );
        assert_eq!(
            agent_task_lifecycle::select_cook_candidate(cook_id)
                .unwrap()
                .run_id,
            cook_id
        );
    });
}

#[test]
fn large_cook_index_selects_the_deterministic_top_64_window() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-large-index";
        let attempts = (1..=10_000)
            .map(
                |attempt| crate::agent_task_lifecycle::AgentTaskCookIndexAttempt {
                    attempt,
                    run_id: format!("{cook_id}-{attempt}"),
                    recorded_at: "2026-01-01T00:00:00Z".to_string(),
                },
            )
            .collect();
        agent_task_lifecycle::replace_cook_index_for_test(
            &crate::agent_task_lifecycle::AgentTaskCookIndex {
                schema: crate::agent_task_lifecycle::schemas::COOK_INDEX.to_string(),
                cook_id: cook_id.to_string(),
                latest_run_id: format!("{cook_id}-10000"),
                latest_substantive_candidate: None,
                attempts,
            },
        )
        .unwrap();
        let selection = agent_task_lifecycle::select_cook_candidate(cook_id).unwrap();
        assert!(selection.incomplete);
        assert_eq!(selection.latest_attempt_run_id, format!("{cook_id}-10000"));
        assert_eq!(selection.skipped_newer_run_ids.len(), 64);
        assert_eq!(
            selection.skipped_newer_run_ids[0],
            format!("{cook_id}-10000")
        );
        assert_eq!(
            selection.skipped_newer_run_ids[63],
            format!("{cook_id}-9937")
        );
    });
}

#[test]
fn adoption_by_cook_id_rejects_conflicting_recipe_attempts_with_explicit_choices() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-conflicting-attempts";
        let first_run_id = "cook-adopt-conflicting-attempts-1";
        let second_run_id = "cook-adopt-conflicting-attempts-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = first_run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        let mut conflicting_plan = options.initial_plan.clone();
        conflicting_plan.plan_id = "conflicting-plan".to_string();
        super::super::record_recipe_attempt(cook_id, 2, second_run_id, &conflicting_plan)
            .expect("persist conflicting second recipe attempt");
        agent_task_lifecycle::submit_plan(&conflicting_plan, Some(second_run_id))
            .expect("persist exact conflicting attempt record without Cook metadata");

        let error = resolve_adoption_target(cook_id)
            .expect_err("conflicting recipe adoption requires an explicit run id");

        assert_eq!(error.details["field"], "cook_recipe.attempts");
        assert!(error.message.contains(first_run_id));
        assert!(error.message.contains(second_run_id));
        assert!(error
            .message
            .contains(&format!("homeboy agent-task adopt {cook_id} --attempt 1")));

        let (record, recipe) = resolve_adoption_target(second_run_id)
            .expect("an exact existing attempt run id selects its owning recipe");
        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, second_run_id);
    });
}

#[test]
fn adoption_attempt_selector_disambiguates_a_first_run_id_equal_to_its_cook_id() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-attempt-id-collision";
        let second_run_id = "cook-adopt-attempt-id-collision-attempt-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = cook_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        let mut conflicting_plan = options.initial_plan.clone();
        conflicting_plan.plan_id = "attempt-two-policy".to_string();
        super::super::record_recipe_attempt(cook_id, 2, second_run_id, &conflicting_plan)
            .expect("persist conflicting second recipe attempt");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(cook_id))
            .expect("persist first lifecycle record");

        let error = resolve_adoption_target(cook_id)
            .expect_err("conflicting attempts require an explicit selector");
        assert!(error.message.contains("--attempt 1"));
        assert!(error.message.contains("plan attempt-two-policy"));

        let (record, recipe) = resolve_adoption_target_with_attempt(cook_id, Some(1))
            .expect("attempt selector resolves the first attempt despite the ID collision");
        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, cook_id);
    });
}

#[test]
fn adoption_ambiguity_describes_policy_choices_without_sensitive_config() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-adopt-policy-summary";
        let first_run_id = "cook-adopt-policy-summary-attempt-1";
        let second_run_id = "cook-adopt-policy-summary-attempt-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = first_run_id.to_string();
        options.to_worktree = "homeboy@policy-destination".to_string();
        options.base = "release".to_string();
        options.head = Some("fix/policy-summary".to_string());
        options.task_base_sha = Some("base-sha".to_string());
        options.no_finalize = false;
        options.protected_branches = vec!["release".to_string()];
        options.gates.verify = vec!["echo super-secret-gate-value".to_string()];
        options.gates.private_verify = vec!["private-check".to_string()];
        options.initial_plan.tasks[0].executor.backend = "provider-one".to_string();
        options.initial_plan.tasks[0].executor.selector = Some("primary".to_string());
        options.initial_plan.tasks[0].executor.model = Some("model-one".to_string());
        options.initial_plan.tasks[0].executor.config = serde_json::json!({
            "api_token": "super-secret-config-value"
        });
        options.initial_plan.tasks[0].policy.apply = "review".to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");

        let mut second_plan = options.initial_plan.clone();
        second_plan.plan_id = "attempt-two-policy".to_string();
        second_plan.tasks[0].executor.backend = "provider-two".to_string();
        second_plan.tasks[0].executor.selector = Some("fallback".to_string());
        second_plan.tasks[0].executor.model = Some("model-two".to_string());
        second_plan.tasks[0].policy.apply = "publish".to_string();
        super::super::record_recipe_attempt(cook_id, 2, second_run_id, &second_plan)
            .expect("persist policy-different attempt");

        let error = resolve_adoption_target(cook_id).expect_err("policies require selection");
        for expected in [
            "destination=homeboy@policy-destination",
            "base=release",
            "head=fix/policy-summary",
            "task-base=base-sha",
            "gates=public:1/private:1",
            "provider/model=provider-one/primary@model-one",
            "provider/model=provider-two/fallback@model-two",
            "review/publication=review-ready/protected:1",
            "task-policy=workspace/artifacts_only/review",
            "task-policy=workspace/artifacts_only/publish",
            "--attempt 1",
        ] {
            assert!(
                error.message.contains(expected),
                "missing {expected}: {error}"
            );
        }
        assert!(!error.message.contains("super-secret-gate-value"));
        assert!(!error.message.contains("super-secret-config-value"));
        assert!(!error.message.contains("api_token"));
    });
}

#[test]
fn adoption_rejects_unknown_run_or_cook_ids() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let error = resolve_adoption_target("unknown-adoption-target")
            .expect_err("unknown adoption target fails closed");

        assert_eq!(error.details["field"], "run_or_cook_id");
        assert!(error
            .message
            .contains("unknown agent-task run or durable cook id"));
    });
}

#[derive(Default)]
struct CaptureBackend {
    body: String,
    committed: bool,
    pushed: bool,
    created: bool,
    hydrate_run_id: Option<String>,
    hydrate_gate_proof_run_id: Option<String>,
    synthetic_gate_proof: Option<AgentTaskPromotionReport>,
}

impl AgentTaskPrFinalizationBackend for CaptureBackend {
    fn hydrate_run(&mut self, run_id: &str) -> Result<RunLifecycleRecord> {
        if self.hydrate_run_id.is_some() {
            return RealAgentTaskPrFinalizationBackend.hydrate_run(run_id);
        }
        Ok(RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Succeeded,
                started_at: None,
                finished_at: Some("2026-07-14T00:00:00Z".to_string()),
                updated_at: None,
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task".to_string(),
                backend: "opencode".to_string(),
                state: ProviderRuntimeState::Succeeded,
                stream_uri: None,
                external_runtime_ids: Vec::new(),
                metadata: serde_json::json!({"model": "openai/gpt-5.6-terra"}),
            }],
            ..RunLifecycleRecord::default()
        })
    }
    fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
        if self.hydrate_run_id.is_some() || self.hydrate_gate_proof_run_id.is_some() {
            return RealAgentTaskPrFinalizationBackend.hydrate_gate_proof(run_id);
        }
        if let Some(mut promotion) = self.synthetic_gate_proof.clone() {
            promotion.source.run_id = Some(run_id.to_string());
            if let Ok(Some(persisted)) = persisted_promotion_for_attempt(run_id) {
                if let Some(follow_up) = persisted.provenance.get("cook_follow_up") {
                    promotion.provenance["cook_follow_up"] = follow_up.clone();
                }
            }
            return Ok(AgentTaskPrDurableGateProof {
                run_id: run_id.to_string(),
                promotion,
            });
        }
        Ok(AgentTaskPrDurableGateProof {
            run_id: run_id.to_string(),
            promotion: promotion(run_id),
        })
    }
    fn current_branch(&mut self, _path: &str) -> Result<String> {
        Ok("fix/8058".to_string())
    }
    fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
        Ok(vec!["src/lib.rs".to_string()])
    }
    fn validate_publication_identity(
        &mut self,
        _path: &str,
    ) -> Result<homeboy_core::git::GitIdentityProof> {
        Ok(homeboy_core::git::GitIdentityProof {
            host: "git.example.test".to_string(),
            name: "Homeboy Bot".to_string(),
            email: "bot@example.test".to_string(),
            committer_name: "Homeboy Bot".to_string(),
            committer_email: "bot@example.test".to_string(),
            commit_sha: None,
            scope: "repository_local".to_string(),
        })
    }
    fn validate_committed_publication_identity(
        &mut self,
        _path: &str,
        expected: Option<&homeboy_core::git::GitIdentityProof>,
    ) -> Result<homeboy_core::git::GitIdentityProof> {
        let mut proof = expected
            .cloned()
            .unwrap_or(homeboy_core::git::GitIdentityProof {
                host: "git.example.test".to_string(),
                name: "Homeboy Bot".to_string(),
                email: "bot@example.test".to_string(),
                committer_name: "Homeboy Bot".to_string(),
                committer_email: "bot@example.test".to_string(),
                commit_sha: None,
                scope: "commit_host_policy".to_string(),
            });
        proof.commit_sha = Some("candidate-sha".to_string());
        proof.scope = "commit_host_policy".to_string();
        Ok(proof)
    }
    fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
        self.committed = true;
        Ok(())
    }
    fn push_branch(
        &mut self,
        _path: &str,
        _commit_sha: &str,
        head: &str,
    ) -> Result<AgentTaskPublicationGitTracking> {
        self.pushed = true;
        Ok(AgentTaskPublicationGitTracking {
            local_branch: head.to_string(),
            remote: "origin".to_string(),
            upstream_ref: format!("refs/remotes/origin/{head}"),
            verified_remote_sha: "candidate-sha".to_string(),
        })
    }
    fn find_open_pr(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        Ok(None)
    }
    fn create_pr(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
        _title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        self.created = true;
        self.body = body.to_string();
        Ok(AgentTaskPrRef {
            number: 8058,
            url: "https://github.com/Extra-Chill/homeboy/pull/8058".to_string(),
        })
    }
    fn update_pr(
        &mut self,
        _path: &str,
        _number: u64,
        _title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        self.body = body.to_string();
        unreachable!("test creates a PR")
    }
    fn verify_publication_binding(
        &mut self,
        _path: &str,
        _base: &str,
        _head: &str,
        candidate_sha: &str,
        changed_files: &[String],
        _pr: &AgentTaskPrRef,
    ) -> Result<AgentTaskPublicationBinding> {
        Ok(AgentTaskPublicationBinding {
            candidate_sha: candidate_sha.to_string(),
            candidate_tree: "candidate-tree".to_string(),
            remote_sha: candidate_sha.to_string(),
            pr_head_sha: candidate_sha.to_string(),
            repository: "Extra-Chill/homeboy".to_string(),
            head_repository: "Extra-Chill/homeboy".to_string(),
            changed_files: changed_files.to_vec(),
        })
    }
}

fn promotion(run_id: &str) -> AgentTaskPromotionReport {
    serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": {"kind": "aggregate", "task_id": "task", "run_id": run_id},
            "to_worktree": "homeboy@8058",
            "target": {"worktree": "homeboy@8058", "path": "/repo"},
            "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
            "changed_files": ["src/lib.rs"],
            "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-lc", "cargo test --locked agent_task_promotion --lib"], "exit_code": 0, "candidate_checkout": {"schema": "homeboy/agent-task-gate-candidate-checkout/v1", "commit": "candidate", "tree": "candidate-tree", "candidate_sha256": "candidate-sha"}}],
            "gate_results": [{"id": "gate", "name": "cargo test --locked agent_task_promotion --lib", "kind": "command", "status": "passed"}],
            "operator_notification": {"status": "completed", "message": "complete"},
            "verified_base": {"base": "main", "sha": "verified-base"},
            "provenance": {"worktree_path": "/repo", "candidate_checkout": {"schema": "homeboy/agent-task-gate-candidate-checkout/v1", "commit": "candidate", "tree": "candidate-tree", "candidate_sha256": "candidate-sha"}}
        })).unwrap()
}

fn tracked_promotion_continuation_options(
    cook_id: &str,
    run_id: &str,
    target: &std::path::Path,
) -> AgentTaskCookServiceOptions {
    let mut options = promotion_claim_options(cook_id, run_id);
    options.to_worktree = target.display().to_string();
    options.source_worktree_path = Some(target.to_path_buf());
    options
}

fn record_tracked_promotion_continuation(
    options: &AgentTaskCookServiceOptions,
    target: &std::path::Path,
) {
    agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
        .unwrap();
    let mut checkpoint = serde_json::to_value(promotion(&options.initial_run_id)).unwrap();
    checkpoint["status"] = serde_json::json!("verification_pending");
    let patch = Command::new("git")
        .args(["diff", "--binary", "--full-index"])
        .current_dir(target)
        .output()
        .expect("capture promoted patch");
    assert!(patch.status.success());
    let patch = String::from_utf8(patch.stdout).expect("patch is UTF-8");
    let patch_path = target
        .parent()
        .expect("candidate parent")
        .join("candidate.patch");
    std::fs::write(&patch_path, &patch).expect("persist patch artifact");
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(target.to_str().expect("target path"))
            .expect("candidate fingerprint");
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } = &candidate
    else {
        panic!("fixture target is a Git worktree");
    };
    checkpoint["to_worktree"] = serde_json::json!(options.to_worktree);
    checkpoint["target"] = serde_json::json!({
        "worktree": options.to_worktree,
        "path": target,
        "branch": "cook-candidate"
    });
    checkpoint["patch_artifact"] = serde_json::json!({
        "id": "patch",
        "kind": "patch",
        "path": patch_path,
        "sha256": format!("{:x}", sha2::Sha256::digest(patch.as_bytes())),
    });
    checkpoint["changed_files"] = serde_json::json!(fingerprint.changed_files);
    checkpoint["provenance"] = serde_json::json!({
        "post_apply": true,
        "candidate": candidate,
        "gate_feedback_baseline": {
            "schema": "homeboy/agent-task-gate-feedback-baseline/v1",
            "current_diff": patch
        }
    });
    agent_task_lifecycle::record_promotion(&options.initial_run_id, checkpoint).unwrap();
}

#[test]
fn fresh_cook_has_no_tracked_promotion_before_lifecycle_materialization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let target = tempfile::tempdir().expect("tempdir");
        let options = tracked_promotion_continuation_options(
            "cook-fresh-promotion",
            "run-fresh-promotion",
            target.path(),
        );

        assert!(!agent_task_lifecycle::run_record_exists(&options.initial_run_id).unwrap());
        assert!(tracked_promotion_continuation(&options).unwrap().is_none());
    });
}

#[test]
fn cook_continuation_authenticates_only_its_exact_tracked_promotion_candidate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("candidate");
        std::fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "--quiet", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(source.join("tracked.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&source)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(&source)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["worktree", "add", "--quiet", "-b", "cook-candidate"])
            .arg(&target)
            .current_dir(&source)
            .status()
            .unwrap()
            .success());
        std::fs::write(target.join("tracked.txt"), "promoted\n").unwrap();

        let mut options = tracked_promotion_continuation_options(
            "cook-tracked-promotion",
            "run-tracked-promotion",
            &target,
        );
        options.to_worktree = "fixture@cook-candidate".to_string();
        record_tracked_promotion_continuation(&options, &target);

        let provider = temp.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                serde_json::json!({ "worktrees": [{
                    "handle": options.to_worktree,
                    "path": target,
                    "branch": "cook-candidate",
                    "safety": { "dirty": true, "unpushed": false, "primary": false },
                }] })
            ),
        )
        .expect("write provider");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).unwrap();
        }
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(
                    homeboy_core::defaults::WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");
        crate::agent_task_candidate_baseline::register();

        let continuation = tracked_promotion_continuation(&options)
            .unwrap()
            .expect("tracked promotion continuation");
        assert_eq!(continuation.baseline["patch_artifact"]["id"], "patch");

        validate_cook_workspace(&options)
            .expect("exact dirty promoted provider destination resumes");

        std::fs::write(target.join("extra.txt"), "unattributed\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("extra drift is rejected");
        assert!(error
            .message
            .contains("promoted candidate baseline could not be verified"));
        std::fs::remove_file(target.join("extra.txt")).unwrap();

        std::fs::write(target.join("tracked.txt"), "changed\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("changed drift is rejected");
        assert!(error
            .message
            .contains("promoted candidate baseline could not be verified"));

        std::fs::write(target.join("tracked.txt"), "base\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("missing candidate is rejected");
        assert!(error
            .message
            .contains("promoted candidate baseline could not be verified"));
    });
}

#[test]
fn adopted_baseline_gate_outcome_is_candidate_bound_and_recovery_safe() {
    let run_id = "cook-10010-attempt-1";
    let command = "cargo test --locked agent_task_promotion --lib";
    let mut accepted = promotion(run_id);
    accepted.status = crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed;
    accepted.deterministic_gates[0].status =
        crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure;
    accepted.deterministic_gates[0].exit_code = 1;
    accepted.deterministic_gates[0].baseline_comparison =
        Some(crate::agent_task_gate::AgentTaskGateBaselineComparison {
            base_ref: "immutable-base".to_string(),
            exit_code: 1,
            failure_fingerprint: "inherited failure".to_string(),
            matches_candidate_failure: true,
        });
    accepted.normalize_gate_outcome();

    assert_eq!(
        accepted.status,
        crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
    );
    assert_eq!(
        accepted.gate_results[0].status,
        homeboy_core::gate::HomeboyGateStatus::Passed
    );
    assert!(accepted.has_visible_passed_gate_for_command(command));

    let mut regression = accepted.clone();
    regression.status = crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed;
    regression.deterministic_gates[0]
        .baseline_comparison
        .as_mut()
        .unwrap()
        .matches_candidate_failure = false;
    regression.normalize_gate_outcome();
    assert_eq!(
        regression.status,
        crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
    );
    assert!(!regression.has_visible_passed_gate_for_command(command));

    let mut wrong_command = accepted.clone();
    wrong_command.deterministic_gates[0].command[2] = "cargo test arbitrary".to_string();
    assert!(!wrong_command.has_visible_passed_gate_for_command(command));

    let mut wrong_candidate = accepted;
    wrong_candidate.deterministic_gates[0]
        .candidate_checkout
        .as_mut()
        .unwrap()
        .commit = "other-candidate".to_string();
    assert!(!wrong_candidate.has_visible_passed_gate_for_command(command));
}

#[test]
fn restarted_cook_alias_and_exact_id_reuse_the_same_persisted_promotion() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-persisted";
        let run_id = "run-persisted";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion(run_id)).unwrap(),
        )
        .unwrap();

        let exact = persisted_promotion_for_attempt(run_id)
            .unwrap()
            .expect("durable promotion");
        let alias = persisted_promotion_for_attempt(cook_id)
            .unwrap()
            .expect("durable promotion through Cook alias");
        let replayed_exact = persisted_promotion_for_attempt(run_id)
            .unwrap()
            .expect("idempotent exact recovery");
        assert_eq!(exact.source.run_id.as_deref(), Some(run_id));
        assert_eq!(alias.source.run_id, exact.source.run_id);
        assert_eq!(alias.patch_artifact.id, exact.patch_artifact.id);
        assert_eq!(replayed_exact.source.run_id, exact.source.run_id);
    });
}

fn promotion_claim_options(cook_id: &str, run_id: &str) -> AgentTaskCookServiceOptions {
    AgentTaskCookServiceOptions {
        cook_id: cook_id.to_string(),
        initial_run_id: run_id.to_string(),
        initial_plan: AgentTaskPlan::new(cook_id, Vec::new()),
        to_worktree: "homeboy@8058".to_string(),
        source_worktree_path: None,
        provider_command: None,
        provider_invocation: None,
        gates: VerifyGateOptions::default(),
        max_attempts: 1,
        no_finalize: true,
        base: "main".to_string(),
        task_base_sha: None,
        head: None,
        title: "Cook".to_string(),
        commit_message: "test".to_string(),
        source_refs: Vec::new(),
        protected_branches: Vec::new(),
        ai_tool: "test".to_string(),
        ai_model: None,
        ai_used_for: "test".to_string(),
        attempt_dispatcher: None,
        harvest_context: Default::default(),
    }
}

#[test]
fn promotion_operation_claim_completes_once_and_replays_persisted_result() {
    // #8357: promoting a cook attempt reserves a durable operation claim before
    // the effect and completes it with the result. An already-persisted
    // promotion (the resume path) loads without repeating the effect, and the
    // claim is marked completed so a subsequent pass replays the same result.
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-promote-claim";
        let run_id = "run-promote-claim";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        // Seed an already-applied promotion so the promote path loads it rather
        // than performing a real git/worktree promotion.
        agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion(run_id)).unwrap(),
        )
        .unwrap();

        let options = promotion_claim_options(cook_id, run_id);
        let operation_key = format!("promote:{run_id}");

        // No claim exists until the first promote pass.
        assert!(
            agent_task_lifecycle::operation_claim(run_id, &operation_key)
                .unwrap()
                .is_none()
        );

        let first = promote_with_operation_claim(&options, run_id).unwrap();
        assert_eq!(first.source.run_id.as_deref(), Some(run_id));

        // The claim is now completed with the promotion result.
        let claim = agent_task_lifecycle::operation_claim(run_id, &operation_key)
            .unwrap()
            .expect("promotion claim recorded");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
        assert!(!agent_task_lifecycle::operation_lease_is_active(run_id, &operation_key).unwrap());

        // A resumed pass replays the same promotion via AlreadyCompleted without
        // re-running the effect.
        let replayed = promote_with_operation_claim(&options, run_id).unwrap();
        assert_eq!(replayed.source.run_id, first.source.run_id);
        assert_eq!(replayed.patch_artifact.id, first.patch_artifact.id);
    });
}

#[test]
fn retry_dispatch_operation_key_claim_dispatches_once() {
    // #8357: the detached retry-dispatch path reserves a durable claim keyed by
    // the retry run id before the handoff and completes it after. A resumed pass
    // (or a concurrent one) observes the completed claim / held lease and must
    // not send a second handoff. This exercises that exactly-once contract at the
    // claim boundary without the full git-backed cook loop.
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-dispatch-claim";
        let next_run_id = "run-dispatch-claim-attempt-2";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(next_run_id)).unwrap();

        let operation_key = retry_dispatch_operation_key(next_run_id);
        let lease = std::time::Duration::from_secs(60);

        // First pass acquires the claim → performs the (modeled) dispatch → completes.
        assert_eq!(
            agent_task_lifecycle::claim_cook_operation(next_run_id, &operation_key, lease).unwrap(),
            agent_task_lifecycle::ClaimOutcome::Acquired
        );
        agent_task_lifecycle::complete_cook_operation(
            next_run_id,
            &operation_key,
            serde_json::json!({ "dispatched_run_id": next_run_id }),
        )
        .unwrap();

        // A resumed pass observes AlreadyCompleted and must not re-dispatch.
        match agent_task_lifecycle::claim_cook_operation(next_run_id, &operation_key, lease)
            .unwrap()
        {
            agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
                assert_eq!(result["dispatched_run_id"], next_run_id);
            }
            other => panic!("expected AlreadyCompleted, got {other:?}"),
        }
    });
}

#[test]
fn finalization_operation_claim_revalidates_completed_publication() {
    // #8357: finalization runs its external effects (commit/push/PR) then records
    // the result. The claim brackets it: the first pass finalizes exactly once and
    // completes the claim, and a resumed pass revalidates the recorded
    // publication via AlreadyCompleted. Uses an injected finalize closure so no
    // real Git/GitHub mutation occurs.
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-finalize-claim";
        let run_id = "run-finalize-claim";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();

        let options = promotion_claim_options(cook_id, run_id);
        let promotion = promotion(run_id);
        let finalize_calls = Arc::new(AtomicUsize::new(0));

        let calls = Arc::clone(&finalize_calls);
        let mut finalize =
            move |_: &AgentTaskCookServiceOptions, rid: &str, _: &AgentTaskPromotionReport| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({"status": "review_ready", "run_id": rid}))
            };

        let first =
            finalize_with_operation_claim(&options, run_id, &promotion, &mut finalize).unwrap();
        assert_eq!(first["status"], "review_ready");
        assert_eq!(finalize_calls.load(Ordering::SeqCst), 1);

        // A resumed pass must re-read publication identities rather than trust a
        // prior durable report.
        let replayed =
            finalize_with_operation_claim(&options, run_id, &promotion, &mut finalize).unwrap();
        assert_eq!(replayed["status"], "review_ready");
        assert_eq!(
            finalize_calls.load(Ordering::SeqCst),
            2,
            "a resumed finalization must revalidate publication identity"
        );

        let operation_key = finalization_operation_key(run_id, &promotion);
        let claim = agent_task_lifecycle::operation_claim(run_id, &operation_key)
            .unwrap()
            .expect("finalization claim recorded");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
    });
}

#[test]
fn review_form_follow_up_finalization_replays_its_durable_claim_after_restart() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-review-form-restart";
        let run_id = "cook-review-form-restart-attempt-2";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, run_id).unwrap();
        let options = promotion_claim_options(cook_id, run_id);
        let promotion = promotion(run_id);
        let calls = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let mut finalize =
                move |_: &AgentTaskCookServiceOptions, _: &str, _: &AgentTaskPromotionReport| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!({"status": "review_ready", "review_form": true}))
                };
            finalize_with_operation_claim(&options, run_id, &promotion, &mut finalize).unwrap();
        }

        let operation_key = finalization_operation_key(run_id, &promotion);
        let claim = agent_task_lifecycle::operation_claim(run_id, &operation_key)
            .unwrap()
            .expect("review-form finalization claim");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
        assert_eq!(claim.result.unwrap()["review_form"], true);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "restart only revalidates publication"
        );
    });
}

#[test]
fn duplicate_controller_passes_revalidate_one_promoted_candidate() {
    // #8357 acceptance (AC5 + AC7): duplicate/concurrent controller passes over
    // the same candidate must produce exactly one promotion checkpoint and
    // revalidate one published candidate. Drive the PRODUCTION `DefaultCookSideEffects`
    // boundary (which routes promote/finalize through the durable operation
    // claims) with an injected finalize effect, so no real Git/GitHub mutation
    // occurs. Promotion is seeded as already-applied so `promote` takes its load
    // path rather than performing a real git promotion.
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-acceptance-once";
        let run_id = "run-acceptance-once";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion(run_id)).unwrap(),
        )
        .unwrap();

        let options = promotion_claim_options(cook_id, run_id);
        let finalize_calls = Arc::new(AtomicUsize::new(0));

        // Run three independent controller passes, each with its own production
        // side-effect boundary, exactly as three restarted/concurrent controllers
        // would. The injected finalize effect increments a shared counter.
        let mut finalizations = Vec::new();
        for _ in 0..3 {
            let calls = Arc::clone(&finalize_calls);
            let mut side_effects = DefaultCookSideEffects::new(
                move |_: &AgentTaskCookServiceOptions, rid: &str, _: &AgentTaskPromotionReport| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!({"status": "review_ready", "run_id": rid}))
                },
            );
            let promotion = side_effects.promote(&options, run_id).unwrap();
            let finalization = side_effects.finalize(&options, run_id, &promotion).unwrap();
            finalizations.push(finalization);
        }

        // Every pass revalidates live publication identity for the same candidate.
        assert_eq!(
            finalize_calls.load(Ordering::SeqCst),
            3,
            "duplicate controller passes must revalidate the published candidate"
        );
        // Every pass observes the same review-ready finalization.
        for finalization in &finalizations {
            assert_eq!(finalization["status"], "review_ready");
        }

        // Exactly one durable promotion checkpoint and one completed finalization
        // claim survive.
        let promote_key = format!("promote:{run_id}");
        let promote_claim = agent_task_lifecycle::operation_claim(run_id, &promote_key)
            .unwrap()
            .expect("promotion claim recorded");
        assert_eq!(
            promote_claim.state,
            agent_task_lifecycle::ClaimState::Completed
        );

        let finalize_key = finalization_operation_key(run_id, &promotion(run_id));
        let finalize_claim = agent_task_lifecycle::operation_claim(run_id, &finalize_key)
            .unwrap()
            .expect("finalization claim recorded");
        assert_eq!(
            finalize_claim.state,
            agent_task_lifecycle::ClaimState::Completed
        );
    });
}

#[test]
fn concurrent_retry_dispatch_claims_admit_exactly_one_dispatcher() {
    // #8357 acceptance (AC5): concurrent continuation attempts racing to dispatch
    // the same retry run must admit exactly one dispatcher; the rest observe a
    // held lease (or a completed claim) and do not send a second handoff. This
    // exercises the claim's atomic converge-on-one-owner contract for the
    // retry-dispatch key across real threads.
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-concurrent-dispatch";
        let next_run_id = "run-concurrent-dispatch-attempt-2";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(next_run_id)).unwrap();

        let operation_key = retry_dispatch_operation_key(next_run_id);
        let lease = std::time::Duration::from_secs(300);
        let acquired = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(4));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let key = operation_key.clone();
                let run = next_run_id.to_string();
                let acquired = Arc::clone(&acquired);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    if let agent_task_lifecycle::ClaimOutcome::Acquired =
                        agent_task_lifecycle::claim_cook_operation(&run, &key, lease).unwrap()
                    {
                        acquired.fetch_add(1, Ordering::SeqCst);
                        // The winning dispatcher records completion after its handoff.
                        agent_task_lifecycle::complete_cook_operation(
                            &run,
                            &key,
                            serde_json::json!({ "dispatched_run_id": run }),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            acquired.load(Ordering::SeqCst),
            1,
            "exactly one concurrent pass may dispatch the retry"
        );
        let claim = agent_task_lifecycle::operation_claim(next_run_id, &operation_key)
            .unwrap()
            .expect("dispatch claim recorded");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
    });
}

#[test]
fn historical_applied_promotion_restores_only_its_exact_checkpoint_baseline() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = AgentTaskPlan::new("cook-historical-baseline", Vec::new());
        let run_id = "cook-historical-baseline-attempt-1";
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt("cook-historical-baseline", 1, run_id).unwrap();
        let candidate = serde_json::json!({"kind":"git","fingerprint":{"head":"abc"}});
        let checkpoint = serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1",
            "status":"verification_pending",
            "source":{"kind":"aggregate","task_id":"task","run_id":run_id},
            "to_worktree":"fixture@target",
            "target":{"worktree":"fixture@target","path":"/fixture/target"},
            "patch_artifact":{"id":"patch","kind":"patch","path":"/fixture/patch","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "provenance":{"post_apply":true,"candidate":candidate,"gate_feedback_baseline":{"schema":"homeboy/agent-task-gate-feedback-baseline/v1","current_diff":"diff --git a/a b/a\n"}},
            "operator_notification":{"status":"blocked","message":"pending"}
        });
        let applied = serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1",
            "status":"applied",
            "source":{"kind":"aggregate","task_id":"task","run_id":run_id},
            "to_worktree":"fixture@target",
            "target":{"worktree":"fixture@target","path":"/fixture/target"},
            "patch_artifact":{"id":"patch","kind":"patch","path":"/fixture/patch","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "provenance":{"candidate":candidate},
            "operator_notification":{"status":"completed","message":"applied"}
        });
        agent_task_lifecycle::record_promotion(run_id, checkpoint).unwrap();
        agent_task_lifecycle::record_promotion(run_id, applied).unwrap();

        let restored = persisted_promotion_for_attempt("cook-historical-baseline")
            .unwrap()
            .expect("alias resolves historical attempt");
        assert_eq!(
            restored.provenance["gate_feedback_baseline"]["current_diff"],
            "diff --git a/a b/a\n"
        );
    });
}

#[test]
fn persisted_promotion_from_another_attempt_is_rejected() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-persisted";
        let run_id = "run-persisted";
        let plan = AgentTaskPlan::new(cook_id, Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion("different-run")).unwrap(),
        )
        .unwrap();

        let error = persisted_promotion_for_attempt(cook_id).unwrap_err();
        assert!(error.message.contains("does not belong to this attempt"));
        assert_eq!(error.details["requested_run_id"], cook_id);
        assert_eq!(error.details["resolved_run_id"], run_id);
        assert_eq!(error.details["promotion_run_id"], "different-run");
        assert!(persisted_promotion_for_attempt(run_id).is_err());
    });
}

#[test]
fn cook_successful_concrete_attempt_publishes_reviewer_body() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-8058-attempt-1";
        let mut fixture_options =
            batch_cook_options("cook-8058", Arc::new(AcceptedDetachedAttemptDispatcher));
        fixture_options.initial_plan.tasks[0].executor.backend = "fixture-provider".to_string();
        fixture_options.initial_plan.tasks[0].executor.model =
            Some("fixture-model; review form: spoof".to_string());
        let plan = fixture_options.initial_plan.clone();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        let options = AgentTaskCookServiceOptions {
            cook_id: "cook-8058".to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: "homeboy@8058".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: VerifyGateOptions {
                verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: Default::default(),
                ..Default::default()
            },
            max_attempts: 1,
            no_finalize: false,
            base: "main".to_string(),
            task_base_sha: Some("task-candidate-base".to_string()),
            head: Some("fix/8058".to_string()),
            title: "Close #8058".to_string(),
            commit_message: "test".to_string(),
            source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/8058".to_string()],
            protected_branches: vec!["main".to_string()],
            ai_tool: "fixture-provider".to_string(),
            ai_model: Some("fixture-model; review form: spoof".to_string()),
            ai_used_for: "Drafted test coverage.".to_string(),
            attempt_dispatcher: None,
            harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
        };
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::record_cook_attempt("cook-8058", 1, run_id).unwrap();
        seed_review_form_aggregate(run_id, &plan);
        let mut backend = CaptureBackend::default();
        finalize_cook_pr_with_backend(&options, run_id, &promotion(run_id), &mut backend).unwrap();
        for section in [
                "## Summary",
                "## What changed",
                "## How to test",
                "## Compatibility",
                "## Evidence",
                "## AI assistance",
                "openai/gpt-5.6-terra",
                "Verified finalization base: main at verified-base",
                // AI-authored prose (from the seeded review form).
                "Close the issue by guarding the reload path.",
                "Add a null guard in the render path.",
                "Internal-only change; no compatibility impact.",
                "Reproduced the failure, isolated the reload path",
                // Deterministic evidence (orchestrator-owned).
                "1. Run `cargo test --locked agent_task_promotion --lib`; expect passes as recorded by Cook's deterministic gate.",
                "Verified candidate scope: 1 changed file(s): src/lib.rs.",
                "Cook deterministic verification: 1 gate(s) completed green.",
            ] {
                assert!(
                    backend.body.contains(section),
                    "missing {section}: {}",
                    backend.body
                );
            }
        for forbidden in [
            "Publication intent",
            "homeboy/agent-task",
            "Changed files",
            "Final status",
        ] {
            assert!(
                !backend.body.contains(forbidden),
                "unexpected {forbidden}: {}",
                backend.body
            );
        }
        assert!(backend.committed && backend.pushed && backend.created);
    });
}

#[test]
fn recovery_hydrates_adopted_baseline_gate_evidence_and_can_preflight_without_mutation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-9750";
        let run_id = "cook-9750-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
            ..Default::default()
        };
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        seed_review_form_aggregate(run_id, &options.initial_plan);
        let mut adopted = promotion(run_id);
        adopted.status = crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed;
        adopted.deterministic_gates[0].status =
            crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure;
        adopted.deterministic_gates[0].exit_code = 1;
        adopted.deterministic_gates[0].baseline_comparison =
            Some(crate::agent_task_gate::AgentTaskGateBaselineComparison {
                base_ref: "immutable-base".to_string(),
                exit_code: 1,
                failure_fingerprint: "inherited failure".to_string(),
                matches_candidate_failure: true,
            });
        adopted.operator_notification =
            crate::agent_task_promotion::AgentTaskPromotionNotification {
                status: "blocked".to_string(),
                message: "patch promoted, but deterministic gates failed".to_string(),
                resumable_blocker: Some("stale gate failure".to_string()),
                next_command: None,
            };
        // This simulates a restart after adoption before a stale owning attempt
        // status can be reconciled. Recovery derives the durable gate outcome.
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(adopted).unwrap())
            .unwrap();

        let mut preflight_backend = CaptureBackend::default();
        let preflight =
            recover_cook_pr_with_backend(cook_id, Vec::new(), true, &mut preflight_backend)
                .unwrap();
        assert_eq!(preflight["status"], "validated");
        assert!(!preflight_backend.committed);
        assert!(!preflight_backend.pushed);
        assert!(!preflight_backend.created);
        let preflight_record = agent_task_lifecycle::status(run_id).unwrap();
        assert_eq!(
            preflight_record.metadata["latest_promotion"]["status"],
            "gate_failed"
        );
        assert_eq!(
            preflight_record.metadata["latest_promotion"]["operator_notification"]["status"],
            "blocked"
        );

        let mut publish_backend = CaptureBackend::default();
        let report = recover_cook_pr_with_backend(
            run_id,
            vec![crate::agent_task_review_dossier::AgentTaskReviewOverride {
                target: crate::agent_task_review_dossier::AgentTaskReviewOverrideTarget::Summary,
                value: "Recovered from durable Cook evidence.".to_string(),
                provenance: "reviewed issue #9750".to_string(),
            }],
            false,
            &mut publish_backend,
        )
        .unwrap();
        assert_eq!(report["status"], "review_ready");
        assert_eq!(report["run_id"], run_id);
        assert_eq!(report["changed_files"], serde_json::json!(["src/lib.rs"]));
        assert!(publish_backend.committed && publish_backend.pushed && publish_backend.created);
        assert!(publish_backend
            .body
            .contains("Recovered from durable Cook evidence."));
        let record = agent_task_lifecycle::status(run_id).unwrap();
        assert_eq!(record.metadata["latest_promotion"]["status"], "applied");
        assert_eq!(
            record.metadata["latest_promotion"]["operator_notification"]["status"],
            "completed"
        );
        assert!(
            record.metadata["latest_promotion"]["operator_notification"]["resumable_blocker"]
                .is_null()
        );
    });
}

#[test]
fn cook_rejects_test_claim_without_matching_durable_gate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-8058-mismatch";
        let plan = AgentTaskPlan::new("cook-8058", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        let options = AgentTaskCookServiceOptions {
            cook_id: "cook-8058".to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: AgentTaskPlan::new("cook-8058", Vec::new()),
            to_worktree: "homeboy@8058".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: VerifyGateOptions {
                verify: vec!["cargo test unsupported".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: Default::default(),
                ..VerifyGateOptions::default()
            },
            max_attempts: 1,
            no_finalize: false,
            base: "main".to_string(),
            task_base_sha: Some("task-candidate-base".to_string()),
            head: Some("fix/8058".to_string()),
            title: "Close #8058".to_string(),
            commit_message: "test".to_string(),
            source_refs: Vec::new(),
            protected_branches: vec!["main".to_string()],
            ai_tool: "fixture-provider".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "Drafted test coverage.".to_string(),
            attempt_dispatcher: None,
            harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
        };
        let error = finalize_cook_pr_with_backend(
            &options,
            run_id,
            &promotion(run_id),
            &mut CaptureBackend::default(),
        )
        .expect_err("unsupported test claim is rejected");
        assert!(error
            .message
            .contains("matching successful visible durable gate"));
    });
}

#[test]
fn follow_up_baseline_is_clean_and_preserves_binary_mode_and_untracked_candidate_state() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = &temp.path().join("repo");
    std::fs::create_dir(root).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let target_head = git_output(root, &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(root.join("candidate.bin"), [0_u8, 1, 2, 255]).unwrap();
    std::fs::write(root.join("candidate.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = std::fs::metadata(root.join("candidate.sh"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(root.join("candidate.sh"), permissions).unwrap();
    assert!(Command::new("git")
        .args(["add", "--all"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let patch = Command::new("git")
        .args([
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--find-renames",
            "HEAD",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(patch.status.success());
    let patch_path = temp.path().join("candidate.patch");
    std::fs::write(&patch_path, patch.stdout).unwrap();
    assert!(Command::new("git")
        .args(["reset"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":target_head},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path}, "changed_files":["candidate.bin", "candidate.sh"],
            "command_evidence":[], "deterministic_gates":[], "gate_results":[],
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        })).unwrap();
    let baseline =
        materialize_follow_up_baseline(&report, "first-run", "candidate-task").expect("baseline");
    assert!(git_output(&baseline.path, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read(baseline.path.join("candidate.bin")).unwrap(),
        [0_u8, 1, 2, 255]
    );
    assert!(
        baseline
            .path
            .join("candidate.sh")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    assert!(!baseline.capability.commit().is_empty());
    assert!(!baseline.capability.tree().is_empty());
    assert_eq!(baseline.capability.bound_task_id(), "candidate-task");
    assert_eq!(
        baseline.artifact_provenance()["source_patch_artifact_sha256"],
        sha2::Sha256::digest(std::fs::read(&patch_path).unwrap())
            .to_vec()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
}

#[test]
fn follow_up_baseline_combines_adopted_candidate_with_overlapping_provider_delta() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = &temp.path().join("repo");
    std::fs::create_dir(root).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("candidate.txt"), "base\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let target_head = git_output(root, &["rev-parse", "HEAD"]).unwrap();

    std::fs::write(root.join("candidate.txt"), "adopted candidate\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "candidate.txt"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    std::fs::write(
        root.join("candidate.txt"),
        "adopted candidate\nprovider delta\n",
    )
    .unwrap();
    let provider_patch = Command::new("git")
        .args(["diff", "--binary", "--full-index", "--find-renames"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(provider_patch.status.success());
    let complete_candidate = Command::new("git")
        .args(["diff", "--binary", "--full-index", "--find-renames", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(complete_candidate.status.success());
    let patch_path = temp.path().join("provider.patch");
    std::fs::write(&patch_path, provider_patch.stdout).unwrap();
    let complete_candidate = String::from_utf8(complete_candidate.stdout).unwrap();
    let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
        "schema":"homeboy/agent-task-promotion-report/v1", "status":"applied",
        "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
        "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":target_head},
        "patch_artifact":{"id":"provider-delta","kind":"patch","path":patch_path},
        "changed_files":["candidate.txt"], "command_evidence":[], "deterministic_gates":[], "gate_results":[],
        "provenance":{"worktree_path":root, "gate_feedback_baseline":{"current_diff":complete_candidate}},
        "operator_notification":{"status":"completed","message":"applied"}
    }))
    .unwrap();

    let baseline =
        materialize_follow_up_baseline(&report, "first-run", "candidate-task").expect("baseline");

    assert!(git_output(&baseline.path, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read_to_string(baseline.path.join("candidate.txt")).unwrap(),
        "adopted candidate\nprovider delta\n"
    );
}

#[test]
fn follow_up_baseline_refuses_when_promotion_target_head_has_advanced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "A"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let head_a = git_output(&root, &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(root.join("advanced.txt"), "B\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "B"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let patch_path = temp.path().join("candidate.patch");
    std::fs::write(&patch_path, "").unwrap();
    let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":head_a},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path},
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        }))
        .unwrap();

    let error = match materialize_follow_up_baseline(&report, "first-run", "candidate-task") {
        Ok(_) => panic!("target advancement rejects the stale promotion baseline"),
        Err(error) => error,
    };

    assert!(
        error.message.contains("target HEAD changed"),
        "unexpected error: {}",
        error.message
    );
}

/// Rebuild the batch children's recorded attempt dispatcher from its durable
/// descriptor, mirroring the production `reconstruct_cook_attempt_dispatcher`
/// closure the CLI supplies. Resume never dispatches a terminal child, so this
/// is only exercised to satisfy the recipe transport contract.
fn test_reconstruct_dispatcher(
    descriptor: &Value,
) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>> {
    match descriptor.get("kind").and_then(Value::as_str) {
        Some("test-recording-detached") => Ok(Some(Arc::new(RecordingDetachedAttemptDispatcher {
            dispatches: Arc::new(AtomicUsize::new(0)),
        }))),
        Some("local") | None => Ok(None),
        Some(other) => Err(Error::validation_invalid_argument(
            "attempt_dispatch.kind",
            format!("test dispatcher reconstruction does not know kind `{other}`"),
            None,
            None,
        )),
    }
}

/// Stage one batch child as if its provider attempt already succeeded on a
/// runner but the coordinator exited before promotion/finalization: persist the
/// durable recipe, a terminal Succeeded aggregate, and an applied promotion.
/// When `pre_finalized` is set, also record a `cook_finalization` so the resume
/// exercises the idempotent load path (no real PR backend needed).
fn stage_terminal_batch_child(
    cook_id: &str,
    status: crate::agent_task_scheduler::AgentTaskAggregateStatus,
    pre_finalized: bool,
    workspace: &std::path::Path,
) -> String {
    let mut options = batch_cook_options(
        cook_id,
        Arc::new(RecordingDetachedAttemptDispatcher {
            dispatches: Arc::new(AtomicUsize::new(0)),
        }),
    );
    // Terminal harvest validates the same linked-worktree proof as production.
    // The fixture receives a real task worktree rather than a provider handle.
    options.to_worktree = workspace.display().to_string();
    options.source_worktree_path = Some(workspace.to_path_buf());
    // The provider attempt is complete; no dispatcher work should run on resume.
    options.no_finalize = false;
    options.provider_command = Some("fixture-provider".to_string());
    options.gates = VerifyGateOptions {
        verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
        private_gate_reveal: crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
        ..Default::default()
    };
    // In production the fanout batch child `run_id` equals the cook's recipe key
    // (both are `cook-<id>`), so the resume path loads the recipe by that id.
    // Mirror that here so the reconstructed cook resolves.
    options.initial_run_id = cook_id.to_string();
    let run_id = options.initial_run_id.clone();
    super::super::persist_initial_recipe(&options).expect("persist child recipe");
    agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
    agent_task_lifecycle::record_run_aggregate(
        &run_id,
        &options.initial_plan,
        &crate::agent_task_scheduler::AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: options.initial_plan.plan_id.clone(),
            status,
            totals: crate::agent_task_scheduler::AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![crate::agent_task::AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "provider".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("provider dispatched once".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: test_review_form_outputs(),
                workflow: None,
                follow_up: None,
                metadata: serde_json::json!({ "model": "fixture-model" }),
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        },
    )
    .unwrap();
    agent_task_lifecycle::record_promotion(
        &run_id,
        serde_json::to_value(promotion(&run_id)).unwrap(),
    )
    .unwrap();
    if pre_finalized {
        agent_task_lifecycle::record_cook_finalization(
            &run_id,
            serde_json::json!({ "status": "review_ready", "pr": { "number": 42 } }),
        )
        .unwrap();
    }
    run_id
}

#[test]
fn resume_cook_batch_harvests_terminal_children_without_redispatching_the_provider() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temporary = tempfile::tempdir().expect("temporary task worktree root");
        let workspace = temporary.path().join("task-worktree");
        let source = std::env::current_dir().expect("test repository checkout");
        assert!(Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&workspace)
            .arg("HEAD")
            .current_dir(source)
            .status()
            .expect("create linked task worktree")
            .success());
        // Two children finished their provider attempt but were never finalized
        // (the synchronous coordinator exited); a pre-recorded finalization
        // stands in for the real PR backend so the resume exercises the
        // idempotent load path deterministically (#9525).
        let child_a = stage_terminal_batch_child(
            "cook-9525-a",
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded,
            true,
            &workspace,
        );
        let child_b = stage_terminal_batch_child(
            "cook-9525-b",
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded,
            true,
            &workspace,
        );
        let child_partial = stage_terminal_batch_child(
            "cook-9525-partial",
            crate::agent_task_scheduler::AgentTaskAggregateStatus::PartialRecoverable,
            true,
            &workspace,
        );

        crate::agent_task_batch::persist_fanout_run_batch(
            "batch-9525",
            "batch-9525",
            &[
                crate::agent_task_batch::FanoutRunBatchChild {
                    task_id: "cook-9525-a".to_string(),
                    run_id: child_a.clone(),
                },
                crate::agent_task_batch::FanoutRunBatchChild {
                    task_id: "cook-9525-b".to_string(),
                    run_id: child_b.clone(),
                },
                crate::agent_task_batch::FanoutRunBatchChild {
                    task_id: "cook-9525-partial".to_string(),
                    run_id: child_partial.clone(),
                },
            ],
            Value::Null,
        )
        .expect("persist batch record");

        // UnusedExecutor asserts the provider is never dispatched again: a
        // terminal child is harvested straight through gates and finalization.
        // The dispatcher is reconstructed only to satisfy the recipe contract.
        let result = resume_cook_batch("batch-9525", UnusedExecutor, test_reconstruct_dispatcher)
            .expect("resume harvests terminal children");

        assert_eq!(
            result.exit_code, 0,
            "both children finalize green: {:#?}",
            result.value
        );
        assert_eq!(result.value.status, "succeeded");
        assert_eq!(result.value.total, 3);
        assert_eq!(result.value.succeeded, 3);
        assert_eq!(result.value.failed, 0);
        for cell in &result.value.cooks {
            assert_eq!(cell.exit_code, 0);
            assert_eq!(
                cell.result.as_ref().map(|report| report.status.as_str()),
                Some("review_ready")
            );
        }

        // Per-child finalization state is reconciled into the durable batch
        // record so the coordinator's progress survives a second exit.
        let batch = crate::agent_task_batch::read_batch_record("batch-9525")
            .expect("batch record after resume");
        let finalizations = batch.metadata["child_finalizations"]
            .as_object()
            .expect("child_finalizations recorded");
        assert!(finalizations.contains_key(&child_a));
        assert!(finalizations.contains_key(&child_b));
        assert!(finalizations.contains_key(&child_partial));

        // Resume is idempotent: a second call still succeeds and does not
        // redispatch or duplicate a PR (finalization is loaded, not recreated).
        let second = resume_cook_batch("batch-9525", UnusedExecutor, test_reconstruct_dispatcher)
            .expect("second resume is idempotent");
        assert_eq!(second.exit_code, 0);
        assert_eq!(second.value.succeeded, 3);
    });
}

#[test]
fn fanout_resume_prefers_immutable_verification_checkpoint_over_later_failed_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("candidate");
        std::fs::create_dir(&target).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&target)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(target.join("lib.rs"), "old\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&target)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(&target)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["remote", "add", "origin", "."])
            .current_dir(&target)
            .status()
            .unwrap()
            .success());
        std::fs::write(target.join("lib.rs"), "new\n").unwrap();
        let patch = format!("{}\n", git_output(&target, &["diff", "--binary"]).unwrap());
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, &patch).unwrap();

        let dispatches = Arc::new(AtomicUsize::new(0));
        let cook_id = "cook-9703";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.initial_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        options.initial_plan.tasks[0].workspace.root = Some(target.display().to_string());
        options.source_worktree_path = None;
        options.gates.verify = vec!["true".to_string()];
        options.no_finalize = false;
        options.head = Some("fix/8058".to_string());
        super::super::persist_initial_recipe(&options).unwrap();
        let run_id = options.initial_run_id.clone();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
        seed_review_form_aggregate(&run_id, &options.initial_plan);
        let mut aggregate = agent_task_lifecycle::read_aggregate(&run_id).unwrap();
        aggregate.outcomes[0]
            .artifacts
            .push(crate::agent_task::AgentTaskArtifact {
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: Some("candidate.patch".to_string()),
                label: None,
                role: None,
                semantic_key: None,
                path: Some(patch_path.display().to_string()),
                url: None,
                mime: Some("text/x-diff".to_string()),
                size_bytes: Some(patch.len() as u64),
                sha256: Some(format!("{:x}", sha2::Sha256::digest(patch.as_bytes()))),
                metadata: Value::Null,
            });
        agent_task_lifecycle::record_run_aggregate(&run_id, &options.initial_plan, &aggregate)
            .unwrap();
        let candidate = crate::agent_task_promotion::candidate_fingerprint(
            target.to_str().expect("target path"),
        )
        .unwrap();
        let base_sha = git_output(&target, &["rev-parse", "HEAD"]).unwrap();
        let mut checkpoint = serde_json::to_value(promotion(&run_id)).unwrap();
        checkpoint["status"] = serde_json::json!("verification_pending");
        checkpoint["source"]["task_id"] = serde_json::json!(options.initial_plan.tasks[0].task_id);
        checkpoint["to_worktree"] = serde_json::json!(options.to_worktree);
        checkpoint["target"] = serde_json::json!({"worktree": options.to_worktree, "path": target});
        checkpoint["patch_artifact"]["path"] = serde_json::json!(patch_path);
        checkpoint["patch_artifact"]["sha256"] =
            serde_json::json!(format!("{:x}", sha2::Sha256::digest(patch.as_bytes())));
        checkpoint["provenance"] = serde_json::json!({
            "worktree_path": target,
            "candidate": candidate,
            "resume_inputs": {"base_ref": "main", "task_base_sha": null, "candidate_ref": null}
        });
        checkpoint["verified_base"]["sha"] = serde_json::json!(base_sha.trim());
        agent_task_lifecycle::record_promotion(&run_id, checkpoint.clone()).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &run_id).unwrap();

        // This is the coordinator's bad later attempt. It becomes the index
        // latest run, but must not replace the promoted source checkpoint.
        let failed_run = agent_task_lifecycle::cook_attempt_run_id(cook_id, 2);
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&failed_run)).unwrap();
        agent_task_lifecycle::record_run_aggregate(
            &failed_run,
            &options.initial_plan,
            &crate::agent_task_scheduler::AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.initial_plan.plan_id.clone(),
                status: crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed,
                totals: crate::agent_task_scheduler::AgentTaskAggregateTotals {
                    failed: 1,
                    ..Default::default()
                },
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .unwrap();
        super::super::record_recipe_attempt(cook_id, 2, &failed_run, &options.initial_plan)
            .unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, &failed_run).unwrap();
        crate::agent_task_batch::persist_fanout_run_batch(
            "batch-9703",
            "batch-9703",
            &[crate::agent_task_batch::FanoutRunBatchChild {
                task_id: cook_id.to_string(),
                run_id: cook_id.to_string(),
            }],
            Value::Null,
        )
        .unwrap();

        let mut backend = CaptureBackend {
            hydrate_gate_proof_run_id: Some(run_id.clone()),
            ..Default::default()
        };
        let first = resume_cook_batch_with_finalizer(
            "batch-9703",
            UnusedExecutor,
            |_| {
                Ok(Some(Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::clone(&dispatches),
                })))
            },
            |options, source_run_id, promotion| {
                finalize_or_load_cook_pr_with_backend(
                    options,
                    source_run_id,
                    promotion,
                    &mut backend,
                )
            },
        )
        .unwrap();
        assert_eq!(first.exit_code, 0, "{:?}", first.value);
        assert_eq!(
            first.value.cooks[0]
                .result
                .as_ref()
                .unwrap()
                .latest_run_id
                .as_deref(),
            Some(run_id.as_str())
        );
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert!(backend.committed);
        assert!(backend.pushed);
        assert!(backend.created);
        let source = agent_task_lifecycle::status(&run_id).unwrap();
        assert_eq!(
            source.metadata["cook_recovery_source_checkpoint"],
            checkpoint
        );
        assert_eq!(
            source.metadata["cook_recovery_checkpoint"]["next_command"],
            "homeboy agent-task fanout resume batch-9703"
        );

        backend.committed = false;
        backend.pushed = false;
        backend.created = false;
        let second = resume_cook_batch_with_finalizer(
            "batch-9703",
            UnusedExecutor,
            |_| {
                Ok(Some(Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::clone(&dispatches),
                })))
            },
            |options, source_run_id, promotion| {
                finalize_or_load_cook_pr_with_backend(
                    options,
                    source_run_id,
                    promotion,
                    &mut backend,
                )
            },
        )
        .unwrap();
        assert_eq!(second.exit_code, 0);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    });
}

#[test]
fn resume_cook_batch_reports_a_child_with_no_recipe_as_unresumable() {
    homeboy_core::test_support::with_isolated_home(|_| {
        // A child that never reached cook start has no durable recipe; resume
        // must surface an actionable error for it rather than fabricate a cook.
        crate::agent_task_batch::persist_fanout_run_batch(
            "batch-9525-missing",
            "batch-9525-missing",
            &[crate::agent_task_batch::FanoutRunBatchChild {
                task_id: "cook-missing".to_string(),
                run_id: "cook-9525-missing-child".to_string(),
            }],
            Value::Null,
        )
        .expect("persist batch record");

        let result = resume_cook_batch(
            "batch-9525-missing",
            UnusedExecutor,
            test_reconstruct_dispatcher,
        )
        .expect("resume returns a report even when a child cannot resume");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "failed");
        assert_eq!(result.value.failed, 1);
        let cell = &result.value.cooks[0];
        assert_eq!(cell.exit_code, 1);
        assert!(cell
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no durable recipe")));
    });
}

#[test]
fn cook_report_latest_run_id_prefers_invocation_over_stale_cook_index() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use crate::agent_task_lifecycle;

        let cook_id = "cook-8010-test";
        let stale_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        let stale_plan = AgentTaskPlan::new("plan-stale", Vec::new());
        agent_task_lifecycle::submit_plan(&stale_plan, Some(&stale_run_id))
            .expect("stale run submitted");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &stale_run_id)
            .expect("stale cook attempt indexed");

        let stale_index = agent_task_lifecycle::cook_index(cook_id).expect("stale index");
        assert_eq!(stale_index.latest_run_id, stale_run_id);

        let fresh_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 2);
        let invocation_run_ids = vec![fresh_run_id.clone()];

        let report = cook_report(
            cook_id.to_string(),
            "execution_budget_exhausted",
            vec![AgentTaskCookAttemptReport {
                attempt: 2,
                run_id: fresh_run_id.clone(),
                run_state: "Running".to_string(),
                aggregate_path: None,
                promotion: None,
                feedback: None,
            }],
            None,
            Some("provider execution stopped because budget was exhausted".to_string()),
            1,
            Some(&fresh_run_id),
        );

        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(fresh_run_id.as_str()),
            "latest_run_id must be THIS invocation's run, not the stale cook_index run"
        );
        assert_ne!(
            report.value.latest_run_id.as_deref(),
            Some(stale_run_id.as_str()),
            "latest_run_id must not point at the prior-session stale run"
        );
        assert_eq!(
            report.value.invocation_run_ids, invocation_run_ids,
            "invocation_run_ids must contain exactly the runs dispatched in this invocation"
        );
        assert!(
            report.value.history_run_ids.contains(&stale_run_id),
            "history_run_ids should still include the full cross-invocation history"
        );
        assert!(
            report.value.history_run_ids.contains(&fresh_run_id),
            "history_run_ids should include the current invocation's run"
        );
    });
}

#[test]
fn post_materialization_failure_families_expose_only_durable_identity_and_legal_recovery() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-9655";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).expect("persist durable recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("materialize durable run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &options.initial_run_id)
            .expect("index durable run");
        agent_task_lifecycle::rewrite_record_for_test(&options.initial_run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .expect("record provider budget consumption");

        // Promotion, deterministic-gate, follow-up materialization, and
        // finalization all return through cook_report after this point.
        for status in [
            "promotion_failure",
            "deterministic_gate_failure",
            "follow_up_materialization_failure",
            "finalization_failure",
        ] {
            let report = cook_report(
                cook_id.to_string(),
                status,
                Vec::new(),
                None,
                Some("private provider evidence remains in durable diagnostics".to_string()),
                1,
                Some(&options.initial_run_id),
            );
            let value = serde_json::to_value(report.value).expect("serialize command data");
            let context = &value["failure_context"];

            assert_eq!(context["cook_id"], cook_id, "{status}");
            assert_eq!(context["latest_run_id"], options.initial_run_id, "{status}");
            assert_eq!(
                context["durable_recipe_ref"],
                format!("homeboy://agent-task/cooks/{cook_id}/recipe"),
                "{status}"
            );
            assert_eq!(context["lifecycle_state"], "Queued", "{status}");
            assert_eq!(context["provider_budget_consumed"], true, "{status}");
            assert_eq!(context["provider_executions_consumed"], 1, "{status}");
            assert_eq!(context["recovery_legal"], true, "{status}");
            assert_eq!(
                context["legal_actions"],
                serde_json::json!([
                    { "action": "status", "command": format!("homeboy agent-task status {} --full", options.initial_run_id) },
                    { "action": "diagnose", "command": format!("homeboy agent-task diagnose {}", options.initial_run_id) },
                    { "action": "resume", "command": format!("homeboy agent-task cook-continue {}", options.initial_run_id) },
                ]),
                "{status}"
            );
            assert!(
                !context.to_string().contains("private provider evidence"),
                "{status} must not copy private evidence into failure_context"
            );
        }
    });
}

#[test]
fn post_recipe_failure_without_lifecycle_retains_identity_but_offers_no_recovery_command() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-9655-recipe-only";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).expect("persist durable recipe");

        let report = durable_cook_error_report(
            &options,
            Error::internal_unexpected("lifecycle materialization failed"),
        )
        .expect("convert post-recipe error");

        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(options.initial_run_id.as_str())
        );
        let context = report
            .value
            .failure_context
            .expect("durable failure context");
        assert_eq!(context.cook_id, cook_id);
        assert_eq!(context.latest_run_id, options.initial_run_id);
        assert_eq!(
            context.durable_recipe_ref,
            format!("homeboy://agent-task/cooks/{cook_id}/recipe")
        );
        assert_eq!(
            context.lifecycle_state,
            "recipe_persisted_without_lifecycle_record"
        );
        assert_eq!(context.phase, "controller");
        assert_eq!(context.reason_code, "internal.unexpected");
        assert_eq!(
            context.diagnostic.expect("durable error diagnostic")["message"],
            "lifecycle materialization failed"
        );
        assert!(!context.recovery_legal);
        assert!(context.legal_actions.is_empty());
        assert!(!context.provider_budget_consumed);
        assert_eq!(context.provider_executions_consumed, 0);
    });
}

#[test]
fn re_materialize_follow_up_baseline_recovers_after_worktree_deletion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = &temp.path().join("repo");
    std::fs::create_dir(root).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let target_head = git_output(root, &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(root.join("patched.txt"), "patched\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "patched.txt"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let patch = Command::new("git")
        .args([
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--find-renames",
            "HEAD",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(patch.status.success());
    let patch_path = temp.path().join("candidate.patch");
    std::fs::write(&patch_path, patch.stdout).unwrap();
    assert!(Command::new("git")
        .args(["reset"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
        "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
        "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
        "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":target_head},
        "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path},
        "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
    }))
    .unwrap();
    let baseline =
        materialize_follow_up_baseline(&report, "first-run", "candidate-task").expect("baseline");
    let baseline_path = baseline.path.clone();
    assert!(baseline_path.exists());
    let original_commit = baseline.capability().commit().to_string();
    let original_tree = baseline.capability().tree().to_string();
    drop(baseline);
    assert!(!baseline_path.exists(), "drop should remove the worktree");
    let re_materialized =
        re_materialize_follow_up_baseline(&report, &baseline_path, "first-run", "candidate-task")
            .expect("re-materialized baseline");
    assert_eq!(re_materialized.path, baseline_path);
    assert!(baseline_path.exists());
    assert_eq!(re_materialized.capability().commit(), original_commit);
    assert_eq!(re_materialized.capability().tree(), original_tree);
    assert_eq!(
        std::fs::read_to_string(baseline_path.join("patched.txt")).unwrap(),
        "patched\n"
    );
    assert!(git_output(&baseline_path, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
fn cook_report_invocation_run_ids_populated_for_policy_failure() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fresh_run_id = "agent-task-fresh-abc123".to_string();
        let report = cook_report(
            "cook-test".to_string(),
            "policy_failure",
            vec![AgentTaskCookAttemptReport {
                attempt: 1,
                run_id: fresh_run_id.clone(),
                run_state: "Succeeded".to_string(),
                aggregate_path: None,
                promotion: None,
                feedback: None,
            }],
            None,
            Some("policy failure".to_string()),
            1,
            Some(&fresh_run_id),
        );

        assert_eq!(report.value.invocation_run_ids, vec![fresh_run_id.clone()]);
        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(fresh_run_id.as_str())
        );
        assert_eq!(report.value.status, "policy_failure");
    });
}
