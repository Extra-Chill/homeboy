//! Tests for the cook orchestration service (`super::cook`). Split from the
//! cook god file via #[path]; logically remains `cook::tests` so `super::`
//! paths are unchanged.

use super::super::cook_adoption::{
    adopt_cook_candidate, adopt_cook_candidate_with_dispatcher_and_backend,
    candidate_adoption_source, concrete_adoption_ai_model, resolve_adoption_target,
    resolve_adoption_target_with_attempt_in_stores,
};
use super::super::cook_baseline::git_output;
use super::super::cook_pre_execution::recover_recipe_attempt_with_stores;
use super::super::cook_promotion::{
    canonical_cook_patch_artifact_id, canonical_cook_recovery_run_id, cook_finalization_options,
    cook_finalization_options_with_stores, cook_promotion_argv, cook_report,
    finalize_cook_pr_with_backend, finalize_cook_pr_with_backend_with_stores,
    finalize_or_load_cook_pr_with_backend, finalize_or_load_cook_pr_with_backend_with_stores,
    mark_replacement_gate_execution_started, moving_base_recovery_for_run,
    moving_base_recovery_for_run_with_stores, moving_base_recovery_from_promotion,
    moving_base_recovery_report, next_moving_base_recovery, persist_manual_finalization_intent,
    persist_manual_finalization_receipt, persisted_promotion_for_attempt,
    persisted_promotion_for_attempt_in_store, prepare_manual_finalization_identity,
    record_replacement_gate_proof, recover_cook_pr_with_backend,
    recover_moving_base_cook_candidate_in_store, refreshed_moving_base_recovery,
    selected_candidate_task_id_in_store, verify_replacement_gates, CookReportInput,
    MovingBaseCookRecovery,
};
use super::super::cook_recipe::{
    persist_initial_recipe, set_initial_recipe_creation_barrier_for_test,
};
use super::*;
use crate::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
};
use crate::agent_task_finalization::{
    AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrFinalizationReport,
    AgentTaskPrRef, AgentTaskPublicationBinding, AgentTaskPublicationGitTracking,
    RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunState};
use crate::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskState};
use homeboy_core::run_lifecycle_record::{
    ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
    RunLifecycleRecord,
};
use serde::Deserialize;
use sha2::Digest;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, LazyLock, Mutex};

const DURABLE_COOK_FIXTURE_SCHEMA: &str = "homeboy/durable-cook-fixture/v1";
static CONFIG_LOCK_STRICT_TEST: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn with_strict_config_lock(test: impl FnOnce()) {
    let _guard = CONFIG_LOCK_STRICT_TEST
        .lock()
        .expect("strict lock test guard");
    std::env::set_var(homeboy_core::config::CONFIG_LOCK_STRICT_ENV, "1");
    struct StrictLockEnv;
    impl Drop for StrictLockEnv {
        fn drop(&mut self) {
            std::env::remove_var(homeboy_core::config::CONFIG_LOCK_STRICT_ENV);
        }
    }
    let _env = StrictLockEnv;
    test();
}

/// `agent_task_lifecycle::submit_plan` against an explicitly injected store.
///
/// The ambient wrapper resolves its store from the process environment and
/// admits through `paths::controller_runtimes_store()`, which stays
/// process-global by design. A hermetic test must not enqueue against the
/// operator's real admission queue, so runtime admission is supplied as stub
/// evidence exactly as the rooted proofs in `agent_task_lifecycle::tests` do.
/// Only the store and that admission edge change; the plan and the requested
/// run identity are passed through untouched (#7505).
fn submit_plan_in_test_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    agent_task_lifecycle::submit_plan_with_runtime_admission_in_store(
        lifecycle_store,
        plan,
        requested_run_id,
        None,
        None,
        None,
        |_| Ok(serde_json::json!({})),
    )
}

#[test]
fn deepest_typed_error_selects_the_deepest_explicit_cause() {
    let diagnostic = serde_json::json!({
        "cause": {
            "schema": "homeboy/command-result/v3",
            "success": false,
            "error": {
                "code": "promotion.rejected",
                "message": "promotion rejected",
                "details": {
                    "cause": {
                        "schema": "homeboy/command-result/v3",
                        "success": false,
                        "error": {
                            "code": "gate.failed",
                            "message": "gate failed",
                            "details": { "field": "verify" }
                        }
                    }
                }
            }
        }
    });

    assert_eq!(
        deepest_typed_error(&diagnostic),
        Some(serde_json::json!({
            "code": "gate.failed",
            "field": "verify",
            "message": "gate failed"
        }))
    );
}

#[test]
fn deepest_typed_error_ignores_sibling_envelopes_and_json_strings() {
    let primary = serde_json::json!({
        "schema": "homeboy/command-result/v3",
        "success": false,
        "error": { "code": "promotion.rejected", "message": "promotion rejected" }
    });
    let unrelated_sibling = serde_json::json!({
        "schema": "homeboy/command-result/v3",
        "success": false,
        "error": { "code": "unrelated", "message": "unrelated sibling" }
    });
    assert_eq!(
        deepest_typed_error(&serde_json::json!({
            "cause": primary,
            "provider_response": unrelated_sibling,
        })),
        Some(serde_json::json!({
            "code": "promotion.rejected",
            "field": null,
            "message": "promotion rejected"
        }))
    );
    assert!(deepest_typed_error(&serde_json::json!({
        "cause": r#"{"schema":"homeboy/command-result/v3","success":false,"error":{"code":"untrusted","message":"provider transcript"}}"#
    }))
    .is_none());
}

#[derive(Debug, Deserialize)]
struct DurableCookFixture {
    schema: String,
    producer: DurableCookFixtureProducer,
    cook: DurableCookFixtureCook,
    source_record: DurableCookFixtureSourceRecord,
    continuation_record: DurableCookFixtureContinuationRecord,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureProducer {
    runtime: String,
    run_record_schema: String,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureCook {
    id: String,
    source_attempt: u32,
    continuation_attempt: u32,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureSourceRecord {
    schema: String,
    state: String,
    latest_promotion: DurableCookFixturePromotion,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixturePromotion {
    status: String,
    post_apply: bool,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureContinuationRecord {
    schema: String,
    state: String,
    latest_promotion: DurableCookFixtureContinuationPromotion,
    aggregate: DurableCookFixtureAggregate,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureContinuationPromotion {
    follow_up_kind: String,
}

#[derive(Debug, Deserialize)]
struct DurableCookFixtureAggregate {
    status: String,
    outcome_status: String,
}

fn durable_cook_0_328_fixture() -> DurableCookFixture {
    let fixture: DurableCookFixture = serde_json::from_str(include_str!(
        "fixtures/durable_cook_0.328_review_form_timeout.json"
    ))
    .expect("0.328 durable Cook fixture is valid JSON");
    assert_eq!(fixture.schema, DURABLE_COOK_FIXTURE_SCHEMA);
    assert_eq!(fixture.producer.runtime, "0.328.1");
    assert_eq!(
        fixture.producer.run_record_schema,
        "homeboy/agent-task-run/v1"
    );
    fixture
}

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
        verification: Vec::new(),
        used_for: "Reproduced the failure, isolated the reload path, added a guard, and verified with the recorded deterministic gate before finalizing.".to_string(),
    }
}

/// The `outputs` object carrying a valid review form under `review_form`.
fn test_review_form_outputs() -> Value {
    serde_json::json!({ "review_form": test_review_form() })
}

#[test]
fn remediation_policy_rejects_denied_workspace_read_before_dispatch() {
    let mut request = AgentTaskRequest {
        schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: "gate-fix".to_string(),
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
        instructions: "Fix the failing gate.".to_string(),
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
    };

    assert_eq!(
        remediation_tool_policy_error(&request).as_deref(),
        Some("Cook remediation policy must grant the runner read access to the task workspace before dispatch")
    );

    request.policy.grant_workspace_read_tool();
    assert!(remediation_tool_policy_error(&request).is_none());
}

#[test]
fn terminal_review_form_continuation_rejects_generic_failed_and_cancelled_runs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "generic-terminal-review-form",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        for (run_id, cancelled) in [
            ("generic-terminal-review-form-failed", false),
            ("generic-terminal-review-form-cancelled", true),
        ] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
            if cancelled {
                agent_task_lifecycle::cancel_run(run_id, Some("policy cancelled")).unwrap();
            } else {
                agent_task_lifecycle::record_pre_execution_failure(
                    run_id,
                    &options.initial_plan,
                    "fixture",
                    &Error::validation_invalid_argument(
                        "fixture",
                        "generic policy failure",
                        None,
                        None,
                    ),
                )
                .unwrap();
            }
            let record = agent_task_lifecycle::status(run_id).unwrap();
            assert!(
                !terminal_review_form_continuation_is_eligible(&options.initial_plan, &record,)
                    .unwrap()
            );
        }
    });
}

#[test]
fn run_next_skips_persisted_test_detached_recipe_and_executes_eligible_work() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "run-next-ineligible-test-detached",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        persist_initial_recipe(&options).expect("persisted test-detached recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("declared Cook attempt submitted");
        agent_task_lifecycle::rewrite_record_for_test(&options.initial_run_id, |record| {
            record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
        })
        .expect("deterministic durable submission timestamp");
        agent_task_lifecycle::cancel_run(&options.initial_run_id, Some("fixture terminal"))
            .expect("declared Cook attempt terminal");
        super::super::enqueue_terminal_continuation(&options.cook_id, &options.initial_run_id)
            .expect("durable Cook continuation queued");

        agent_task_lifecycle::submit_plan(
            &batch_cook_options(
                "run-next-eligible-work",
                Arc::new(AcceptedDetachedAttemptDispatcher),
            )
            .initial_plan,
            Some("run-next-eligible-work"),
        )
        .expect("eligible work queued");

        let result = super::super::run_next_with_cook_dispatcher(
            Arc::new(ImmediateSuccessExecutor),
            |_| Ok(None),
            None,
        )
        .expect("ineligible continuation does not block eligible work");

        assert_eq!(
            result.value.expect("eligible aggregate").plan_id,
            "run-next-eligible-work"
        );
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].run_id, options.initial_run_id);
        assert_eq!(
            result.skipped[0].submitted_at.as_deref(),
            Some("2000-01-01T00:00:00+00:00")
        );
        assert!(result.skipped[0]
            .age_seconds
            .is_some_and(|age_seconds| age_seconds > 800_000_000));
        assert_eq!(
            result.skipped[0].dispatcher_kind.as_deref(),
            Some("test-detached")
        );
        assert_eq!(
            result.skipped[0].category,
            "cook_continuation_preflight_failed"
        );
        assert_eq!(result.skipped[0].error_code, "validation.invalid_argument");
        assert!(result.skipped[0].remediation.contains("agent-task status"));
    });
}

#[test]
fn scoped_run_next_claims_its_fanout_cook_continuation_by_exact_identity() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut options = batch_cook_options(
            "scoped-continuation",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_plan.metadata["batch_id"] = serde_json::json!("scoped-fanout");
        persist_initial_recipe(&options).expect("persisted recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("declared Cook attempt submitted");
        agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, &options.initial_run_id)
            .expect("Cook identity persisted");
        crate::agent_task_batch::persist_fanout_run_batch(
            "scoped-fanout",
            "scoped-fanout",
            &[crate::agent_task_batch::FanoutRunBatchChild {
                task_id: "child".to_string(),
                run_id: options.initial_run_id.clone(),
            }],
            serde_json::json!({}),
        )
        .expect("fanout persisted");
        agent_task_lifecycle::cancel_run(&options.initial_run_id, Some("fixture terminal"))
            .expect("attempt terminal");
        super::super::enqueue_terminal_continuation(&options.cook_id, &options.initial_run_id)
            .expect("continuation queued");
        let scope = crate::agent_task_batch::owned_child_run_ids("scoped-fanout")
            .expect("owned fanout child");

        let result = super::super::run_next_with_cook_dispatcher(
            Arc::new(ImmediateSuccessExecutor),
            |_| Ok(None),
            Some(&scope),
        )
        .expect("scoped continuation admission");

        assert_eq!(
            result.skipped.len(),
            1,
            "claimed continuation returns its diagnostic"
        );
        assert!(
            super::super::claim_continuation_for(&options.cook_id, &options.initial_run_id)
                .expect("continuation lookup")
                .is_none(),
            "the scoped claim consumed exactly this fanout continuation"
        );
    });
}

#[test]
fn scoped_run_next_skips_bad_fanout_continuation_and_claims_queued_child() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut bad = batch_cook_options(
            "scoped-bad-continuation",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        bad.initial_plan.metadata["batch_id"] = serde_json::json!("scoped-recovery");
        persist_initial_recipe(&bad).expect("persisted bad recipe");
        agent_task_lifecycle::submit_plan(&bad.initial_plan, Some(&bad.initial_run_id))
            .expect("bad attempt submitted");
        agent_task_lifecycle::record_cook_attempt(&bad.cook_id, 1, &bad.initial_run_id)
            .expect("bad Cook identity persisted");
        agent_task_lifecycle::cancel_run(&bad.initial_run_id, Some("fixture terminal"))
            .expect("bad attempt terminal");
        super::super::enqueue_terminal_continuation(&bad.cook_id, &bad.initial_run_id)
            .expect("bad continuation queued");

        let mut ready = batch_cook_options(
            "scoped-ready-child",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        )
        .initial_plan;
        ready.plan_id = "scoped-ready".to_string();
        ready.metadata["batch_id"] = serde_json::json!("scoped-recovery");
        agent_task_lifecycle::submit_plan(&ready, Some("scoped-ready-child"))
            .expect("ready child submitted");
        crate::agent_task_batch::persist_fanout_run_batch(
            "scoped-recovery",
            "scoped-recovery",
            &[
                crate::agent_task_batch::FanoutRunBatchChild {
                    task_id: "bad".to_string(),
                    run_id: bad.initial_run_id.clone(),
                },
                crate::agent_task_batch::FanoutRunBatchChild {
                    task_id: "ready".to_string(),
                    run_id: "scoped-ready-child".to_string(),
                },
            ],
            serde_json::json!({}),
        )
        .expect("fanout persisted");
        let scope = crate::agent_task_batch::owned_child_run_ids("scoped-recovery")
            .expect("owned children");

        let result = super::super::run_next_with_cook_dispatcher(
            Arc::new(ImmediateSuccessExecutor),
            |_| Ok(None),
            Some(&scope),
        )
        .expect("bad continuation does not block scoped queue admission");

        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].run_id, bad.initial_run_id);
        assert_eq!(
            result.value.expect("ready aggregate").plan_id,
            "scoped-ready"
        );
        assert_eq!(
            agent_task_lifecycle::status("scoped-ready-child")
                .expect("ready child status")
                .state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn run_next_redacts_poisoned_recipe_dispatcher_kind() {
    homeboy_core::test_support::with_isolated_home(|_| {
        const POISONED_KIND: &str = "LEAK_RECIPE_DISPATCHER_SECRET";
        let options = batch_cook_options(
            "run-next-poisoned-recipe",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        persist_initial_recipe(&options).expect("persisted recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("declared Cook attempt submitted");
        agent_task_lifecycle::cancel_run(&options.initial_run_id, Some("fixture terminal"))
            .expect("declared Cook attempt terminal");
        super::super::enqueue_terminal_continuation(&options.cook_id, &options.initial_run_id)
            .expect("durable Cook continuation queued");

        let recipe_path = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-cooks")
            .join(&options.cook_id)
            .join("recipe.json");
        let mut recipe: Value =
            serde_json::from_slice(&std::fs::read(&recipe_path).expect("persisted recipe bytes"))
                .expect("persisted recipe JSON");
        recipe["promotion_transport"]["attempt_dispatch"]["kind"] =
            serde_json::json!(POISONED_KIND);
        std::fs::write(
            &recipe_path,
            serde_json::to_vec(&recipe).expect("poisoned recipe JSON"),
        )
        .expect("poisoned recipe persisted");

        agent_task_lifecycle::submit_plan(
            &batch_cook_options(
                "run-next-after-poisoned-recipe",
                Arc::new(AcceptedDetachedAttemptDispatcher),
            )
            .initial_plan,
            Some("run-next-after-poisoned-recipe"),
        )
        .expect("eligible work queued");

        let result = super::super::run_next_with_cook_dispatcher(
            Arc::new(ImmediateSuccessExecutor),
            |_| Ok(None),
            None,
        )
        .expect("poisoned continuation does not block eligible work");
        let status = agent_task_lifecycle::status(&options.initial_run_id)
            .expect("continuation record status");
        let logs =
            agent_task_lifecycle::logs(&options.initial_run_id).expect("continuation record logs");
        let rendered = serde_json::to_string(&serde_json::json!({
            "queue_skips": result.skipped,
            "status": status,
            "logs": logs,
        }))
        .expect("queue projections serialize");

        assert_eq!(
            result.value.expect("eligible aggregate").plan_id,
            "run-next-after-poisoned-recipe"
        );
        assert!(!rendered.contains(POISONED_KIND));
        assert!(rendered.contains("cook_continuation_unsupported_dispatcher"));
        assert!(rendered.contains("agent-task status <run-id> --exact --full"));
    });
}

#[test]
fn malformed_continuation_does_not_head_of_line_block_run_next() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let queue = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-cook-continuations");
        std::fs::create_dir_all(&queue).expect("continuation queue");
        std::fs::write(queue.join("000-malformed.pending"), "not JSON")
            .expect("malformed continuation persisted");
        agent_task_lifecycle::submit_plan(
            &batch_cook_options(
                "run-next-after-malformed-continuation",
                Arc::new(AcceptedDetachedAttemptDispatcher),
            )
            .initial_plan,
            Some("run-next-after-malformed-continuation"),
        )
        .expect("eligible work queued");

        let result = super::super::run_next_with_cook_dispatcher(
            Arc::new(ImmediateSuccessExecutor),
            |_| Ok(None),
            None,
        )
        .expect("malformed continuation is skipped");

        assert_eq!(
            result.value.expect("eligible aggregate").plan_id,
            "run-next-after-malformed-continuation"
        );
        assert!(queue.join("000-malformed.failed").is_file());
    });
}

#[test]
fn durable_cook_inspection_reports_an_unsupported_run_schema() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let options = batch_cook_options(
            "unsupported-durable-cook-schema",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        let run_id = "unsupported-durable-cook-schema-attempt-1";
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::inject_raw_record_metadata_for_corruption_test(run_id, |metadata| {
            metadata["agent_task_run"]["schema"] = serde_json::json!("homeboy/agent-task-run/v999");
        })
        .unwrap();

        let error = agent_task_lifecycle::status(run_id)
            .expect_err("unsupported durable record schema must be diagnosable");
        assert!(error
            .message
            .contains("unsupported durable agent-task run schema"));
        assert!(error.message.contains("homeboy/agent-task-run/v999"));
    });
}

fn seed_review_form_aggregate(run_id: &str, plan: &AgentTaskPlan) {
    let aggregate = review_form_aggregate(plan);
    agent_task_lifecycle::record_run_aggregate(run_id, plan, &aggregate).unwrap();
}

fn review_form_aggregate(plan: &AgentTaskPlan) -> crate::agent_task_scheduler::AgentTaskAggregate {
    use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };
    let form = test_review_form();
    let task = plan.tasks.first().expect("review form plan has one task");
    AgentTaskAggregate {
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
    }
}

fn seed_timeout_review_form_aggregate(run_id: &str, plan: &AgentTaskPlan) {
    use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    let task = plan.tasks.first().expect("review form plan has one task");
    agent_task_lifecycle::record_run_aggregate(
        run_id,
        plan,
        &AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Failed,
            totals: AgentTaskAggregateTotals {
                failed: 1,
                ..Default::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: task.task_id.clone(),
                status: AgentTaskOutcomeStatus::Timeout,
                summary: Some("review form provider timed out".to_string()),
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
                metadata: serde_json::json!({ "model": task.executor.model() }),
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

fn seed_patch_alias_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    patches: &[(&str, &std::path::Path, &str)],
) {
    let patches = patches
        .iter()
        .map(|(id, path, patch)| {
            (
                *id,
                *path,
                *patch,
                serde_json::json!({
                    "producer_attempt": 2,
                    "base_ref": "main",
                    "provider_backend": "fixture",
                }),
            )
        })
        .collect::<Vec<_>>();
    let lifecycle_store =
        AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
    seed_patch_alias_aggregate_with_metadata(&lifecycle_store, run_id, plan, &patches, Vec::new());
}

fn seed_patch_alias_aggregate_with_metadata(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    patches: &[(&str, &std::path::Path, &str, Value)],
    diagnostics: Vec<crate::agent_task::AgentTaskDiagnostic>,
) {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    let task = plan.tasks.first().expect("candidate plan has one task");
    let artifacts = patches
        .iter()
        .map(|(id, path, patch, metadata)| {
            std::fs::write(path, patch).expect("write candidate patch");
            AgentTaskArtifact {
                id: (*id).to_string(),
                kind: "patch".to_string(),
                path: Some(path.display().to_string()),
                size_bytes: Some(patch.len() as u64),
                sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                    patch.as_bytes(),
                )),
                metadata: metadata.clone(),
                ..Default::default()
            }
        })
        .collect();
    lifecycle_store
        .record_run_aggregate(
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
                    artifacts,
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics,
                    outputs: test_review_form_outputs(),
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
        .expect("persist candidate aggregate");
}

fn seed_patch_alias_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    patches: &[(&str, &std::path::Path, &str)],
) {
    use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    };

    let task = plan.tasks.first().expect("candidate plan has one task");
    let artifacts = patches
        .iter()
        .map(|(id, path, patch)| {
            std::fs::write(path, patch).expect("write candidate patch");
            AgentTaskArtifact {
                id: (*id).to_string(),
                kind: "patch".to_string(),
                path: Some(path.display().to_string()),
                size_bytes: Some(patch.len() as u64),
                sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                    patch.as_bytes(),
                )),
                metadata: serde_json::json!({
                    "producer_attempt": 2,
                    "base_ref": "main",
                    "provider_backend": "fixture",
                }),
                ..Default::default()
            }
        })
        .collect();
    lifecycle_store
        .record_run_aggregate(
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
                    artifacts,
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: test_review_form_outputs(),
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
        .expect("persist candidate aggregate");
}

#[test]
fn cook_selects_successful_rotated_patch_and_collapses_equivalent_aliases() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-canonical-rotated-alias";
        let timed_out = "cook-canonical-rotated-alias-attempt-1";
        let successful = "cook-canonical-rotated-alias-attempt-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = timed_out.to_string();
        persist_initial_recipe(&options).unwrap();
        super::super::record_recipe_attempt(cook_id, 2, successful, &options.initial_plan).unwrap();
        for (attempt, run_id) in [(1, timed_out), (2, successful)] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id).unwrap();
        }
        seed_timeout_review_form_aggregate(timed_out, &options.initial_plan);
        let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        seed_patch_alias_aggregate(
            successful,
            &options.initial_plan,
            &[
                ("patch", &temp.path().join("patch"), patch),
                ("provider-alias", &temp.path().join("alias"), patch),
            ],
        );

        assert_eq!(
            canonical_cook_patch_artifact_id(&options, successful).unwrap(),
            Some("patch".to_string())
        );
    });
}

#[test]
fn cook_requires_selection_for_distinct_canonical_patches_before_promotion() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-canonical-distinct";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        let mut options = options;
        options.gates.private_verify = vec!["private gate".to_string()];
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .unwrap();
        seed_patch_alias_aggregate(
            &options.initial_run_id,
            &options.initial_plan,
            &[
                (
                    "patch-a",
                    &temp.path().join("a"),
                    "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+one\n",
                ),
                (
                    "patch-b",
                    &temp.path().join("b"),
                    "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-old\n+two\n",
                ),
            ],
        );
        let error = canonical_cook_patch_artifact_id(&options, &options.initial_run_id)
            .expect_err("distinct candidates require a choice");
        assert_eq!(error.details["state"], "selection_required");
        assert_eq!(error.details["choices"].as_array().unwrap().len(), 2);
        assert!(error.details["choices"][0]["command"]
            .as_str()
            .unwrap()
            .contains("--private-verify"));
    });
}

#[test]
fn cook_selection_comparison_projects_three_distinct_candidates_from_patch_artifacts() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let options = batch_cook_options(
            "cook-semantic-candidates",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("submit candidate plan");
        seed_patch_alias_aggregate(
            &options.initial_run_id,
            &options.initial_plan,
            &[
                (
                    "patch-a",
                    &temp.path().join("a"),
                    "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+one\n",
                ),
                (
                    "patch-b",
                    &temp.path().join("b"),
                    "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+two\n",
                ),
                (
                    "patch-c",
                    &temp.path().join("c"),
                    "diff --git a/.github/workflows/test.yml b/.github/workflows/test.yml\n--- a/.github/workflows/test.yml\n+++ b/.github/workflows/test.yml\n@@ -1 +1 @@\n-old\n+token=fixture\n",
                ),
            ],
        );

        let error = canonical_cook_patch_artifact_id(&options, &options.initial_run_id)
            .expect_err("distinct candidates require a choice");
        let choices = error.details["choices"]
            .as_array()
            .expect("comparison choices");
        assert_eq!(choices.len(), 3);
        let patch_a = choices
            .iter()
            .find(|choice| choice["artifact_id"] == "patch-a")
            .expect("patch a comparison");
        let patch_b = choices
            .iter()
            .find(|choice| choice["artifact_id"] == "patch-b")
            .expect("patch b comparison");
        let patch_c = choices
            .iter()
            .find(|choice| choice["artifact_id"] == "patch-c")
            .expect("patch c comparison");
        assert_eq!(patch_a["changed_files"], serde_json::json!(["src/lib.rs"]));
        assert_eq!(
            patch_a["line_stats"],
            serde_json::json!({ "insertions": 1, "deletions": 1 })
        );
        assert_eq!(
            patch_a["diff_summary"],
            "1 file(s), 1 insertion(s), 1 deletion(s)"
        );
        assert_eq!(
            patch_a["overlap"]["shared_changed_files"],
            serde_json::json!([]),
            "all three candidates must share a file before it is reported as common"
        );
        assert!(patch_a["risk_flags"]
            .as_array()
            .expect("risk flags")
            .contains(&serde_json::json!("missing_test_evidence")));
        assert!(patch_b["risk_flags"]
            .as_array()
            .expect("risk flags")
            .contains(&serde_json::json!("missing_test_evidence")));
        assert!(patch_c["risk_flags"]
            .as_array()
            .expect("risk flags")
            .contains(&serde_json::json!("security_sensitive_automation_change")));
        assert!(patch_c["risk_flags"]
            .as_array()
            .expect("risk flags")
            .contains(&serde_json::json!("sensitive_literal_pattern_added")));
        for choice in choices {
            assert!(choice["patch_artifact"]["path"].is_string());
            assert!(choice["provider"].is_string());
            assert!(choice["attempt"].is_number());
            assert!(choice["test_evidence"].is_array());
            assert!(choice.get("diagnostics").is_none());
            assert!(choice["command"]
                .as_str()
                .expect("promotion command")
                .contains("--artifact-id"));
        }
        assert!(error.details["comparison"]["shared_outcome_diagnostics"].is_array());
    });
}

#[test]
fn cook_selection_risks_use_files_beyond_the_preview_limit() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let options = batch_cook_options(
            "cook-preview-risk",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .unwrap();
        let mut late_security = String::new();
        for index in 0..12 {
            late_security.push_str(&format!("diff --git a/src/{index}.rs b/src/{index}.rs\n--- a/src/{index}.rs\n+++ b/src/{index}.rs\n@@ -1 +1 @@\n-old\n+new\n"));
        }
        late_security.push_str("diff --git a/zz/Dockerfile b/zz/Dockerfile\n--- a/zz/Dockerfile\n+++ b/zz/Dockerfile\n@@ -1 +1 @@\n-old\n+new\n");
        seed_patch_alias_aggregate(
            &options.initial_run_id,
            &options.initial_plan,
            &[
                (
                    "safe",
                    &temp.path().join("safe"),
                    "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
                ),
                ("late-risk", &temp.path().join("late-risk"), &late_security),
            ],
        );
        let error =
            canonical_cook_patch_artifact_id(&options, &options.initial_run_id).unwrap_err();
        let candidate = error.details["choices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|choice| choice["artifact_id"] == "late-risk")
            .unwrap();
        assert_eq!(candidate["changed_file_count"], 13);
        assert_eq!(candidate["changed_files_omitted_count"], 1);
        assert!(candidate["risk_flags"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("security_sensitive_automation_change")));
    });
}

#[test]
fn cook_selection_candidate_evidence_does_not_bless_other_candidates() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let options = batch_cook_options(
            "cook-evidence-attribution",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .unwrap();
        let metadata = |test_evidence: Value| {
            serde_json::json!({
                "producer_attempt": 2, "base_ref": "main", "provider_backend": "fixture", "test_evidence": test_evidence,
            })
        };
        seed_patch_alias_aggregate_with_metadata(
            &AgentTaskLifecycleStore::from_current_environment().unwrap(),
            &options.initial_run_id,
            &options.initial_plan,
            &[
                (
                    "a",
                    &temp.path().join("a"),
                    "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+one\n",
                    metadata(serde_json::json!([{ "command": "cargo test", "exit_code": 0 }])),
                ),
                (
                    "b",
                    &temp.path().join("b"),
                    "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-old\n+two\n",
                    metadata(serde_json::json!([])),
                ),
            ],
            Vec::new(),
        );
        let error =
            canonical_cook_patch_artifact_id(&options, &options.initial_run_id).unwrap_err();
        let choices = error.details["choices"].as_array().unwrap();
        let a = choices
            .iter()
            .find(|choice| choice["artifact_id"] == "a")
            .unwrap();
        let b = choices
            .iter()
            .find(|choice| choice["artifact_id"] == "b")
            .unwrap();
        assert!(a["recommendation"].is_object());
        assert!(b["test_evidence"].as_array().unwrap().is_empty());
        assert!(b["recommendation"].is_null());
    });
}

#[test]
fn cook_selection_bounds_candidate_inventory_and_oversized_diagnostics() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let options = batch_cook_options(
            "cook-bounded-inventory",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .unwrap();
        let patches = (0..18)
            .map(|index| {
                let id = format!("candidate-{index:02}");
                let path = temp.path().join(&id);
                let patch = format!(
                    "diff --git a/{id} b/{id}\n--- a/{id}\n+++ b/{id}\n@@ -1 +1 @@\n-old\n+new\n"
                );
                (
                    id,
                    path,
                    patch,
                    serde_json::json!({ "producer_attempt": 2, "provider_backend": "fixture" }),
                )
            })
            .collect::<Vec<_>>();
        let refs = patches
            .iter()
            .map(|(id, path, patch, metadata)| {
                (
                    id.as_str(),
                    path.as_path(),
                    patch.as_str(),
                    metadata.clone(),
                )
            })
            .collect::<Vec<_>>();
        seed_patch_alias_aggregate_with_metadata(
            &AgentTaskLifecycleStore::from_current_environment().unwrap(),
            &options.initial_run_id,
            &options.initial_plan,
            &refs,
            vec![crate::agent_task::AgentTaskDiagnostic {
                class: "fixture".to_string(),
                message: "x".repeat(4096),
                data: Value::Null,
            }],
        );
        let error =
            canonical_cook_patch_artifact_id(&options, &options.initial_run_id).unwrap_err();
        assert_eq!(error.details["choices"].as_array().unwrap().len(), 16);
        assert_eq!(error.details["comparison"]["omitted_candidate_count"], 2);
        assert_eq!(
            error.details["comparison"]["shared_outcome_diagnostics"][0]["omitted"],
            "json_value_exceeds_byte_limit"
        );
    });
}

#[test]
fn cook_selection_command_shell_round_trips_the_full_promotion_contract() {
    use std::collections::BTreeMap;

    let mut options = batch_cook_options(
        "cook shell contract",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    );
    options.to_worktree = "repo@candidate worktree".to_string();
    options.base = "release branch".to_string();
    options.provider_invocation = Some(homeboy_core::command_invocation::CommandInvocation {
        argv: vec![
            "provider tool".to_string(),
            "--prompt=fix 'quoted' value".to_string(),
        ],
        ..Default::default()
    });
    options.gates = crate::agent_task_gate::VerifyGateOptions {
        verify: vec!["cargo test --package 'space name'".to_string()],
        private_verify: vec!["private gate --token '$SAFE'".to_string()],
        gate_environment: crate::agent_task_gate::AgentTaskGateEnvironmentPolicy {
            mode: crate::agent_task_gate::AgentTaskGateEnvironmentMode::Replace,
            variables: BTreeMap::from([("MESSAGE".to_string(), "hello world".to_string())]),
            preserve: BTreeMap::from([("TOOL_HOME".to_string(), "HOME/.tool path".to_string())]),
            extension_inputs: vec![serde_json::from_value(serde_json::json!({
                "id": "extension input",
                "source": "/tmp/input with spaces",
            }))
            .unwrap()],
            ..Default::default()
        },
        gate_package_artifacts: vec![serde_json::from_value(serde_json::json!({
            "id": "package artifact",
            "environment": {"name": "PACKAGE_PATH", "default": "/tmp/package with spaces"},
            "required_paths": [{"path": "artifact file.json"}],
            "remediation": {"command": "install --with spaces"},
        }))
        .unwrap()],
        gate_toolchains: vec![crate::agent_task_gate::AgentTaskGateToolchainRequirement {
            command: "tool with spaces".to_string(),
            probe_arguments: vec!["probe".to_string(), "--format=json".to_string()],
        }],
        ..Default::default()
    };

    let expected = cook_promotion_argv(&options, "run id", "task id", "patch id");
    let rendered = super::super::cook_promotion::cook_promotion_command(
        &options, "run id", "task id", "patch id",
    );
    let output = Command::new("sh")
        .args(["-c", &format!("set -- {rendered}; printf '%s\\0' \"$@\"")])
        .output()
        .expect("execute rendered command through POSIX shell");
    assert!(output.status.success());
    let actual = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8(argument.to_vec()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert!(actual
        .windows(2)
        .any(|args| args == ["--verify", "cargo test --package 'space name'"]));
    assert!(actual
        .windows(2)
        .any(|args| args == ["--private-verify", "private gate --token '$SAFE'"]));
    assert!(actual
        .windows(2)
        .any(|args| args == ["--gate-env", "MESSAGE=hello world"]));
    assert!(actual.contains(&"--provider-argv=provider tool".to_string()));
    assert!(actual.contains(&"--provider-argv=--prompt=fix 'quoted' value".to_string()));
    let toolchain_spec = actual
        .windows(2)
        .find(|args| args[0] == "--gate-toolchain-spec")
        .map(|args| {
            serde_json::from_str::<crate::agent_task_gate::AgentTaskGateToolchainRequirement>(
                &args[1],
            )
            .unwrap()
        })
        .expect("non-default toolchain probe is rendered as an exact JSON contract");
    assert_eq!(toolchain_spec.probe_arguments, ["probe", "--format=json"]);
}

#[derive(Clone)]
struct CanonicalSelectionSideEffects {
    promotions: Arc<AtomicUsize>,
    selected_artifact: Arc<Mutex<Option<String>>>,
}

impl CookSideEffectService for CanonicalSelectionSideEffects {
    fn promote(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        options: &AgentTaskCookServiceOptions,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport> {
        self.promotions.fetch_add(1, Ordering::SeqCst);
        let artifact = canonical_cook_patch_artifact_id(options, run_id)?;
        *self.selected_artifact.lock().unwrap() = artifact;
        Ok(promotion(run_id))
    }

    fn recover_moving_base(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        _options: &AgentTaskCookServiceOptions,
        _recovery: &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport> {
        unreachable!("canonical selection stops before moving-base recovery")
    }

    fn finalize(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        _options: &AgentTaskCookServiceOptions,
        _run_id: &str,
        _promotion: &AgentTaskPromotionReport,
    ) -> Result<Value> {
        unreachable!("no-finalize Cook must not finalize")
    }
}

struct SelectionRequiredSideEffects;

impl CookSideEffectService for SelectionRequiredSideEffects {
    fn promote(
        &mut self,
        lifecycle_store: &AgentTaskLifecycleStore,
        _options: &AgentTaskCookServiceOptions,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport> {
        let aggregate = lifecycle_store.read_aggregate(run_id)?;
        let choices = aggregate
            .outcomes
            .iter()
            .flat_map(|outcome| outcome.artifacts.iter())
            .map(|artifact| serde_json::json!({ "artifact_id": artifact.id }))
            .collect::<Vec<_>>();
        assert_eq!(choices.len(), 2, "rooted aggregate has distinct candidates");
        Err(homeboy_core::Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            "Cook found distinct canonical patch candidates; select one before promotion",
            serde_json::json!({
                "field": "artifact_id",
                "state": "selection_required",
                "selection_required": true,
                "choices": choices,
            }),
        ))
    }

    fn recover_moving_base(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        _options: &AgentTaskCookServiceOptions,
        _recovery: &MovingBaseCookRecovery,
    ) -> Result<AgentTaskPromotionReport> {
        unreachable!("selection-required Cook must not recover a moving base")
    }

    fn finalize(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        _options: &AgentTaskCookServiceOptions,
        _run_id: &str,
        _promotion: &AgentTaskPromotionReport,
    ) -> Result<Value> {
        unreachable!("selection-required Cook must not finalize")
    }
}

#[test]
fn cook_promotes_the_rotated_success_after_a_retained_timeout_with_aliases_collapsed() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let cook_id = "cook-full-rotated-alias";
        let timed_out = "cook-full-rotated-alias-attempt-1";
        let successful = "cook-full-rotated-alias-attempt-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.max_attempts = 2;
        persist_initial_recipe(&options).unwrap();
        super::super::record_recipe_attempt(cook_id, 2, successful, &options.initial_plan).unwrap();
        for (attempt, run_id) in [(1, timed_out), (2, successful)] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id).unwrap();
        }
        seed_timeout_review_form_aggregate(timed_out, &options.initial_plan);
        let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        seed_patch_alias_aggregate(
            successful,
            &options.initial_plan,
            &[
                ("patch", &temp.path().join("patch"), patch),
                ("provider-alias", &temp.path().join("alias"), patch),
            ],
        );
        let mut resumed_options = options.clone();
        resumed_options.initial_run_id = successful.to_string();
        let promotions = Arc::new(AtomicUsize::new(0));
        let selected_artifact = Arc::new(Mutex::new(None));
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(CanonicalSelectionSideEffects {
                promotions: Arc::clone(&promotions),
                selected_artifact: Arc::clone(&selected_artifact),
            })),
            ..CookContext::new(resumed_options, Arc::new(UnusedExecutor))
        })
        .unwrap();
        assert_eq!(result.value.status, "green_no_finalize");
        assert_eq!(promotions.load(Ordering::SeqCst), 1);
        assert_eq!(
            *selected_artifact.lock().unwrap(),
            Some("patch".to_string())
        );
    });
}

#[test]
fn cook_persists_selection_required_before_promotion_or_gates() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let cook_id = "cook-full-selection-required";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &options.initial_run_id).unwrap();
        seed_patch_alias_aggregate(
            &options.initial_run_id,
            &options.initial_plan,
            &[
                (
                    "patch-a",
                    &temp.path().join("a"),
                    "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+one\n",
                ),
                (
                    "patch-b",
                    &temp.path().join("b"),
                    "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-old\n+two\n",
                ),
            ],
        );
        let promotions = Arc::new(AtomicUsize::new(0));
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(CanonicalSelectionSideEffects {
                promotions: Arc::clone(&promotions),
                selected_artifact: Arc::new(Mutex::new(None)),
            })),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
        })
        .unwrap();
        let record = agent_task_lifecycle::status(&options.initial_run_id).unwrap();
        assert_eq!(result.value.status, "selection_required");
        assert_eq!(promotions.load(Ordering::SeqCst), 1);
        assert!(record.metadata.get("latest_promotion").is_none());
        assert_eq!(
            record.metadata["cook_selection_required"]["state"],
            "selection_required"
        );
        assert_eq!(
            record.metadata["cook_selection_required"]["choices"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    });
}

#[test]
fn cook_selection_required_metadata_uses_supplied_lifecycle_store() {
    let _ambient_context = homeboy_core::test_support::HomeGuard::new();
    let supplied_context = homeboy_core::test_support::HermeticTestContext::new();
    let ambient_store =
        AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
    let recipe_store = CookRecipeStore::new(supplied_context.path_roots());
    let supplied_store = AgentTaskLifecycleStore::new(supplied_context.path_roots());
    let temp = tempfile::tempdir().expect("candidate artifacts");
    let cook_id = "same-selection-required-cook";
    let run_id = "same-selection-required-run";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = run_id.to_string();

    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist supplied recipe");
    for store in [&ambient_store, &supplied_store] {
        store
            .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
                Ok(serde_json::json!({}))
            })
            .expect("seed lifecycle record");
    }
    supplied_store
        .record_cook_attempt(cook_id, 1, run_id)
        .expect("record supplied Cook attempt");
    seed_patch_alias_aggregate_in_store(
        &supplied_store,
        run_id,
        &options.initial_plan,
        &[
            (
                "patch-a",
                &temp.path().join("a"),
                "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+one\n",
            ),
            (
                "patch-b",
                &temp.path().join("b"),
                "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-old\n+two\n",
            ),
        ],
    );
    supplied_store
        .record_metadata_value(
            run_id,
            "latest_promotion",
            serde_json::json!({ "status": "verification_pending" }),
        )
        .expect("seed rooted verification-pending continuation");
    ambient_store
        .record_metadata_value(
            run_id,
            "controller_admission",
            serde_json::json!({
                "state": "none",
                "owner": null,
                "position": null,
                "requested_at_ms": null,
                "wait_duration_ms": null,
            }),
        )
        .expect("seed ambient runtime-promotion projection");
    let ambient_before = ambient_store
        .read_record(run_id)
        .expect("read untouched root");

    let result = run_cook_spine(
        &recipe_store,
        &supplied_store,
        options,
        Arc::new(UnusedExecutor),
        &mut SelectionRequiredSideEffects,
        None,
        false,
    )
    .expect("Cook reports selection-required promotion failure");

    let supplied_record = supplied_store
        .read_record(run_id)
        .expect("read supplied root");
    let ambient_record = ambient_store
        .read_record(run_id)
        .expect("read untouched root");
    assert_eq!(result.value.status, "selection_required");
    assert_eq!(
        supplied_record.metadata["cook_selection_required"]["state"],
        "selection_required"
    );
    assert!(supplied_record
        .metadata
        .get("cook_controller_failure")
        .is_some());
    assert_eq!(ambient_record.metadata, ambient_before.metadata);
    assert_eq!(ambient_record.updated_at, ambient_before.updated_at);
}

#[test]
fn candidate_selection_uses_the_winner_for_review_form_and_status_projection() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
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
    submit_plan_in_test_store(&lifecycle_store, &plan, Some(run_id)).unwrap();
    agent_task_lifecycle::record_run_aggregate_in_store(
        &lifecycle_store,
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
        review_form_from_aggregate(
            &agent_task_lifecycle::read_aggregate_in_store(&lifecycle_store, run_id).unwrap()
        )
        .unwrap(),
        Some(test_review_form())
    );
    assert_eq!(
        selected_candidate_task_id_in_store(&lifecycle_store, run_id).unwrap(),
        Some("winner".to_string())
    );
    let status = agent_task_lifecycle::run_status_in_store(&lifecycle_store, run_id, None).unwrap();
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
fn pre_artifact_interruption_claim_isolated_store_pairs_recover_identical_ids() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
    let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
    let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-pre-artifact-cook";
    let run_id = "same-pre-artifact-run";
    let mut left_options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    left_options.initial_run_id = run_id.to_string();
    left_options.initial_plan.plan_id = "left-pre-artifact-plan".to_string();
    let mut right_options =
        batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    right_options.initial_run_id = run_id.to_string();
    right_options.initial_plan.plan_id = "right-pre-artifact-plan".to_string();
    left_recipe_store
        .persist_initial_recipe(&left_options)
        .expect("persist left recipe");
    let left_plan = left_recipe_store
        .load_recipe(cook_id)
        .expect("load left recipe")
        .attempts[0]
        .plan
        .clone();
    right_recipe_store
        .persist_initial_recipe(&right_options)
        .expect("persist right recipe");
    let right_plan = right_recipe_store
        .load_recipe(cook_id)
        .expect("load right recipe")
        .attempts[0]
        .plan
        .clone();
    left_lifecycle_store
        .submit_plan_with_runtime_admission(&left_plan, run_id, |_| {
            Ok(serde_json::json!({ "store": "left" }))
        })
        .expect("seed left lifecycle");
    right_lifecycle_store
        .submit_plan_with_runtime_admission(&right_plan, run_id, |_| {
            Ok(serde_json::json!({ "store": "right" }))
        })
        .expect("seed right lifecycle");
    let barrier = Arc::new(Barrier::new(2));

    let (left_result, right_result) = std::thread::scope(|scope| {
        let left_barrier = Arc::clone(&barrier);
        let recipe_store = left_recipe_store.clone();
        let lifecycle_store = left_lifecycle_store.clone();
        let plan = left_plan.clone();
        let left = scope.spawn(move || {
            left_barrier.wait();
            claim_pre_artifact_interruption_retry_with_stores(
                (&recipe_store, &lifecycle_store),
                cook_id,
                1,
                run_id,
                &plan,
                false,
            )
            .expect("claim left retry")
            .expect("left retry acquired")
        });
        let right_barrier = Arc::clone(&barrier);
        let recipe_store = right_recipe_store.clone();
        let lifecycle_store = right_lifecycle_store.clone();
        let plan = right_plan.clone();
        let right = scope.spawn(move || {
            right_barrier.wait();
            claim_pre_artifact_interruption_retry_with_stores(
                (&recipe_store, &lifecycle_store),
                cook_id,
                1,
                run_id,
                &plan,
                false,
            )
            .expect("claim right retry")
            .expect("right retry acquired")
        });
        (left.join().unwrap(), right.join().unwrap())
    });

    assert_eq!(left_result.0, 2);
    assert_eq!(right_result.0, 2);
    assert_ne!(left_result.1, right_result.1);
    for (recipe_store, lifecycle_store, plan, expected_plan_id, expected_result) in [
        (
            &left_recipe_store,
            &left_lifecycle_store,
            &left_plan,
            "left-pre-artifact-plan",
            &left_result,
        ),
        (
            &right_recipe_store,
            &right_lifecycle_store,
            &right_plan,
            "right-pre-artifact-plan",
            &right_result,
        ),
    ] {
        let persisted = recipe_store.load_recipe(cook_id).expect("load recipe");
        assert_eq!(persisted.attempts[1].plan, *plan);
        let persisted_claim = lifecycle_store
            .operation_claim(run_id, &pre_artifact_interruption_operation_key(run_id))
            .expect("read completed operation claim")
            .expect("completed operation claim exists");
        assert_eq!(
            persisted_claim.result.as_ref().unwrap()["next_run_id"],
            expected_result.1
        );
        let resumed = claim_pre_artifact_interruption_retry_with_stores(
            (recipe_store, lifecycle_store),
            cook_id,
            1,
            run_id,
            plan,
            false,
        )
        .expect("resume completed claim")
        .expect("completed retry retained");
        assert_eq!(&resumed, expected_result);
        let recipe = recipe_store.load_recipe(cook_id).expect("load recipe");
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].plan.plan_id, expected_plan_id);
        let claim = lifecycle_store
            .operation_claim(run_id, &pre_artifact_interruption_operation_key(run_id))
            .expect("read operation claim")
            .expect("operation claim exists");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
        assert_eq!(
            claim.result.as_ref().unwrap()["next_run_id"],
            expected_result.1
        );
    }
    assert_ne!(
        left_lifecycle_store.run_dir(run_id),
        right_lifecycle_store.run_dir(run_id)
    );
}

#[test]
fn cook_spine_materializes_into_the_injected_stores_across_split_recipe_and_lifecycle_roots() {
    let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(recipe_context.data_dir(), lifecycle_context.data_dir());

    let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(lifecycle_context.path_roots());

    let cook_id = "split-root-spine-cook";
    let run_id = "split-root-spine-run";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = run_id.to_string();
    // The candidate-group preflight is the first purely local boundary the spine
    // reaches after materialization, so an ambiguous candidate group stops this
    // Cook exactly there. Everything past it — the runtime generation pin, the
    // dispatch loop's controller-plan and attempt writes — still reaches
    // process-global lifecycle state and is not part of this seam yet (#7505).
    let mut sibling = options.initial_plan.tasks[0].clone();
    sibling.task_id = "sibling".to_string();
    options.initial_plan.tasks.push(sibling);

    let error = run_cook_spine(
        &recipe_store,
        &lifecycle_store,
        options,
        Arc::new(UnusedExecutor),
        &mut DefaultCookSideEffects::new(|_, _, _, _| Ok(serde_json::json!({}))),
        None,
        false,
    )
    .expect_err("the spine stops at its candidate-group preflight");
    assert_eq!(error.details["field"], "group_key");

    // The recipe landed under the recipe root.
    assert!(recipe_store.recipe_exists(cook_id));
    assert_eq!(
        recipe_store
            .load_recipe(cook_id)
            .expect("read recipe in the recipe root")
            .attempts[0]
            .run_id,
        run_id
    );

    // The run record and the Cook index landed under the lifecycle root.
    assert_eq!(
        lifecycle_store
            .read_cook_index(cook_id)
            .expect("read cook index in the lifecycle root")
            .latest_run_id,
        run_id
    );
    assert_eq!(
        lifecycle_store
            .read_record(run_id)
            .expect("read run record in the lifecycle root")
            .run_id,
        run_id
    );
    assert!(lifecycle_store
        .run_dir(run_id)
        .starts_with(lifecycle_context.data_dir()));
    assert!(lifecycle_store
        .cook_index_path(cook_id)
        .starts_with(lifecycle_context.data_dir()));

    // The negatives: neither root holds the other's durable state, so the
    // injected pair — not an ambient root — decided where every write went.
    let recipe_root_lifecycle_store = AgentTaskLifecycleStore::new(recipe_context.path_roots());
    assert!(!recipe_root_lifecycle_store
        .record_exists(run_id)
        .expect("no run record in the recipe root"));
    assert!(!recipe_root_lifecycle_store.cook_index_exists(cook_id));
    let lifecycle_root_recipe_store = CookRecipeStore::new(lifecycle_context.path_roots());
    assert!(!lifecycle_root_recipe_store.recipe_exists(cook_id));
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
        scoped_follow_up_budget(scope, &code_budget, consumed_code_budget, None);
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
        "max_same_provider_retries": 1,
        "max_provider_rotations": 0,
        "review_plan_provider_executions": 1,
    });
    let persisted_authority =
        source_request.inputs["cook_loop"]["execution_budget_authority"].clone();
    let (narrowed_budget, _) = scoped_follow_up_budget(
        scope,
        &code_budget,
        consumed_code_budget,
        Some(&persisted_authority),
    );
    assert_eq!(
        narrowed_budget,
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(2, 1, 0),
        "the canonical review authority remains bounded"
    );
    assert_eq!(
        follow_up_budget_scope(&source_request, &follow_up_request),
        CookFollowUpBudgetScope::FreshCookReview,
        "a review-only retry reuses its persisted review allowance"
    );
    follow_up_request.inputs["cook_loop"]["execution_budget_authority"] =
        persisted_authority.clone();
    assert_eq!(
        follow_up_request.inputs["cook_loop"]["execution_budget_authority"], persisted_authority,
        "cook-continue carries the exact authority instead of minting one"
    );

    let timed_out_usage = ExecutionBudgetUsage {
        executions: 1,
        ..Default::default()
    };
    let remaining_after_timeout = budget_remaining(&review_budget, timed_out_usage).unwrap();
    assert_eq!(
        remaining_after_timeout,
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 1, 0),
        "the timed-out review execution leaves exactly one same-provider retry"
    );
    assert_eq!(
        reserve_remediation_budget(&remaining_after_timeout, true).unwrap(),
        ExecutionBudgetUsage {
            same_provider_retries: 1,
            ..Default::default()
        }
    );
    assert!(budget_remaining(
        &review_budget,
        ExecutionBudgetUsage {
            executions: 2,
            same_provider_retries: 1,
            provider_rotations: 0,
        }
    )
    .is_none());

    follow_up_request.inputs["cook_loop"]["review_form_required"] = serde_json::json!(false);
    assert_eq!(
        follow_up_budget_scope(&source_request, &follow_up_request),
        CookFollowUpBudgetScope::Cook
    );
}

#[test]
fn cook_materialization_capacity_targets_the_explicit_lifecycle_scratch_root() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("source"), "fixture").unwrap();

    let left = reserve_cook_materialization_capacity(&left_store, workspace.path()).unwrap();
    let right = reserve_cook_materialization_capacity(&right_store, workspace.path()).unwrap();

    assert_eq!(left.root(), left_store.data_root().canonicalize().unwrap());
    assert_eq!(
        right.root(),
        right_store.data_root().canonicalize().unwrap()
    );
    assert_ne!(left.root(), right.root());
}

#[test]
fn gate_feedback_child_budget_preserves_declared_retry_and_rotation_capacity() {
    let declared = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 1, 1);

    assert_eq!(
        child_execution_budget(CookFollowUpBudgetScope::Cook, &declared),
        declared
    );
}

#[test]
fn persisted_review_budget_authority_preserves_lower_bounds_and_caps_larger_values() {
    let scope = CookFollowUpBudgetScope::FreshCookReview;
    let cook_budget = crate::agent_task_scheduler::AgentTaskExecutionBudget::new(9, 9, 9);
    let lower = serde_json::json!({
        "kind": "fresh_cook_review",
        "max_provider_executions": 1,
        "max_same_provider_retries": 0,
        "max_provider_rotations": 0,
        "review_plan_provider_executions": 0,
    });
    let (budget, _) = scoped_follow_up_budget(
        scope,
        &cook_budget,
        ExecutionBudgetUsage::default(),
        Some(&lower),
    );
    assert_eq!(
        budget,
        crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 0)
    );
    assert_eq!(review_budget_authority(scope, Some(&lower)), lower);

    let larger = serde_json::json!({
        "kind": "fresh_cook_review",
        "max_provider_executions": 99,
        "max_same_provider_retries": 99,
        "max_provider_rotations": 99,
        "review_plan_provider_executions": 99,
    });
    assert_eq!(
        review_budget_authority(scope, Some(&larger)),
        serde_json::json!({
            "kind": "fresh_cook_review",
            "max_provider_executions": 2,
            "max_same_provider_retries": 1,
            "max_provider_rotations": 0,
            "review_plan_provider_executions": 1,
        })
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
fn moving_base_recovery_isolates_identical_attempts_across_explicit_stores() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
    let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
    let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-moving-base-cook";
    let run_id = "same-moving-base-run";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = run_id.to_string();

    for (recipe_store, lifecycle_store, blocker) in [
        (&left_recipe_store, &left_lifecycle_store, "left blocker"),
        (&right_recipe_store, &right_lifecycle_store, "right blocker"),
    ] {
        recipe_store.persist_initial_recipe(&options).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
                Ok(serde_json::json!({}))
            })
            .unwrap();
        lifecycle_store
            .mutate_record(run_id, |record| {
                record.metadata["cook_id"] = serde_json::json!(cook_id);
                true
            })
            .unwrap();
        let mut recovery = moving_base_recovery_from_promotion(cook_id, run_id, promotion(run_id));
        recovery.blocker = blocker.to_string();
        lifecycle_store
            .record_cook_moving_base_recovery(run_id, serde_json::to_value(recovery).unwrap())
            .unwrap();
    }

    left_lifecycle_store
        .mutate_record(run_id, |record| {
            let identity = homeboy_lab_runner_contract::ExecutionPlacementIdentity {
                repository: "fixture".to_string(),
                workspace: "fixture".to_string(),
                task: "task".to_string(),
                candidate: None,
                base: None,
            };
            record.metadata["execution_placement_decision"] = serde_json::to_value(
                homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
                    "fixture",
                    "v1",
                    identity,
                    homeboy_lab_runner_contract::Placement::Local,
                ),
            )
            .unwrap();
            true
        })
        .unwrap();

    let left =
        moving_base_recovery_for_run_with_stores(&left_recipe_store, &left_lifecycle_store, run_id)
            .unwrap()
            .expect("left recovery");
    let right = moving_base_recovery_for_run_with_stores(
        &right_recipe_store,
        &right_lifecycle_store,
        run_id,
    )
    .unwrap()
    .expect("right recovery");

    assert_eq!(left.blocker, "left blocker");
    assert_eq!(right.blocker, "right blocker");
    assert_eq!(
        left.continuation,
        format!("homeboy --placement local agent-task cook-continue {run_id}")
    );
    assert_eq!(
        right.continuation,
        format!("homeboy agent-task cook-continue {run_id}")
    );
    assert_ne!(
        left_lifecycle_store.run_dir(run_id),
        right_lifecycle_store.run_dir(run_id)
    );
}

#[test]
fn candidate_adoption_lifecycle_and_promotion_evidence_do_not_alias_across_explicit_roots() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-adoption-cook";
    let run_id = "same-adoption-run";
    let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));

    for (store, candidate_sha, model, root) in [
        (&left_store, "left-candidate", "left-model", "left"),
        (&right_store, "right-candidate", "right-model", "right"),
    ] {
        store
            .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
                Ok(serde_json::json!({ "root": root }))
            })
            .unwrap();
        store.record_cook_attempt(cook_id, 1, run_id).unwrap();
        store
            .start_candidate_adoption_with_policy(
                run_id,
                candidate_sha,
                model,
                "verification",
                false,
                false,
            )
            .unwrap();
        store
            .checkpoint_candidate_adoption(run_id, "finalization", "finalize pull request")
            .unwrap();
        store
            .record_promotion(run_id, serde_json::json!({ "root": root }))
            .unwrap();
        store
            .record_candidate_adoption_result(run_id, serde_json::json!({ "root": root }))
            .unwrap();
        store.finish_candidate_adoption(run_id, None).unwrap();
    }

    for (store, candidate_sha, model, root) in [
        (&left_store, "left-candidate", "left-model", "left"),
        (&right_store, "right-candidate", "right-model", "right"),
    ] {
        let record = store.read_record(run_id).unwrap();
        let adoption = record.candidate_adoption.unwrap();
        assert_eq!(adoption.candidate_sha, candidate_sha);
        assert_eq!(adoption.ai_model, model);
        assert_eq!(adoption.result.unwrap()["root"], root);
        assert_eq!(record.metadata["latest_promotion"]["root"], root);
    }

    assert_ne!(left_store.run_dir(run_id), right_store.run_dir(run_id));
    assert_ne!(
        left_store.cook_index_path(cook_id),
        right_store.cook_index_path(cook_id)
    );
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

        let first = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                Err(Error::validation_invalid_argument(
                    "base",
                    "HEAD is behind or diverged from resolved base `main`",
                    None,
                    None,
                ))
            }))),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
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
        let second = run_cook(CookContext {
            side_effects: Some(Box::new(TestCookSideEffects::new(
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
            ))),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
        })
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
        let third = run_cook(CookContext {
            side_effects: Some(Box::new(TestCookSideEffects::new(
                |_: &_, _: &_, _: &_| panic!("failed rebased gates must not finalize"),
                |_: &AgentTaskCookServiceOptions, recovery: &MovingBaseCookRecovery| {
                    let mut failed = recovery.promotion.clone();
                    failed.status = AgentTaskPromotionStatus::GateFailed;
                    Ok(failed)
                },
            ))),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
        })
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
        let first = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                Err(Error::validation_invalid_argument(
                    "base",
                    "HEAD is behind or diverged from resolved base `main`",
                    None,
                    None,
                ))
            }))),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
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
        let second = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(
                move |_, _, _, recovered| {
                    finalization_calls_for_finalizer.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(recovered.status, AgentTaskPromotionStatus::Applied);
                    assert_eq!(recovered.verified_base.as_ref().unwrap().sha, expected_base);
                    Ok(serde_json::json!({"status": "review_ready"}))
                },
            ))),
            ..CookContext::new(options.clone(), Arc::new(UnusedExecutor))
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
        let error = recover_moving_base_cook_candidate_in_store(
            &agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store"),
            &options,
            &rebased_recovery,
        )
        .unwrap_err();
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
struct ProviderStartObservingDispatcher {
    run_id: String,
    phase_at_dispatch: Arc<Mutex<Option<Value>>>,
}

impl AgentTaskCookAttemptDispatcher for ProviderStartObservingDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-provider-start-observing" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        assert_eq!(run_id, self.run_id);
        let identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
            run_id,
            "fixture-lab",
            "provider-start-observing-job",
        );
        agent_task_lifecycle::bind_accepted_lab_runner_job(
            &identity,
            "/runner/workspace",
            &["homeboy".to_string(), "agent-task".to_string()],
        )?;
        let record = agent_task_lifecycle::status(run_id)?;
        *self.phase_at_dispatch.lock().expect("provider start phase") = Some(serde_json::json!({
            "phase": record.metadata["cook_progress"]["phase"],
            "state": record.state,
            "runner_job_id": record.runner_job_id(),
        }));
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

#[derive(Debug)]
struct WorkspaceCapturingDetachedAttemptDispatcher {
    plan: Arc<Mutex<Option<AgentTaskPlan>>>,
}

impl AgentTaskCookAttemptDispatcher for WorkspaceCapturingDetachedAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-workspace-capturing-detached" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        *self.plan.lock().expect("captured dispatch plan") = Some(plan.clone());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "workspace-capturing-daemon-job",
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
struct ImmediateSuccessExecutor;

impl AgentTaskExecutorAdapter for ImmediateSuccessExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
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
struct RecordingImmediateSuccessExecutor {
    starts: Arc<AtomicUsize>,
}

impl AgentTaskExecutorAdapter for RecordingImmediateSuccessExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        self.starts.fetch_add(1, Ordering::SeqCst);
        ImmediateSuccessExecutor.execute(request, context)
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
        assert!(
            request.policy.permits_workspace_read_tool(),
            "the form-only remediation must be able to inspect the authenticated candidate"
        );
        assert_eq!(request.policy.write, "none");
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
struct RecordingReviewFormExecutor {
    executions: Arc<AtomicUsize>,
}

impl AgentTaskExecutorAdapter for RecordingReviewFormExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        self.executions.fetch_add(1, Ordering::SeqCst);
        ReviewFormOnlyExecutor.execute(request, context)
    }
}

#[derive(Clone)]
struct TerminalSuccessExecutor;

impl AgentTaskExecutorAdapter for TerminalSuccessExecutor {
    fn execute(
        &self,
        request: crate::agent_task::AgentTaskRequest,
        _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> crate::agent_task::AgentTaskOutcome {
        crate::agent_task::AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("terminal retry fixture succeeded".to_string()),
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
                Arc::new(ProviderMissingExecutor),
                derived_cook_baseline,
                None,
            )?
        } else {
            run_loaded_plan_with_derived_cook_baseline(
                plan,
                Some(run_id),
                Arc::new(ReviewFormOnlyExecutor),
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
        assert_eq!(result.exit_code, 0, "{:#?}", result.value);
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

/// Records which children were actually dispatched.
///
/// The property under test — that a coordinator does not run a child twice —
/// is about work *started*, not about what a report says afterwards. A cell can
/// be shaped correctly for the wrong reason, so this observes the dispatch
/// boundary directly.
#[derive(Debug)]
struct DispatchCountingAttemptDispatcher {
    dispatched: Arc<Mutex<Vec<String>>>,
}

impl AgentTaskCookAttemptDispatcher for DispatchCountingAttemptDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-dispatch-counter" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.dispatched
            .lock()
            .expect("dispatched children")
            .push(run_id.to_string());
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
    runtime_recovery: Option<agent_task_lifecycle::AgentTaskLabRuntimeRecovery>,
    phase: &'static str,
}

#[derive(Debug)]
struct RetryableTransportFailingAttemptDispatcher {
    dispatches: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct AttestingRetryableTransportDispatcher {
    observations: Arc<Mutex<Vec<(String, Value, Value, bool)>>>,
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

    fn pre_execution_failure_phase(&self) -> &'static str {
        self.phase
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        let mut error = Error::validation_invalid_argument(
            "controller_admission",
            self.message,
            Some("fixture controller diagnostics".to_string()),
            None,
        );
        if let Some(recovery) = &self.runtime_recovery {
            error.details["lab_handoff_runtime_recovery"] =
                serde_json::to_value(recovery).expect("runtime recovery serializes");
        }
        Err(error)
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

impl AgentTaskCookAttemptDispatcher for AttestingRetryableTransportDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(serde_json::json!({ "kind": "test-attesting-transport-failure" }))
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        let task = &plan.tasks[0];
        let root = task
            .workspace
            .root
            .as_deref()
            .expect("baseline workspace root");
        let identity = task.metadata["cook_workspace_identity"].clone();
        let predecessor = task.metadata["cook_workspace_identity_predecessor"].clone();
        let matches = crate::agent_task_workspace_identity::workspace_matches_attestation(
            std::path::Path::new(root),
            &identity,
        );
        self.observations
            .lock()
            .expect("baseline observations")
            .push((root.to_string(), identity, predecessor, matches));
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
                runtime_tools: Vec::new(),
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
        draft_pr: false,
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
fn workspace_base_ancestry_preflight_rejects_behind_and_diverged_without_attributing_base_files() {
    let remote = tempfile::tempdir().expect("bare origin");
    let workspace = tempfile::tempdir().expect("workspace");
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
    git(remote.path(), &["init", "--bare"]);
    let output = Command::new("git")
        .args([
            "clone",
            remote.path().to_str().unwrap(),
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .expect("clone origin");
    assert!(
        output.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(
        workspace.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(workspace.path(), &["config", "user.name", "Test"]);
    git(workspace.path(), &["checkout", "-b", "main"]);
    std::fs::write(workspace.path().join("base.txt"), "base\n").unwrap();
    git(workspace.path(), &["add", "base.txt"]);
    git(workspace.path(), &["commit", "-m", "base"]);
    git(workspace.path(), &["push", "-u", "origin", "main"]);
    git(workspace.path(), &["checkout", "-b", "candidate"]);
    git(workspace.path(), &["checkout", "main"]);
    std::fs::write(workspace.path().join("newer-base.txt"), "base only\n").unwrap();
    git(workspace.path(), &["add", "newer-base.txt"]);
    git(workspace.path(), &["commit", "-m", "advance base"]);
    git(workspace.path(), &["push"]);
    git(workspace.path(), &["checkout", "candidate"]);

    let behind = preflight_cook_workspace_base_ancestry(workspace.path(), "main")
        .expect_err("strictly behind destination is rejected before provider execution");
    assert_eq!(
        behind.details["workspace_base_ancestry"]["direction"],
        "behind"
    );
    assert_eq!(
        behind.details["workspace_base_ancestry"]["base_only_commits"],
        1
    );
    assert_eq!(
        behind.details["workspace_base_ancestry"]["candidate_only_commits"],
        0
    );
    assert_eq!(
        behind.details["workspace_base_ancestry"]["next_action"],
        "converge_destination_before_provider"
    );

    git(workspace.path(), &["merge", "--ff-only", "origin/main"]);
    preflight_cook_workspace_base_ancestry(workspace.path(), "main")
        .expect("clean intentional no-change destination is equivalent to its resolved base");
    std::fs::write(workspace.path().join("candidate.txt"), "candidate only\n").unwrap();
    git(workspace.path(), &["add", "candidate.txt"]);
    git(workspace.path(), &["commit", "-m", "candidate"]);
    preflight_cook_workspace_base_ancestry(workspace.path(), "main")
        .expect("ahead destination retains a candidate relative to the resolved base");

    git(workspace.path(), &["checkout", "main"]);
    std::fs::write(workspace.path().join("newer-base-2.txt"), "base only\n").unwrap();
    git(workspace.path(), &["add", "newer-base-2.txt"]);
    git(workspace.path(), &["commit", "-m", "advance base again"]);
    git(workspace.path(), &["push"]);
    git(workspace.path(), &["checkout", "candidate"]);
    let diverged = preflight_cook_workspace_base_ancestry(workspace.path(), "main")
        .expect_err("diverged destination is rejected before provider execution");
    assert_eq!(
        diverged.details["workspace_base_ancestry"]["direction"],
        "diverged"
    );
    assert_eq!(
        diverged.details["workspace_base_ancestry"]["base_only_commits"],
        1
    );
    assert_eq!(
        diverged.details["workspace_base_ancestry"]["candidate_only_commits"],
        1
    );
}

#[cfg(unix)]
#[test]
fn initial_cook_adopts_only_clean_issue_owned_unpushed_provider_worktree() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|home| {
        let primary = tempfile::tempdir().expect("primary repository");
        let target = home.path().join("issue-worktree");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(primary.path(), &["init", "--initial-branch=main"]);
        git(
            primary.path(),
            &["config", "user.email", "agent@example.test"],
        );
        git(primary.path(), &["config", "user.name", "Agent"]);
        std::fs::write(primary.path().join("tracked"), "base\n").unwrap();
        git(primary.path(), &["add", "tracked"]);
        git(primary.path(), &["commit", "-m", "base"]);
        let base = git(primary.path(), &["rev-parse", "HEAD"]);
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "issue-11091",
                target.to_str().unwrap(),
            ],
        );
        std::fs::write(target.join("candidate"), "committed\n").unwrap();
        git(&target, &["add", "candidate"]);
        git(&target, &["commit", "-m", "candidate"]);

        let provider = home.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                serde_json::json!({"worktrees": [{
                    "handle": "fixture@issue-11091", "path": target, "branch": "issue-11091",
                    "task_url": "https://example.test/issues/11091",
                    "safety": {"dirty": false, "unpushed": true, "primary": false}
                }]})
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).unwrap();
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
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
                        task_url: Some("$.task_url".to_string()),
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).unwrap();

        let mut options =
            batch_cook_options("issue-11091", Arc::new(AcceptedDetachedAttemptDispatcher));
        options.to_worktree = "fixture@issue-11091".to_string();
        options.source_worktree_path = Some(target.clone());
        options.task_base_sha = Some(base);
        options.initial_plan.tasks[0].workspace.task_url =
            Some("https://example.test/issues/11091".to_string());
        validate_cook_workspace(&options)
            .expect("clean issue-owned committed checkout is adoptable");

        std::fs::write(target.join("dirty"), "drift\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("dirty checkout remains blocked");
        assert_eq!(
            error.details["workspace"]["classification"],
            "workspace.resolved_but_dirty"
        );
        std::fs::remove_file(target.join("dirty")).unwrap();

        options.initial_plan.tasks[0].workspace.task_url =
            Some("https://example.test/issues/other".to_string());
        let error =
            validate_cook_workspace(&options).expect_err("wrong task ownership remains blocked");
        assert!(error.message.contains("not owned by this Cook task"));

        options.initial_plan.tasks[0].workspace.task_url =
            Some("https://example.test/issues/11091".to_string());
        options.task_base_sha = Some("0000000000000000000000000000000000000000".to_string());
        let error = validate_cook_workspace(&options).expect_err("unsafe ancestry remains blocked");
        assert!(error.message.contains("cannot be trusted"));
    });
}

#[cfg(unix)]
#[test]
fn explicit_cook_workspace_bypasses_a_timed_out_provider_lookup() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary repository");
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(primary.path(), &["init", "--initial-branch=main"]);
        git(
            primary.path(),
            &["config", "user.email", "agent@example.test"],
        );
        git(primary.path(), &["config", "user.name", "Agent"]);
        std::fs::write(primary.path().join("tracked.txt"), "base\n").expect("write base");
        git(primary.path(), &["add", "tracked.txt"]);
        git(primary.path(), &["commit", "-m", "base"]);
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "fix/cwd-authority",
                target.to_str().expect("target path"),
                "HEAD",
            ],
        );

        let provider = tempfile::NamedTempFile::new().expect("provider file");
        std::fs::write(provider.path(), "#!/bin/sh\nsleep 2\n").expect("write provider");
        let mut permissions = std::fs::metadata(provider.path())
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(provider.path(), permissions).expect("make provider executable");
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "timeout".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 1,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.path().display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");

        let mut options = batch_cook_options(
            "cwd-authoritative-workspace",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.to_worktree = "fixture@cwd-authority".to_string();
        options.source_worktree_path = Some(target);
        options.task_base_sha = Some("base".to_string());
        options.initial_plan.tasks[0].workspace.task_url =
            Some("https://example.test/issues/cwd-authority".to_string());
        options.initial_plan.tasks[0].metadata["worktree_provision"] =
            serde_json::json!({ "kind": "explicit_cwd" });

        validate_cook_workspace(&options)
            .expect("explicit workspace must not wait for provider resolution");
    });
}

#[test]
fn explicit_cook_workspace_cleanliness_is_an_initial_admission_check() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary repository");
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(primary.path(), &["init", "--initial-branch=main"]);
        git(
            primary.path(),
            &["config", "user.email", "agent@example.test"],
        );
        git(primary.path(), &["config", "user.name", "Agent"]);
        std::fs::write(primary.path().join("tracked.txt"), "base\n").expect("write base");
        git(primary.path(), &["add", "tracked.txt"]);
        git(primary.path(), &["commit", "-m", "base"]);
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "fix/cwd-cleanliness",
                target.to_str().expect("target path"),
                "HEAD",
            ],
        );

        let mut options = batch_cook_options(
            "cwd-cleanliness-boundary",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.to_worktree = target.display().to_string();
        options.source_worktree_path = Some(target.clone());
        options.initial_plan.tasks[0].metadata["worktree_provision"] =
            serde_json::json!({ "kind": "explicit_cwd" });
        validate_cook_workspace(&options).expect("clean explicit CWD has a valid identity");
        admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
            .expect("clean explicit CWD is admitted before its first provider attempt");

        std::fs::write(target.join("candidate.txt"), "provider change\n")
            .expect("write candidate change");
        validate_cook_workspace(&options)
            .expect("retry identity validation retains provider candidate changes");
        let error =
            admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
                .expect_err("a dirty initial CWD cannot enter its first provider attempt");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error.message.contains("must be clean"));
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("persist a pre-provider lifecycle failure");
        agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, &options.initial_run_id)
            .expect("index zero-execution attempt");
        admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
            .expect_err("a retry after a zero-execution lifecycle failure must reject user drift");

        std::fs::remove_file(target.join("candidate.txt")).expect("remove pre-provider drift");
        let evidence = target.join(".homeboy/evidence/input/context.txt");
        std::fs::create_dir_all(evidence.parent().expect("evidence parent"))
            .expect("create projected evidence directory");
        std::fs::write(&evidence, "controller evidence\n").expect("write projected evidence");
        options.initial_plan.tasks[0].executor.config = serde_json::json!({
            "evidence_inputs": [{ "path": evidence }]
        });
        admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
            .expect("the durable projected evidence path is not user drift");

        agent_task_lifecycle::record_metadata_value(
            &options.initial_run_id,
            "provider_executions_consumed",
            serde_json::json!(1),
        )
        .expect("record provider execution boundary");
        std::fs::write(target.join("candidate.txt"), "provider change\n")
            .expect("write provider candidate");
        admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
            .expect("candidate changes remain admissible after a durable provider execution");

        agent_task_lifecycle::rewrite_record_for_test(&options.initial_run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::Value::Null;
            record.metadata["provider_executions"] = serde_json::json!([{
                "key": "task:1", "state": "succeeded"
            }]);
        })
        .expect("persist a historical provider execution ledger without its counter");
        admit_explicit_cook_workspace_before_provider(&options, &options.initial_run_id)
            .expect("historical provider ledger keeps candidate changes admissible");
    });
}

#[test]
fn dirty_explicit_cwd_blocks_detached_provider_dispatch() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary repository");
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(primary.path(), &["init", "--initial-branch=main"]);
        git(
            primary.path(),
            &["config", "user.email", "agent@example.test"],
        );
        git(primary.path(), &["config", "user.name", "Agent"]);
        std::fs::write(primary.path().join("tracked.txt"), "base\n").expect("write base");
        git(primary.path(), &["add", "tracked.txt"]);
        git(primary.path(), &["commit", "-m", "base"]);
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "fix/cwd-dispatch",
                target.to_str().expect("target path"),
                "HEAD",
            ],
        );

        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            "cwd-detached-dirty",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: dispatches.clone(),
            }),
        );
        options.to_worktree = target.display().to_string();
        options.source_worktree_path = Some(target.clone());
        options.initial_plan.tasks[0].workspace.root = Some(target.display().to_string());
        options.initial_plan.tasks[0].metadata["worktree_provision"] =
            serde_json::json!({ "kind": "explicit_cwd" });
        std::fs::write(target.join("untracked.txt"), "user drift\n").expect("write drift");

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("Cook reports admission failure");
        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn reconstructed_cook_rejects_a_removed_managed_workspace_before_provider_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary repository");
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(primary.path(), &["init", "--initial-branch=main"]);
        git(
            primary.path(),
            &["config", "user.email", "agent@example.test"],
        );
        git(primary.path(), &["config", "user.name", "Agent"]);
        std::fs::write(primary.path().join("tracked.txt"), "base\n").expect("write base");
        git(primary.path(), &["add", "tracked.txt"]);
        git(primary.path(), &["commit", "-m", "base"]);
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "fix/continuation",
                target.to_str().expect("target path"),
                "HEAD",
            ],
        );

        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            "removed-managed-continuation",
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: dispatches.clone(),
            }),
        );
        options.to_worktree = "fixture@removed-continuation".to_string();
        options.source_worktree_path = Some(target.clone());
        persist_initial_recipe(&options).expect("persist Cook recipe");
        let recipe = super::super::load_recipe(&options.cook_id).expect("load Cook recipe");
        let reconstructed = super::super::reconstruct_options_with_dispatcher(
            &recipe,
            Some(Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: dispatches.clone(),
            })),
        )
        .expect("reconstruct persisted Cook options");

        let data_root = homeboy_core::paths::observation_db()
            .expect("observation database")
            .parent()
            .expect("observation data root")
            .to_path_buf();
        let records = data_root.join("task-worktrees");
        std::fs::create_dir_all(&records).expect("create task-worktree registry");
        let record = serde_json::json!({
            "id": options.to_worktree,
            "component_id": "fixture",
            "source_checkout": primary.path(),
            "worktree_path": target,
            "branch": "fix/continuation",
            "base_ref": "main",
            "cleanup_policy": "remove_when_safe",
            "branch_cleanup_intent": "delete_when_merged",
            "created_at": "2026-01-01T00:00:00Z",
            "state": "removed",
            "lifecycle_revision": 1,
        });
        std::fs::write(
            records.join(format!(
                "{}.json",
                homeboy_core::paths::sanitize_path_segment(&options.to_worktree)
            )),
            serde_json::to_vec(&record).expect("serialize removed worktree record"),
        )
        .expect("write removed worktree record");

        let error = validate_cook_workspace(&reconstructed)
            .expect_err("removed managed worktree must reject reconstructed Cook");
        assert_eq!(
            error.code,
            homeboy_core::error::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error.message.contains("no longer active"));

        let result = run_cook(CookContext::new(reconstructed, Arc::new(UnusedExecutor)))
            .expect("durable Cook failure report before provider execution");
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "durable_failure");
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn concurrent_first_cooks_elect_one_recipe_creator_without_replacing_its_plan() {
    let ambient_lifecycle_store =
        AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
    let run_id = homeboy_core::test_support::with_isolated_home(|_| {
        run_concurrent_first_cooks_recipe_creator_fixture()
    });
    assert!(!ambient_lifecycle_store
        .record_exists(&run_id)
        .expect("ambient lifecycle state remains untouched"));
}

fn run_concurrent_first_cooks_recipe_creator_fixture() -> String {
    let roots = homeboy_core::paths::PathRoots::from_environment().expect("isolated roots");
    let store = CookRecipeStore::new(roots.clone());
    let lifecycle_store = AgentTaskLifecycleStore::new(roots);
    let cook_id = format!("concurrent-first-cook-{}", uuid::Uuid::new_v4());
    let mut winner = batch_cook_options(&cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    winner.initial_plan.plan_id = "creator-plan".to_string();
    let mut loser = winner.clone();
    loser.initial_plan.plan_id = "loser-plan".to_string();

    let barrier = Arc::new(Barrier::new(2));
    set_initial_recipe_creation_barrier_for_test(Some(Arc::clone(&barrier)));
    let (winner_result, loser_result) = std::thread::scope(|scope| {
        let winner = scope.spawn(|| {
            run_cook(CookContext {
                store: Some(&store),
                lifecycle_store: Some(&lifecycle_store),
                side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                    Ok(serde_json::json!({}))
                }))),
                ..CookContext::new(winner, Arc::new(UnusedExecutor))
            })
        });
        let loser = scope.spawn(|| {
            run_cook(CookContext {
                store: Some(&store),
                lifecycle_store: Some(&lifecycle_store),
                side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                    Ok(serde_json::json!({}))
                }))),
                ..CookContext::new(loser, Arc::new(UnusedExecutor))
            })
        });
        (winner.join().unwrap(), loser.join().unwrap())
    });
    set_initial_recipe_creation_barrier_for_test(None);

    let outcomes = [winner_result.unwrap(), loser_result.unwrap()];
    let statuses = outcomes
        .iter()
        .map(|outcome| outcome.value.status.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.value.status == "in_flight")
            .count(),
        1,
        "unexpected concurrent Cook statuses: {statuses:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.value.status == "durable_failure")
            .count(),
        1,
        "unexpected concurrent Cook statuses: {statuses:?}"
    );
    let recipe = store.load_recipe(&cook_id).expect("creator recipe");
    let creator_options = super::super::reconstruct_options_with_dispatcher(
        &recipe,
        Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
    )
    .expect("creator options");
    let record_before = lifecycle_store
        .read_record(&creator_options.initial_run_id)
        .expect("running creator record");
    let plan_before = lifecycle_store
        .read_controller_plan(&creator_options.initial_run_id)
        .expect("creator controller plan");
    let aggregate_before = agent_task_lifecycle::read_aggregate_in_store(
        &lifecycle_store,
        &creator_options.initial_run_id,
    )
    .ok();

    assert_eq!(record_before.state, AgentTaskRunState::Running);
    assert_eq!(
        lifecycle_store
            .read_record(&creator_options.initial_run_id)
            .expect("creator record remains running")
            .state,
        record_before.state
    );
    assert_eq!(
        lifecycle_store
            .read_controller_plan(&creator_options.initial_run_id)
            .expect("creator plan remains immutable"),
        plan_before
    );
    assert_eq!(
        agent_task_lifecycle::read_aggregate_in_store(
            &lifecycle_store,
            &creator_options.initial_run_id,
        )
        .ok(),
        aggregate_before
    );
    creator_options.initial_run_id
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
    assert_eq!(
        request.metadata["publication"],
        serde_json::json!({
            "capability": "unavailable",
            "owner": "controller",
            "status": "not_attempted"
        })
    );
    assert!(request
        .instructions
        .contains("non-publishable attempt workspace"));
    assert!(request
        .instructions
        .contains("do not push, create a pull request"));
    assert!(request
        .instructions
        .contains("controller-owned publication"));

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
fn provider_prompt_distinguishes_controller_owned_gates_from_focused_checks() {
    let mut options = batch_cook_options(
        "controller-owned-gate-contract",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    );
    options.gates.verify = vec!["cargo test --locked -p homeboy-agents".to_string()];
    options.gates.private_verify = vec!["private-gate --token secret".to_string()];

    project_controller_owned_gate_contract(&mut options);

    let instructions = &options.initial_plan.tasks[0].instructions;
    assert!(instructions.contains("Declared deterministic gates are controller-owned."));
    assert!(instructions.contains("`cargo test --locked -p homeboy-agents`"));
    assert!(instructions.contains("1 private deterministic gate(s)"));
    assert!(!instructions.contains("private-gate --token secret"));
    assert!(instructions.contains("focused check only when it directly reduces uncertainty"));
    assert!(instructions.contains("authoritative final gate evidence separately"));

    project_controller_owned_gate_contract(&mut options);
    assert_eq!(
        options.initial_plan.tasks[0]
            .instructions
            .matches("Declared deterministic gates are controller-owned.")
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
    source: std::path::PathBuf,
    provider: std::path::PathBuf,
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
        // The fixture declares `base: "main"`, and promotion verifies that base
        // exists on origin. Without an explicit initial branch this repo inherits
        // the host's `init.defaultBranch` — still `master` on stock git — so the
        // declared base never resolves and every adoption test fails on a machine
        // that has not opted into `main`.
        git(&source, &["init", "--initial-branch=main"]);
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
            source,
            provider,
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

    fn adopt(
        &self,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        executor: SharedAgentTaskExecutor,
        backend: &mut CaptureBackend,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        self.adopt_run_with_inherited_failure_acceptance(
            &self.run_id,
            false,
            dispatcher,
            executor,
            backend,
        )
    }

    fn adopt_run(
        &self,
        run_id: &str,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        executor: SharedAgentTaskExecutor,
        backend: &mut CaptureBackend,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        self.adopt_run_with_inherited_failure_acceptance(
            run_id, false, dispatcher, executor, backend,
        )
    }

    fn adopt_run_with_inherited_failure_acceptance(
        &self,
        run_id: &str,
        accept_inherited_failures: bool,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        executor: SharedAgentTaskExecutor,
        backend: &mut CaptureBackend,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        adopt_cook_candidate_with_dispatcher_and_backend(
            run_id,
            &self.candidate,
            AgentTaskCandidateAdoptionOptions {
                ai_model: Some("openai/gpt-5.6-terra".to_string()),
                replace_interrupted: false,
                accept_inherited_failures,
            },
            dispatcher,
            executor,
            backend,
        )
    }
}

#[test]
fn adoption_accepts_an_identical_immutable_baseline_failure_when_explicitly_authorized() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut fixture = CandidateAdoptionFixture::new("cook-11978-inherited", 1, 0, true, None);
        fixture.options.gates.verify = vec![
            "printf inherited >&2; exit 1".to_string(),
            "test \"$(cat lib.rs)\" = candidate".to_string(),
        ];
        fixture.options.gates.execution_policy =
            crate::agent_task_gate::AgentTaskGateExecutionPolicy::ContinueAll;
        persist_initial_recipe(&fixture.options).expect("persist inherited-failure recipe");
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt_run_with_inherited_failure_acceptance(
                &fixture.run_id,
                true,
                |_| Ok(None),
                Arc::new(UnusedExecutor),
                &mut backend,
            )
            .expect("identical inherited failure is accepted");

        assert_eq!(result.exit_code, 0, "{:#?}", result.value);
        assert_eq!(result.value.status, "green_no_finalize");
        let promotion = result.value.attempts[0].promotion.as_ref().unwrap();
        assert_eq!(
            promotion.deterministic_gates[0].status,
            crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
        );
        assert_eq!(
            promotion.deterministic_gates[0]
                .baseline_comparison
                .as_ref()
                .unwrap()
                .result,
            crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed
        );
        assert_eq!(
            promotion.deterministic_gates[1].status,
            crate::agent_task_gate::AgentTaskGateStatus::Succeeded
        );
        assert_eq!(
            promotion.provenance["candidate"]["fingerprint"]["head"],
            fixture.candidate
        );
        assert!(
            promotion.deterministic_gates[0]
                .candidate_checkout
                .is_some(),
            "normalized inherited-red evidence remains bound to its immutable candidate checkout"
        );

        let reused = fixture
            .adopt(|_| Ok(None), Arc::new(UnusedExecutor), &mut backend)
            .expect("completed adoption returns its durable report");
        assert_eq!(reused.exit_code, 1, "{:#?}", reused.value);
        assert_eq!(reused.value.status, "baseline_red");
        assert!(reused
            .value
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("--accept-inherited-failures")));
    });
}

#[test]
fn adoption_blocks_a_changed_failure_despite_inherited_failure_authorization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut fixture = CandidateAdoptionFixture::new("cook-11978-regression", 1, 0, true, None);
        fixture.options.gates.verify = vec!["cat lib.rs >&2; exit 1".to_string()];
        persist_initial_recipe(&fixture.options).expect("persist regression recipe");
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt_run_with_inherited_failure_acceptance(
                &fixture.run_id,
                true,
                |_| Ok(None),
                Arc::new(UnusedExecutor),
                &mut backend,
            )
            .expect("regression produces a blocking report");

        assert_eq!(result.exit_code, 1, "{:#?}", result.value);
        assert_eq!(result.value.status, "gate_failed");
        let promotion = result.value.attempts[0].promotion.as_ref().unwrap();
        assert_eq!(
            promotion.deterministic_gates[0].status,
            crate::agent_task_gate::AgentTaskGateStatus::Failed
        );
        assert_eq!(
            promotion.deterministic_gates[0]
                .baseline_comparison
                .as_ref()
                .unwrap()
                .result,
            crate::agent_task_gate::AgentTaskGateDifferentialResult::CandidateRegression
        );
    });
}

#[test]
fn adoption_blocks_inherited_failure_when_candidate_package_artifact_differs_from_base() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut fixture = CandidateAdoptionFixture::new("cook-11978-artifact", 1, 0, true, None);
        let artifact = fixture.source.join("fixtures/input.bin");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"candidate package input").unwrap();
        git_output(&fixture.source, &["add", "fixtures/input.bin"]).unwrap();
        git_output(
            &fixture.source,
            &["commit", "-m", "candidate package input"],
        )
        .unwrap();
        fixture.candidate = git_output(&fixture.source, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(
            &fixture.provider,
            format!(
                "#!/bin/sh\ncat >/dev/null\ngit -C {target} fetch origin {candidate}\ngit -C {target} reset --hard --quiet FETCH_HEAD\nprintf '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{target}\",\"command_evidence\":[]}}'\n",
                target = fixture.target.display(),
                candidate = fixture.candidate,
            ),
        )
        .unwrap();
        fixture.options.gates.verify = vec!["printf inherited >&2; exit 1".to_string()];
        fixture.options.gates.gate_package_artifacts = vec![
            crate::agent_task_gate::AgentTaskGatePackageArtifactRequirement {
                id: "candidate-input".to_string(),
                environment: crate::agent_task_gate::AgentTaskGateArtifactEnvironmentMapping {
                    name: "CANDIDATE_INPUT".to_string(),
                    source: None,
                    default: Some("fixtures".to_string()),
                },
                required_paths: vec![
                    crate::agent_task_gate::AgentTaskGateArtifactPathRequirement {
                        path: "fixtures/input.bin".to_string(),
                        sha256: None,
                    },
                ],
                remediation: serde_json::json!({"action": "restore_candidate_input"}),
            },
        ];
        persist_initial_recipe(&fixture.options).expect("persist package-artifact recipe");
        let mut backend = CaptureBackend::default();

        let result = fixture
            .adopt_run_with_inherited_failure_acceptance(
                &fixture.run_id,
                true,
                |_| Ok(None),
                Arc::new(UnusedExecutor),
                &mut backend,
            )
            .expect("package drift produces a blocking report");

        assert_eq!(result.exit_code, 1, "{:#?}", result.value);
        assert_eq!(result.value.status, "gate_failed");
        let promotion = result.value.attempts[0].promotion.as_ref().unwrap();
        assert_eq!(
            promotion.deterministic_gates[0]
                .baseline_comparison
                .as_ref()
                .unwrap()
                .result,
            crate::agent_task_gate::AgentTaskGateDifferentialResult::Inconclusive
        );
        assert!(promotion.deterministic_gates[0]
            .environment
            .package_artifacts[0]
            .artifacts[0]
            .sha256
            .is_some());
    });
}

#[test]
fn continuation_finalizes_applied_green_candidate_despite_recoverable_artifact_diagnostic() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus};
        use crate::agent_task_scheduler::{
            AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
        };

        let run_id = "cook-applied-recoverable-artifact";
        let mut options = batch_cook_options(
            "cook-applied-recoverable-artifact",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_run_id = run_id.to_string();
        options.no_finalize = false;
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("submit terminal attempt");
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &options.initial_plan,
            &AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: options.initial_plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::PartialRecoverable,
                totals: AgentTaskAggregateTotals {
                    succeeded: 1,
                    ..Default::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("provider completed".to_string()),
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        id: "unprojected-provider-artifact".to_string(),
                        kind: "patch".to_string(),
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
        .expect("persist recoverable aggregate");
        agent_task_lifecycle::record_promotion(
            run_id,
            serde_json::to_value(promotion(run_id)).expect("serialize applied promotion"),
        )
        .expect("persist green promotion");

        assert!(
            agent_task_lifecycle::terminal_artifact_projection_readiness(run_id)
                .expect("read artifact projection")
                .is_some()
        );
        let recipe = super::super::load_recipe(&options.cook_id).expect("load recipe");
        super::super::reconcile_recipe_attempt_for_continuation(&recipe, run_id)
            .expect("green applied candidate does not need provider artifact replay");

        let finalizations = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&finalizations);
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(
                move |_, _, finalized_run, _| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(finalized_run, run_id);
                    Ok(serde_json::json!({"status": "review_ready"}))
                },
            ))),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
        })
        .expect("continue through finalization");

        assert_eq!(result.value.status, "review_ready");
        assert_eq!(finalizations.load(Ordering::SeqCst), 1);
    });
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
                runtime_recovery: None,
                phase: "controller_admission",
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        let result = run_cook(CookContext::new(
            AgentTaskCookServiceOptions {
                initial_run_id: run_id.to_string(),
                ..options
            },
            Arc::new(UnusedExecutor),
        ))
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

#[cfg(unix)]
#[test]
fn persistent_slow_provider_with_known_path_returns_exhausted_cwd_recovery() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("workspace root");
        let source = root.path().join("source");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&source).expect("source directory");
        assert!(Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(&source)
            .status()
            .expect("git init")
            .success());
        for args in [
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("git config")
                .success());
        }
        std::fs::write(source.join("tracked.txt"), "base\n").expect("write base");
        std::fs::write(
            source.join("package.json"),
            r#"{"scripts":{"test":"true"}}"#,
        )
        .expect("write package manifest");
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&source)
            .status()
            .expect("git add")
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(&source)
            .status()
            .expect("git commit")
            .success());
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "cook-slow-worktree-lookup"
            ])
            .arg(&workspace)
            .current_dir(&source)
            .status()
            .expect("git worktree add")
            .success());
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nsleep 1\nprintf '%s\\n' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"unreachable\",\"handle\":\"fixture@cook-slow-worktree-lookup\",\"path\":\"{}\",\"branch\":\"cook-slow-worktree-lookup\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'\n",
                workspace.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions.clone()).expect("make provider executable");

        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 250,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve_identity: Some(vec![
                        provider.display().to_string(),
                        "identity".to_string(),
                        "{handle}".to_string(),
                    ]),
                    attest_safety: Some(vec![
                        provider.display().to_string(),
                        "safety".to_string(),
                        "{identity}".to_string(),
                    ]),
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");

        let cook_id = "cook-slow-worktree-lookup";
        let run_id = "cook-slow-worktree-lookup-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        // The provider identity is deliberately absent until durable admission.
        // The detached dispatcher keeps the resumed Cook test focused on its
        // durable retry and workspace lifecycle rather than local execution.
        options.initial_run_id = run_id.to_string();
        options.max_attempts = 2;
        options.gates.verify = vec!["npm test".to_string()];
        options.initial_plan.tasks[0].metadata["worktree_provision"] = serde_json::json!({
            "action": "lookup_pending",
            "kind": "provider",
            "handle": options.to_worktree,
        });
        let exact_handle = options.to_worktree.clone();

        options.source_worktree_path = Some(workspace.clone());
        let result = run_cook(CookContext::new(options.clone(), Arc::new(UnusedExecutor)))
            .expect("Cook records exhausted provider timeout");

        assert_eq!(result.exit_code, 1, "{:?}", result.value);
        assert_eq!(result.value.cook_id, cook_id);
        assert_eq!(result.value.latest_run_id.as_deref(), Some(run_id));
        let record = agent_task_lifecycle::status(run_id).expect("durable lookup record");
        let persisted_plan = agent_task_lifecycle::load_plan(run_id).expect("durable lookup plan");
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            persisted_plan.metadata["worktree_provider_resolve"]["phase"],
            "worktree_provider_lookup"
        );
        assert_eq!(
            persisted_plan.metadata["worktree_provider_resolve"]["attempt"],
            2
        );
        assert_eq!(
            persisted_plan.metadata["worktree_provider_resolve"]["retry_disposition"],
            "exhausted"
        );
        assert!(
            persisted_plan.metadata["worktree_provider_resolve"]["deadline_unix_ms"]
                .as_u64()
                .expect("deadline")
                > 0
        );
        assert!(
            persisted_plan.metadata["worktree_provider_resolve"]["events"]
                .as_array()
                .expect("durable resolve events")
                .iter()
                .any(|event| event["attempt"] == 1 && event["next_retry_unix_ms"].is_number())
        );
        assert!(
            persisted_plan.metadata["worktree_provider_resolve"]["cwd_recovery_command"]
                .as_str()
                .expect("cwd recovery command")
                .contains(&format!("--cwd {}", workspace.display()))
        );
        let recipe = super::super::load_recipe(cook_id).expect("durable Cook identity");
        assert_eq!(recipe.attempts[0].run_id, run_id);
        assert_eq!(
            persisted_plan.tasks[0].metadata["worktree_provision"]["handle"],
            exact_handle
        );
        assert_eq!(persisted_plan.tasks[0].workspace.root, None);
        assert!(persisted_plan.metadata.get("cook_provision").is_none());
        assert!(persisted_plan.tasks[0]
            .metadata
            .get("cook_workspace_identity")
            .is_none());
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nif test \"$1\" = identity; then printf '%s\\n' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"recovered-identity\",\"handle\":\"fixture@cook-slow-worktree-lookup\",\"path\":\"{}\",\"branch\":\"cook-slow-worktree-lookup\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'; else printf '%s\\n' '{{\"schema\":\"homeboy/worktree-provider-safety/v1\",\"identity_token\":\"recovered-identity\",\"observed_at\":\"2026-01-01T00:00:00Z\",\"dirty\":false,\"unpushed\":false,\"fresh\":true,\"latency_ms\":0,\"budget_ms\":0}}'; fi\n",
                workspace.display()
            ),
        )
        .expect("recover provider");
        std::fs::set_permissions(&provider, permissions).expect("restore executable provider");

        let retry = crate::agent_task_service::retry(run_id, None, false, false)
            .expect("reserve a Cook-owned retry successor");
        assert_eq!(retry.record.metadata["retry_of"], run_id);
        assert_eq!(retry.record.metadata["cook_id"], cook_id);
        assert_eq!(retry.record.metadata["cook_attempt"], 2);
        let recipe = super::super::load_recipe(cook_id).expect("same Cook recipe owns retry");
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[1].run_id, retry.record.run_id);

        let mut resumed_options = super::super::reconstruct_options_with_dispatcher(
            &recipe,
            Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
        )
        .expect("reconstruct Cook-owned retry");
        resumed_options.initial_run_id = retry.record.run_id.clone();
        resumed_options.initial_plan =
            agent_task_lifecycle::load_plan(&retry.record.run_id).expect("load durable retry plan");
        let resumed = run_cook(CookContext::new(resumed_options, Arc::new(UnusedExecutor)))
            .expect("same Cook retry materializes its recovered provider workspace");
        assert_eq!(resumed.value.cook_id, cook_id);
        let resumed_plan =
            agent_task_lifecycle::load_plan(&retry.record.run_id).expect("materialized retry plan");
        assert_eq!(
            resumed_plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.to_str().expect("utf8 workspace"))
        );
        let workspace_identity = resumed_plan.tasks[0].metadata["cook_workspace_identity"].clone();
        assert_eq!(
            resumed_plan.metadata["cook_provision"]["workspace_identity"]["token"],
            "recovered-identity"
        );
        assert_eq!(
            resumed_plan.metadata["cook_provision"]["action"],
            "existing"
        );
        let resumed_record = agent_task_lifecycle::status(&retry.record.run_id)
            .expect("resumed Cook remains status-addressable");
        assert_eq!(resumed_record.metadata["provider_executions_consumed"], 0);

        let recipe = super::super::load_recipe(cook_id).expect("same durable Cook recipe");
        let mut repeated_options = super::super::reconstruct_options_with_dispatcher(
            &recipe,
            Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
        )
        .expect("reconstruct materialized Cook retry");
        repeated_options.initial_run_id = retry.record.run_id.clone();
        repeated_options.initial_plan = agent_task_lifecycle::load_plan(&retry.record.run_id)
            .expect("load materialized durable retry plan");
        run_cook(CookContext::new(repeated_options, Arc::new(UnusedExecutor)))
            .expect("repeated continuation reuses the materialized workspace");
        let repeated_plan = agent_task_lifecycle::load_plan(&retry.record.run_id)
            .expect("load repeated durable retry plan");
        assert_eq!(
            repeated_plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.to_str().expect("utf8 workspace"))
        );
        assert_eq!(
            repeated_plan.tasks[0].metadata["cook_workspace_identity"],
            workspace_identity
        );
    });
}

#[cfg(unix)]
#[test]
fn pinned_missing_provider_ensures_an_unattached_branch_after_durable_admission() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
        let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
        let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
        let recipe_root_lifecycle_store = AgentTaskLifecycleStore::new(recipe_context.path_roots());
        let lifecycle_store = AgentTaskLifecycleStore::new(lifecycle_context.path_roots());
        let ambient_lifecycle_store =
            AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
        assert_ne!(recipe_context.data_dir(), lifecycle_context.data_dir());
        let root = tempfile::tempdir().expect("workspace root");
        let source = root.path().join("source");
        let workspace = root.path().join("workspace");
        for args in [
            vec![
                "init",
                "--quiet",
                "-b",
                "main",
                source.to_str().expect("source path"),
            ],
            vec![
                "-C",
                source.to_str().expect("source path"),
                "config",
                "user.email",
                "test@example.com",
            ],
            vec![
                "-C",
                source.to_str().expect("source path"),
                "config",
                "user.name",
                "Homeboy Test",
            ],
            vec![
                "-C",
                source.to_str().expect("source path"),
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "base",
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .status()
                .expect("git runs")
                .success());
        }
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let created = provider_dir.path().join("created");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\ncase \"$1\" in\nresolve)\n  if test -f '{}'; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@durable-ensure\",\"path\":\"{}\",\"branch\":\"durable-ensure\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else printf '%s\\n' '{{\"worktrees\":[]}}' ; fi\n  ;;\nensure)\n  test \"$2\" = fixture && test \"$3\" = main && test \"$4\" = durable-ensure && test \"$5\" = https://example.test/issues/12601 && test \"$6\" = agent_task_cook && test \"$7\" = durable-ensure-run && test \"$8\" = remove_on_success || exit 9\n  git -C '{}' worktree add --quiet '{}' durable-ensure && touch '{}'\n  ;;\nesac\n",
                created.display(),
                workspace.display(),
                source.display(),
                workspace.display(),
                created.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string(), "resolve".to_string()]),
                    ensure: Some(vec![
                        provider.display().to_string(),
                        "ensure".to_string(),
                        "{repo}".to_string(),
                        "{base}".to_string(),
                        "{head}".to_string(),
                        "{task_url}".to_string(),
                        "{purpose}".to_string(),
                        "{owner_run_ref}".to_string(),
                        "{cleanup_policy}".to_string(),
                    ]),
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");
        assert!(Command::new("git")
            .args(["branch", "durable-ensure", "HEAD"])
            .current_dir(&source)
            .status()
            .expect("create unattached branch")
            .success());

        let cook_id = "durable-ensure";
        let run_id = "durable-ensure-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending",
            "kind": "provider",
            "handle": options.to_worktree,
            "worktree_provider_id": "fixture",
            "provision_intent": {
                "repo": "fixture",
                "base": "main",
                "head": "durable-ensure",
                "task_url": "https://example.test/issues/12601",
            },
            "lifecycle_intent": {
                "purpose": "agent_task_cook",
                "cleanup_policy": "remove_on_success",
            },
        });

        recipe_store
            .persist_initial_recipe(&options)
            .expect("persist recipe in the injected recipe store");
        materialize_initial_cook_attempt_with_stores(&recipe_store, &lifecycle_store, &options)
            .expect("materialize run in the injected lifecycle store");
        materialize_pending_cook_workspace(&lifecycle_store, &mut options, None)
            .expect("materialize the ensured workspace in the injected lifecycle store");

        assert!(created.exists(), "ensure ran after durable Cook admission");
        assert!(recipe_store.recipe_exists(cook_id));
        let record = lifecycle_store
            .read_record(run_id)
            .expect("injected lifecycle store has the exact materialized run");
        assert_eq!(record.run_id, run_id);
        let plan = lifecycle_store
            .read_controller_plan(run_id)
            .expect("injected lifecycle store has the materialized plan");
        assert_eq!(plan.metadata["cook_provision"]["action"], "existing");
        assert_eq!(
            plan.metadata["cook_provision"]["lifecycle_intent"]["owner_run_ref"],
            run_id
        );
        assert_eq!(
            plan.metadata["cook_provision"]["worktree_provider_id"],
            "fixture"
        );
        assert_eq!(
            options.source_worktree_path.as_deref(),
            Some(workspace.as_path())
        );
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.to_str().expect("workspace path"))
        );
        assert!(!recipe_root_lifecycle_store
            .record_exists(run_id)
            .expect("recipe root has no lifecycle state"));
        assert!(!ambient_lifecycle_store
            .record_exists(run_id)
            .expect("ambient lifecycle state remains untouched"));
    });
}

#[cfg(unix)]
#[test]
fn deferred_ensure_only_provider_fails_after_durable_cook_admission() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let ensured = provider_dir.path().join("ensured");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\n", ensured.display()),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    ensure: Some(vec![provider.display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");

        let cook_id = "ensure-only";
        let run_id = "ensure-only-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending",
            "kind": "provider",
            "handle": options.to_worktree,
            "provision_intent": {
                "repo": "fixture",
                "base": "main",
                "head": "ensure-only",
                "task_url": "https://example.test/issues/12601",
            },
        });

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("Cook reports the postcondition failure");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "pre_execution_failure");
        assert!(ensured.exists(), "ensure ran after durable Cook admission");
        let record = agent_task_lifecycle::exact_record(run_id)
            .expect("ensure-only postcondition failure has an addressable run");
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "worktree_provider_lookup"
        );
    });
}

#[cfg(unix)]
#[test]
fn deferred_ensure_only_failure_uses_injected_recipe_and_lifecycle_stores() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
        let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
        let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
        let recipe_root_lifecycle_store = AgentTaskLifecycleStore::new(recipe_context.path_roots());
        let lifecycle_store = AgentTaskLifecycleStore::new(lifecycle_context.path_roots());
        let ambient_lifecycle_store =
            AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
        assert_ne!(recipe_context.data_dir(), lifecycle_context.data_dir());

        let provider_dir = tempfile::tempdir().expect("provider directory");
        let ensured = provider_dir.path().join("ensured");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\n", ensured.display()),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    ensure: Some(vec![provider.display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");

        let cook_id = "split-ensure-only";
        let run_id = "split-ensure-only-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending",
            "kind": "provider",
            "handle": options.to_worktree,
            "provision_intent": {
                "repo": "fixture",
                "base": "main",
                "head": "split-ensure-only",
                "task_url": "https://example.test/issues/12601",
            },
        });

        let result = run_cook_spine(
            &recipe_store,
            &lifecycle_store,
            options,
            Arc::new(UnusedExecutor),
            &mut DefaultCookSideEffects::new(|_, _, _, _| Ok(serde_json::json!({}))),
            None,
            false,
        )
        .expect("Cook reports the injected-store postcondition failure");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "pre_execution_failure");
        assert!(ensured.exists(), "ensure ran after durable Cook admission");
        assert!(recipe_store.recipe_exists(cook_id));
        let record = lifecycle_store
            .read_record(run_id)
            .expect("injected lifecycle store has the exact failed run");
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "worktree_provider_lookup"
        );
        assert!(!recipe_root_lifecycle_store
            .record_exists(run_id)
            .expect("recipe root has no lifecycle state"));
        assert!(!ambient_lifecycle_store
            .record_exists(run_id)
            .expect("ambient lifecycle state remains untouched"));
    });
}

#[cfg(unix)]
#[test]
fn review_12349_deferred_lookup_cancellation_preserves_cancelled_state() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let marker = provider_dir.path().join("started");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\nsleep 5\n", marker.display()),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve_identity: Some(vec![
                        provider.display().to_string(),
                        "token=provider-secret-must-not-persist".to_string(),
                    ]),
                    attest_safety: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save config");

        let cook_id = "review-12349-cancelled-lookup";
        let run_id = "review-12349-cancelled-lookup-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending", "kind": "provider", "handle": options.to_worktree,
            "worktree_provider_id": "fixture",
        });

        let cook = std::thread::spawn(move || {
            run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            marker.exists(),
            "provider lookup started before cancellation"
        );
        agent_task_lifecycle::cancel_run(run_id, Some("review cancellation"))
            .expect("cancel durable lookup run");

        let result = cook
            .join()
            .expect("Cook thread joins")
            .expect("Cook returns cancellation report");
        assert_eq!(result.value.status, "cancelled");
        let record = agent_task_lifecycle::status(run_id).expect("cancelled durable record");
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Cancelled
        );
        assert!(record.metadata["pre_execution_failure"].is_null());
        assert!(!record
            .metadata
            .to_string()
            .contains("provider-secret-must-not-persist"));
    });
}

#[cfg(unix)]
#[test]
fn short_cook_deadline_caps_resolve_timeout_without_starting_retry() {
    use crate::agent_task_timeout::{now_unix_ms, with_current_cook_deadline, CookDeadline};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    homeboy_core::test_support::with_isolated_home(|_| {
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("provider");
        std::fs::write(&provider, "#!/bin/sh\nsleep 1\n").expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 5_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve_identity: Some(vec![
                        provider.display().to_string(),
                        "{handle}".to_string(),
                    ]),
                    attest_safety: Some(vec!["true".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save config");
        let cook_id = "cook-split-identity-timeout";
        let run_id = "cook-split-identity-timeout-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.attempt_dispatcher = None;
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending", "kind": "provider", "handle": options.to_worktree,
            "worktree_provider_id": "fixture",
        });

        let phases = Arc::new(Mutex::new(Vec::new()));
        let observed_phases = Arc::clone(&phases);
        let observer = move |event: &CookProgressEvent<'_>| {
            observed_phases
                .lock()
                .expect("phase lock")
                .push((event.phase.to_string(), event.detail.map(str::to_string)));
            Ok(())
        };
        let started = std::time::Instant::now();
        let result = with_current_cook_deadline(
            Some(CookDeadline::from_unix_ms(
                now_unix_ms().saturating_add(200),
            )),
            || {
                run_cook(CookContext {
                    durable_observer: Some(&observer),
                    ..CookContext::new(options, Arc::new(UnusedExecutor))
                })
            },
        )
        .expect("Cook records short-deadline lookup timeout");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "effective lookup timeout must cap total pre-execution time"
        );
        assert_eq!(result.value.status, "pre_execution_failure");
        let record = agent_task_lifecycle::status(run_id).expect("durable timeout record");
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(record.metadata["pre_execution_failure"]["retryable"], true);
        assert_eq!(
            record.metadata["pre_execution_failure"]["details"]["worktree_provider_lookup"],
            "timed_out"
        );
        assert_eq!(
            record.metadata["pre_execution_failure"]["details"]
                ["worktree_provider_call_classification"],
            "timeout"
        );
        let plan = agent_task_lifecycle::load_plan(run_id).expect("durable timeout plan");
        assert_eq!(plan.metadata["worktree_provider_resolve"]["attempt"], 1);
        assert_eq!(
            plan.metadata["worktree_provider_resolve"]["retry_disposition"],
            "deadline_expired"
        );
        assert_eq!(
            plan.metadata["worktree_provider_resolve"]["configured_timeout_ms"],
            5_000
        );
        assert_eq!(
            plan.metadata["worktree_provider_resolve"]["provider_id"],
            "fixture"
        );
        assert!(plan.metadata["worktree_provider_resolve"]["events"]
            .as_array()
            .expect("resolve events")
            .iter()
            .any(|event| event["attempt"] == 1
                && event["effective_timeout_ms"].as_u64().unwrap_or(u64::MAX) < 5_000));
        assert!(plan.metadata["worktree_provider_resolve"]["cwd_recovery_command"].is_null());
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert!(record.metadata["provider_executions"].is_null());
        assert_eq!(
            record.metadata["pre_execution_failure"]["details"]["worktree_provider_resolve"],
            plan.metadata["worktree_provider_resolve"]
        );
        assert!(phases
            .lock()
            .expect("phase lock")
            .iter()
            .any(|(phase, detail)| {
                phase == "worktree_provider_lookup"
                    && detail.as_deref() == Some("starting bounded provider workspace lookup")
            }));
        let recipe = super::super::load_recipe(cook_id).expect("durable recipe");
        assert_eq!(
            recipe.attempts[0].plan.metadata["cook_provision"]["worktree_provider_id"],
            "fixture"
        );
        let retry = agent_task_lifecycle::retry(run_id, Some("cook-split-identity-timeout-retry"))
            .expect("split timeout remains retryable");
        assert_eq!(retry.metadata["retry_of"], run_id);
    });
}

#[test]
fn reconstruction_restores_a_materialized_pending_workspace_from_the_durable_plan() {
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut options = batch_cook_options(
        "cook-materialized-restart",
        Arc::new(AcceptedDetachedAttemptDispatcher),
    );
    let mut persisted = options.initial_plan.clone();
    persisted.tasks[0].workspace.root = Some(workspace.display().to_string());
    persisted.tasks[0].metadata["cook_workspace_identity"] = serde_json::json!({
        "canonical_path": workspace,
        "device": 1,
        "inode": 1,
    });
    persisted.metadata["cook_provision"] = serde_json::json!({
        "action": "existing",
        "workspace_identity": {
            "schema": "homeboy/worktree-provider-identity/v1",
            "provider_id": "split-only",
            "token": "opaque",
            "handle": options.to_worktree,
            "path": workspace,
            "branch": "branch",
            "primary": false,
            "latency_ms": 1,
            "budget_ms": 10
        }
    });

    rebind_baseline_continuation_workspace(&mut options, &persisted)
        .expect("reconstruct materialized workspace");

    assert_eq!(
        options.source_worktree_path.as_deref(),
        Some(workspace.as_path())
    );
    assert!(persisted.tasks[0]
        .metadata
        .get("cook_workspace_identity")
        .is_some());
}

#[cfg(unix)]
#[test]
fn split_only_provider_is_authoritative_without_source_override_or_legacy_commands() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("workspace root");
        let source = root.path().join("source");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&source).expect("source directory");
        assert!(Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(&source)
            .status()
            .expect("git init")
            .success());
        for args in [
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("git config")
                .success());
        }
        std::fs::write(source.join("tracked.txt"), "base\n").expect("write base");
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&source)
            .status()
            .expect("git add")
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(&source)
            .status()
            .expect("git commit")
            .success());
        assert!(Command::new("git")
            .args(["worktree", "add", "--quiet", "-b", "split-only"])
            .arg(&workspace)
            .current_dir(&source)
            .status()
            .expect("git worktree add")
            .success());
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nif test \"$1\" = safety; then printf '%s' '{{\"schema\":\"homeboy/worktree-provider-safety/v1\",\"identity_token\":\"opaque\",\"observed_at\":\"2026-01-01T00:00:00Z\",\"dirty\":false,\"unpushed\":false,\"fresh\":true,\"latency_ms\":0,\"budget_ms\":0}}'; else printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"opaque\",\"handle\":\"fixture@split-only\",\"path\":\"{}\",\"branch\":\"split-only\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'; fi\n",
                workspace.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve_identity: Some(vec![
                        provider.display().to_string(),
                        "identity".to_string(),
                        "{handle}".to_string(),
                    ]),
                    attest_safety: Some(vec![
                        provider.display().to_string(),
                        "safety".to_string(),
                        "{identity}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save config");
        let mut options =
            batch_cook_options("split-only", Arc::new(AcceptedDetachedAttemptDispatcher));
        options.to_worktree = "fixture@split-only".to_string();
        options.source_worktree_path = None;
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "existing",
            "workspace_identity": { "schema": "homeboy/worktree-provider-identity/v1", "provider_id": "fixture", "token": "opaque", "handle": "fixture@split-only", "path": workspace, "branch": "split-only", "primary": false, "latency_ms": 0, "budget_ms": 0 }
        });

        validate_cook_workspace(&options)
            .expect("provider-owned workspace passes mandatory dispatch revalidation");
    });
}

#[cfg(unix)]
#[test]
fn pending_cook_workspace_lookup_remains_bound_to_timed_out_provider() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider tempdir");
        let primary = temp.path().join("primary");
        let original = temp.path().join("original");
        let switched = temp.path().join("switched");
        for args in [
            vec![
                "init",
                "--initial-branch",
                "main",
                primary.to_str().expect("primary path"),
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "config",
                "user.email",
                "test@example.com",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "config",
                "user.name",
                "Test",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "worktree",
                "add",
                "-b",
                "original",
                original.to_str().expect("original path"),
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "worktree",
                "add",
                "-b",
                "switched",
                switched.to_str().expect("switched path"),
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .status()
                .expect("git runs")
                .success());
        }
        let marker = temp.path().join("provider-marker");
        let original_provider = temp.path().join("original-provider");
        let switched_provider = temp.path().join("switched-provider");
        for (script, label, branch, path) in [
            (&original_provider, "original", "original", &original),
            (&switched_provider, "switched", "switched", &switched),
        ] {
            std::fs::write(
                script,
                format!(
                    "#!/bin/sh\nprintf '%s' '{label}' > '{}'\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@provider-bound\",\"path\":\"{}\",\"branch\":\"{branch}\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                    marker.display(), path.display()
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(script)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(script, permissions).expect("make provider executable");
        }
        let provider_config =
            |script: &std::path::Path| homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![script.display().to_string(), "{handle}".to_string()]),
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
                        task_url: None,
                    },
                ),
            };
        // The timeout recipe predates the new provider. A re-selection would use
        // `a-switched`; durable retry must invoke only `z-original`.
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "z-original".to_string(),
            provider_config(&original_provider),
        );
        config.worktree_providers.insert(
            "a-switched".to_string(),
            provider_config(&switched_provider),
        );
        homeboy_core::defaults::save_config(&config).expect("save changed provider config");
        let mut options = batch_cook_options(
            "provider-bound",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending", "kind": "provider", "handle": options.to_worktree,
            "worktree_provider_id": "z-original",
        });

        let lifecycle_store =
            AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
        materialize_pending_cook_workspace(&lifecycle_store, &mut options, None)
            .expect("materialize original provider workspace");

        assert_eq!(
            std::fs::read_to_string(marker).expect("provider marker"),
            "original"
        );
        let canonical_original = std::fs::canonicalize(&original).expect("canonical original");
        assert_eq!(
            options.source_worktree_path.as_deref(),
            Some(canonical_original.as_path())
        );
        assert_eq!(
            options.initial_plan.tasks[0].workspace.root.as_deref(),
            canonical_original.to_str()
        );

        let captured = Arc::new(Mutex::new(None));
        let mut dispatch_options = batch_cook_options(
            "provider-bound-dispatch",
            Arc::new(WorkspaceCapturingDetachedAttemptDispatcher {
                plan: Arc::clone(&captured),
            }),
        );
        dispatch_options.to_worktree = "fixture@provider-bound".to_string();
        dispatch_options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending", "kind": "provider",
            "handle": dispatch_options.to_worktree,
            "worktree_provider_id": "z-original",
        });

        let report = run_cook(CookContext::new(dispatch_options, Arc::new(UnusedExecutor)))
            .expect("Cook dispatches the materialized provider workspace");
        assert_eq!(report.value.status, "in_flight");
        let dispatched = captured
            .lock()
            .expect("captured dispatch plan")
            .clone()
            .expect("provider dispatch received a plan");
        assert_eq!(
            dispatched.tasks[0].workspace.root.as_deref(),
            canonical_original.to_str(),
            "provider resolution projects its concrete path into the canonical dispatch plan"
        );
        assert_eq!(
            dispatched.metadata["cook_provision"]["workspace_identity"]["handle"],
            "fixture@provider-bound",
            "the logical handle remains the authenticated provider identity"
        );
        let recipe = super::super::load_recipe("provider-bound-dispatch")
            .expect("logical Cook recipe remains durable");
        assert!(recipe.attempts[0].plan.tasks[0].workspace.root.is_none());
        assert_eq!(
            recipe.attempts[0].plan.metadata["cook_provision"]["action"],
            "lookup_pending"
        );
    });
}

#[cfg(unix)]
#[test]
fn pending_repo_only_lookup_rejects_provider_workspace_from_another_repository() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider tempdir");
        let primary = temp.path().join("foreign-primary");
        let foreign = temp.path().join("foreign-worktree");
        for args in [
            vec![
                "init",
                "--initial-branch",
                "main",
                primary.to_str().expect("primary path"),
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "config",
                "user.email",
                "test@example.com",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "config",
                "user.name",
                "Test",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "remote",
                "add",
                "origin",
                "https://token:provider-secret@example.com/foreign.git",
            ],
            vec![
                "-C",
                primary.to_str().expect("primary path"),
                "worktree",
                "add",
                "-b",
                "foreign",
                foreign.to_str().expect("foreign path"),
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .status()
                .expect("git runs")
                .success());
        }
        let provider = temp.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@repo-only-mismatch\",\"path\":\"{}\",\"branch\":\"foreign\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                foreign.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy_core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy_core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy_core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy_core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string(), "{handle}".to_string()]),
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");
        let cook_id = "repo-only-mismatch";
        let run_id = "repo-only-mismatch-run";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.attempt_dispatcher = None;
        options.initial_run_id = run_id.to_string();
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "action": "lookup_pending", "kind": "provider", "handle": options.to_worktree,
            "worktree_provider_id": "fixture",
        });
        options.initial_plan.metadata["cook_repository_identity"] = serde_json::json!({
            "slug": "expected", "repository_name": "expected",
            "provenance": "--repo:requested-repository",
        });

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("Cook records identity failure");

        assert_eq!(result.value.status, "pre_execution_failure");
        let record = agent_task_lifecycle::status(run_id).expect("durable identity failure");
        assert_eq!(record.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            record.metadata["pre_execution_failure"]["phase"],
            "worktree_provider_lookup"
        );
        assert!(record.metadata["pre_execution_failure"]["message"]
            .as_str()
            .expect("message")
            .contains("does not match"));
        assert!(!record.metadata.to_string().contains("provider-secret"));
        let persisted = agent_task_lifecycle::load_plan(run_id).expect("persisted plan");
        assert!(persisted.tasks[0].workspace.root.is_none());
        assert!(persisted.tasks[0]
            .metadata
            .get("cook_workspace_identity")
            .is_none());
    });
}

#[test]
fn cook_failure_context_counts_preflight_cook_alias_as_zero_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-provider-execution-accounting";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = cook_id.to_string();
        options.max_attempts = 2;
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        super::super::materialize_initial_cook_attempt(&options)
            .expect("materialize preflight attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            cook_id,
            &options.initial_plan,
            "worktree_resolution",
            &Error::internal_io("worktree is unavailable", None).with_retryable(true),
        )
        .expect("record zero-execution preflight failure");

        let preflight = agent_task_lifecycle::exact_record(cook_id)
            .expect("read preflight record without resolving its Cook alias");
        assert_eq!(preflight.metadata["provider_executions_consumed"], 0);
        assert_eq!(
            preflight.metadata["pre_execution_failure"]["provider_executions_consumed"],
            0
        );

        let provider_run_id = format!("{cook_id}-attempt-2");
        super::super::record_recipe_attempt(cook_id, 2, &provider_run_id, &options.initial_plan)
            .expect("append provider attempt to recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&provider_run_id))
            .expect("submit provider attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, &provider_run_id)
            .expect("index provider attempt");
        assert_eq!(
            agent_task_lifecycle::status(cook_id)
                .expect("resolve Cook alias to provider attempt")
                .run_id,
            provider_run_id
        );
        assert_eq!(
            agent_task_lifecycle::exact_record(cook_id)
                .expect("retain exact preflight record")
                .metadata["provider_executions_consumed"],
            0
        );
        assert_eq!(
            agent_task_lifecycle::reserve_provider_execution(
                &provider_run_id,
                &options.initial_plan.tasks[0],
                1,
            )
            .expect("reserve one real provider execution"),
            agent_task_lifecycle::ProviderExecutionReservation::Acquired
        );

        let report = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "gate_failed",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(&provider_run_id),
        });
        let context = report.value.failure_context.expect("failure context");
        assert_eq!(context.provider_executions_consumed, 1);
        assert!(context.provider_budget_consumed);
        assert!(context
            .next_actions
            .iter()
            .any(|action| action.action == "resume"));
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
            Arc::new(ImmediateSuccessExecutor),
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
fn approved_empty_provider_failure_retry_stays_attached_and_becomes_continuation_candidate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-provider-failure-retry";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = format!("{cook_id}-attempt-1");
        options.max_attempts = 2;
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        super::super::materialize_initial_cook_attempt(&options)
            .expect("materialize first attempt");

        let failed = crate::agent_task_service::execution::run_submitted(
            options.initial_run_id.clone(),
            Arc::new(ProviderMissingExecutor),
        )
        .expect("record provider failure");
        assert_eq!(failed.exit_code, 1);
        assert_eq!(
            agent_task_lifecycle::status(&options.initial_run_id)
                .expect("failed Cook attempt")
                .state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );

        // Invoking retry is the explicit operator approval boundary. Its durable
        // reservation must immediately bind to the owning Cook recipe and index.
        let retry = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect("approved provider retry remains Cook-owned");
        let retry_run_id = retry.record.run_id;
        let patch_root = tempfile::tempdir().expect("candidate patch root");
        seed_substantive_candidate_aggregate(
            &retry_run_id,
            &options.initial_plan,
            &patch_root.path().join("candidate.patch"),
            "diff --git a/fixture.txt b/fixture.txt\n+index 0000000..1111111 100644\n--- a/fixture.txt\n+++ b/fixture.txt\n@@ -0,0 +1 @@\n+fixed\n",
        );

        let index = agent_task_lifecycle::cook_index(cook_id).expect("Cook retry index");
        assert_eq!(index.latest_run_id, retry_run_id);
        assert_eq!(index.attempts.len(), 2);
        let selection = agent_task_lifecycle::select_cook_candidate(cook_id)
            .expect("substantive retry is selected");
        assert_eq!(selection.run_id, retry_run_id);
        assert_eq!(selection.reason, "latest_substantive_candidate_pointer");
        assert_eq!(
            super::super::resolve_cook_continuation_run_id(cook_id)
                .expect("cook-continue resolves the retry candidate"),
            retry_run_id
        );
        assert_eq!(
            agent_task_lifecycle::status(&retry_run_id)
                .expect("successful retry")
                .state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn cancelled_provider_child_retry_stays_attached_to_its_cook() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-cancelled-provider-retry";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = format!("{cook_id}-attempt-1");
        options.max_attempts = 2;
        super::super::persist_initial_recipe(&options).expect("persist Cook recipe");
        super::super::materialize_initial_cook_attempt(&options)
            .expect("materialize first attempt");
        agent_task_lifecycle::cancel_run(&options.initial_run_id, Some("owner exited"))
            .expect("cancel detached provider child");

        let retry = crate::agent_task_service::retry(&options.initial_run_id, None, false, false)
            .expect("cancelled Cook child retry remains Cook-owned");

        assert_eq!(retry.record.metadata["retry_of"], options.initial_run_id);
        assert_eq!(retry.record.metadata["cook_id"], cook_id);
        assert_eq!(retry.record.metadata["cook_attempt"], 2);
        assert_eq!(
            agent_task_lifecycle::cook_index(cook_id)
                .expect("Cook retry index")
                .latest_run_id,
            retry.record.run_id
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
            Arc::new(TerminalSuccessExecutor),
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
        let result = run_cook(CookContext {
            durable_observer: Some(&move |event: &CookProgressEvent<'_>| {
                let (phase, cook, run) = (event.phase, event.cook_id, event.run_id);
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
            }),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
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
        let result = run_cook(CookContext {
            durable_observer: Some(&move |event: &CookProgressEvent<'_>| {
                let (phase, cook, run) = (event.phase, event.cook_id, event.run_id);
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
            }),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
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
        assert_eq!(result.value.status, "pre_execution_failure");
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
fn provider_dispatch_observes_a_durable_provider_start_boundary() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-provider-start-durable";
        let run_id = format!("{cook_id}-run");
        let phase_at_dispatch = Arc::new(Mutex::new(None));
        let options = batch_cook_options(
            cook_id,
            Arc::new(ProviderStartObservingDispatcher {
                run_id: run_id.clone(),
                phase_at_dispatch: Arc::clone(&phase_at_dispatch),
            }),
        );

        let result =
            run_cook(CookContext::new(options, Arc::new(UnusedExecutor))).expect("dispatch Cook");

        assert_eq!(result.value.status, "in_flight");
        assert_eq!(
            phase_at_dispatch
                .lock()
                .expect("provider start phase")
                .as_ref(),
            Some(&serde_json::json!({
                "phase": "provider_start",
                "state": "running",
                "runner_job_id": "provider-start-observing-job",
            }))
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

        // This case drives the spine directly, exactly as the former
        // `run_cook_with_boundaries_observed_inner` did: both roots resolve from
        // the ambient environment and no durable-error report or terminal
        // notification wraps the outcome.
        let ambient_recipe_store =
            CookRecipeStore::from_current_data_root().expect("ambient recipe store");
        let ambient_lifecycle_store =
            AgentTaskLifecycleStore::from_current_environment().expect("ambient lifecycle store");
        let result = run_cook_spine(
            &ambient_recipe_store,
            &ambient_lifecycle_store,
            options.clone(),
            Arc::new(UnusedExecutor),
            &mut DefaultCookSideEffects::new(|_, _, _, _| Ok(serde_json::json!({}))),
            None,
            false,
        )
        .expect("continuation repairs and dispatches recipe-bound retry");
        let repeated = run_cook_spine(
            &ambient_recipe_store,
            &ambient_lifecycle_store,
            options,
            Arc::new(UnusedExecutor),
            &mut DefaultCookSideEffects::new(|_, _, _, _| Ok(serde_json::json!({}))),
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
fn recipe_only_initial_attempt_recovers_once_without_provider_dispatch() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-recipe-only-initial";
        let run_id = "cook-recipe-only-initial-attempt-1";
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RecordingDetachedAttemptDispatcher {
                dispatches: Arc::clone(&dispatches),
            }),
        );
        options.initial_run_id = run_id.to_string();
        persist_initial_recipe(&options).expect("persist recipe before injected lifecycle failure");

        agent_task_lifecycle::fail_next_record_write_for_test();
        assert!(super::super::cook_pre_execution::recover_recipe_attempt(cook_id).is_err());
        assert!(!agent_task_lifecycle::run_record_exists(run_id).expect("record remains absent"));

        let recovered = super::super::cook_pre_execution::recover_recipe_attempt(cook_id)
            .expect("recover recipe-only initial attempt")
            .expect("recipe resolves one record");
        let repeated = super::super::cook_pre_execution::recover_recipe_attempt(cook_id)
            .expect("repeated recovery is idempotent")
            .expect("recipe still resolves one record");
        assert_eq!(recovered.run_id, run_id);
        assert_eq!(repeated.run_id, run_id);
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        let index = agent_task_lifecycle::cook_index(cook_id).expect("repaired Cook index");
        assert_eq!(index.attempts.len(), 1);
        assert_eq!(index.latest_run_id, run_id);
    });
}

#[test]
fn recipe_recovery_rejects_foreign_lifecycle_record_before_indexing() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let cook_id = "cook-recipe-foreign-record";
    let run_id = "cook-recipe-foreign-record-attempt-1";
    let mut options = batch_cook_options(
        cook_id,
        Arc::new(RecordingDetachedAttemptDispatcher {
            dispatches: Arc::new(AtomicUsize::new(0)),
        }),
    );
    options.initial_run_id = run_id.to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    submit_plan_in_test_store(&lifecycle_store, &options.initial_plan, Some(run_id))
        .expect("persist foreign lifecycle record");
    agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle_store, "other-cook", 1, run_id)
        .expect("bind foreign Cook ownership");

    let error = recover_recipe_attempt_with_stores(&recipe_store, &lifecycle_store, cook_id)
        .expect_err("foreign lifecycle record is rejected");
    assert!(error.message.contains("belongs to a different Cook"));
    assert!(
        !agent_task_lifecycle::cook_index_exists_in_store(&lifecycle_store, cook_id)
            .expect("no local index written")
    );
    assert_eq!(
        agent_task_lifecycle::exact_record_in_store(&lifecycle_store, run_id)
            .expect("foreign record remains")
            .metadata["cook_id"],
        "other-cook"
    );
}

#[test]
fn concurrent_cook_registration_preserves_every_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        with_strict_config_lock(|| {
            let cook_id = "cook-concurrent-index";
            let first = "cook-concurrent-index-attempt-1";
            let second = "cook-concurrent-index-attempt-2";
            let plan = batch_cook_options(
                cook_id,
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .initial_plan;
            agent_task_lifecycle::submit_plan(&plan, Some(first)).expect("submit first");
            agent_task_lifecycle::submit_plan(&plan, Some(second)).expect("submit second");
            let barrier = Arc::new(Barrier::new(2));
            let left = {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    agent_task_lifecycle::record_cook_attempt(cook_id, 1, first)
                })
            };
            let right = {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    agent_task_lifecycle::record_cook_attempt(cook_id, 2, second)
                })
            };
            left.join()
                .expect("first registration thread")
                .expect("register first");
            right
                .join()
                .expect("second registration thread")
                .expect("register second");
            let index = agent_task_lifecycle::cook_index(cook_id).expect("concurrent index");
            assert_eq!(index.attempts.len(), 2);
            assert!(index.attempts.iter().any(|attempt| attempt.run_id == first));
            assert!(index
                .attempts
                .iter()
                .any(|attempt| attempt.run_id == second));
            assert_eq!(index.latest_run_id, second);
        });
    });
}

#[test]
fn conflicting_cook_index_rejection_leaves_lifecycle_and_index_unchanged() {
    homeboy_core::test_support::with_isolated_home(|_| {
        with_strict_config_lock(|| {
            let cook_id = "cook-index-conflict";
            let run_id = "cook-index-conflict-attempt";
            let plan = batch_cook_options(
                cook_id,
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .initial_plan;
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))
                .expect("submit lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, 2, run_id)
                .expect("seed conflicting durable index");
            let before_record =
                agent_task_lifecycle::exact_record(run_id).expect("record before retry");
            let before_index =
                agent_task_lifecycle::cook_index(cook_id).expect("index before retry");

            let error = agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect_err("conflicting index attempt must reject before metadata write");
            assert!(error
                .message
                .contains("maps this run to a different attempt"));
            assert_eq!(
                agent_task_lifecycle::exact_record(run_id).expect("record after rejection"),
                before_record
            );
            assert_eq!(
                agent_task_lifecycle::cook_index(cook_id).expect("index after rejection"),
                before_index
            );
        });
    });
}

#[test]
fn strict_locked_retry_registration_uses_the_outer_transaction() {
    homeboy_core::test_support::with_isolated_home(|_| {
        with_strict_config_lock(|| {
            let cook_id = "cook-strict-locked-retry";
            let run_id = "cook-strict-locked-retry-attempt-2";
            let plan = batch_cook_options(
                cook_id,
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .initial_plan;
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("reserve retry run");
            homeboy_core::config::with_config_lock(|| {
                agent_task_lifecycle::record_cook_attempt_locked_in_store(
                    &agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                        .expect("lifecycle store"),
                    cook_id,
                    2,
                    run_id,
                )
                .expect("register retry through outer transaction");
                Ok(())
            })
            .expect("strict lock accepts one owner");
            assert_eq!(
                agent_task_lifecycle::cook_index(cook_id)
                    .expect("retry index")
                    .latest_run_id,
                run_id
            );
        });
    });
}

#[test]
fn strict_cross_cook_run_ownership_rejection_leaves_state_unchanged() {
    homeboy_core::test_support::with_isolated_home(|_| {
        with_strict_config_lock(|| {
            let run_id = "cook-cross-owner-attempt";
            let plan = batch_cook_options(
                "cook-cross-owner-a",
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .initial_plan;
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit shared run");
            agent_task_lifecycle::record_cook_attempt("cook-cross-owner-a", 1, run_id)
                .expect("bind first owner");
            let before_record = agent_task_lifecycle::exact_record(run_id).expect("record before");
            let before_index =
                agent_task_lifecycle::cook_index("cook-cross-owner-a").expect("first index before");

            let error = agent_task_lifecycle::record_cook_attempt("cook-cross-owner-b", 1, run_id)
                .expect_err("second Cook cannot claim the same run");
            assert!(error
                .message
                .contains("already owned by a different Cook attempt"));
            assert_eq!(
                agent_task_lifecycle::exact_record(run_id).expect("record after"),
                before_record
            );
            assert_eq!(
                agent_task_lifecycle::cook_index("cook-cross-owner-a").expect("first index after"),
                before_index
            );
            assert!(
                !agent_task_lifecycle::cook_index_exists("cook-cross-owner-b")
                    .expect("second index remains absent")
            );
        });
    });
}

#[test]
fn strict_terminal_lab_cook_registration_projects_authority_after_unlock_idempotently() {
    homeboy_core::test_support::with_isolated_home(|_| {
        with_strict_config_lock(|| {
            let cook_id = "cook-strict-terminal-lab";
            let run_id = "cook-strict-terminal-lab-attempt";
            let plan = batch_cook_options(
                cook_id,
                Arc::new(RecordingDetachedAttemptDispatcher {
                    dispatches: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .initial_plan;
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit Lab attempt");
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "strict-terminal-lab",
                    runner_job_id: "strict-terminal-job",
                    remote_workspace: "/runner/strict-terminal-workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )
            .expect("accept Lab handoff");
            let aggregate = crate::agent_task_scheduler::AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: plan.plan_id.clone(),
                status: crate::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded,
                totals: Default::default(),
                outcomes: Vec::new(),
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            };
            agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)
                .expect("terminalize Lab attempt");

            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("register terminal Cook without lock reentry");
            let receipt = agent_task_lifecycle::resolve_workspace_terminal_authority(
                run_id,
                "strict-terminal-lab",
                "/runner/strict-terminal-workspace",
                Some("strict-terminal-job"),
            )
            .expect("read terminal authority")
            .expect("authority projected after unlock");
            assert_eq!(receipt.run_id, run_id);

            agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
                .expect("replayed registration retains idempotent authority");
            assert!(agent_task_lifecycle::resolve_workspace_terminal_authority(
                run_id,
                "strict-terminal-lab",
                "/runner/strict-terminal-workspace",
                Some("strict-terminal-job"),
            )
            .expect("re-read terminal authority")
            .is_some());
        });
    });
}

#[test]
fn retry_after_admission_failure_restores_managed_workspace_after_baseline_cleanup() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temp source root");
        let primary = temp.path().join("primary");
        let source = temp.path().join("source");
        std::fs::create_dir(&primary).expect("create primary");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&primary)
                .status()
                .expect("run git")
                .success());
        };
        git(&["init"]);
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(primary.join("fixture.txt"), "base\n").expect("write base");
        git(&["add", "fixture.txt"]);
        git(&["commit", "-m", "base"]);
        git(&[
            "worktree",
            "add",
            "--detach",
            source.to_str().expect("UTF-8 source path"),
            "HEAD",
        ]);
        std::fs::write(source.join("fixture.txt"), "dirty candidate\n")
            .expect("write dirty candidate");

        let run_id = "cook-admission-retry-attempt-1";
        let mut options = batch_cook_options(
            "cook-admission-retry",
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "controller generation is held by another cook",
                runtime_recovery: None,
                phase: "controller_admission",
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(source.clone());
        options.to_worktree = source.display().to_string();
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": "source@cook-admission-retry",
            "root": source,
            "branch": "fix/cook-admission-retry",
        });

        run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("persist admission failure");
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
            serde_json::json!(source),
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

        let result = crate::agent_task_service::execution::run_submitted(
            retry.run_id,
            Arc::new(SucceedingExecutor),
        )
        .expect("retry reaches a real Git workspace");
        assert_eq!(result.exit_code, 0, "{:#?}", result.value);
    });
}

#[test]
fn retry_reports_missing_candidate_source_as_retryable_recovery() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temp source root");
        let primary = temp.path().join("primary");
        let source = temp.path().join("source");
        std::fs::create_dir(&primary).expect("create primary");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&primary)
                .status()
                .expect("run git")
                .success());
        };
        git(&["init"]);
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(primary.join("fixture.txt"), "base\n").expect("write base");
        git(&["add", "fixture.txt"]);
        git(&["commit", "-m", "base"]);
        git(&[
            "worktree",
            "add",
            "--detach",
            source.to_str().expect("UTF-8 source path"),
            "HEAD",
        ]);
        std::fs::write(source.join("fixture.txt"), "dirty candidate\n")
            .expect("write dirty candidate");

        let run_id = "cook-missing-worktree-attempt-1";
        let mut options = batch_cook_options(
            "cook-missing-worktree",
            Arc::new(AdmissionFailingAttemptDispatcher {
                message: "controller generation is held by another cook",
                runtime_recovery: None,
                phase: "controller_admission",
            }),
        );
        options.initial_run_id = run_id.to_string();
        options.source_worktree_path = Some(source.clone());
        options.to_worktree = source.display().to_string();
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": "source@cook-missing-worktree",
            "root": source,
            "branch": "fix/cook-missing-worktree",
        });

        run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("persist admission failure");
        std::fs::remove_dir_all(&source).expect("remove managed worktree");

        let error = agent_task_lifecycle::retry(run_id, Some("cook-missing-worktree-retry"))
            .expect_err("missing candidate source requires recovery");

        assert_eq!(error.retryable, Some(true), "{error:#?}");
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

        let failure = run_cook(CookContext::new(options.clone(), Arc::new(UnusedExecutor)))
            .expect("transport preparation failure is durably reported");
        assert_eq!(failure.exit_code, 1);
        assert_eq!(failure.value.status, "durable_failure");
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

        let resumed = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
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

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("cook records materialization failure");

        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(result.value.attempts.len(), 1);
        assert!(result.value.failure_context.is_some());
        assert!(super::super::recipe_exists(cook_id).expect("durable recipe lookup"));
        let record = agent_task_lifecycle::status(run_id).expect("persisted failed attempt");
        assert_eq!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(
            result
                .value
                .failure_context
                .unwrap()
                .provider_executions_consumed,
            0
        );
        assert!(agent_task_lifecycle::run_record_exists(run_id).expect("run record lookup"));
    });
}

#[test]
fn run_cook_persists_recipe_in_explicit_store_only() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("temp source root");
        let explicit_data = tempfile::tempdir().expect("explicit Cook data root");
        let store = CookRecipeStore::from_data_root(explicit_data.path().to_path_buf());
        let cook_id = "cook-explicit-recipe-store";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = "cook-explicit-recipe-store-attempt-1".to_string();
        options.source_worktree_path = Some(source.path().to_path_buf());

        let result = run_cook(CookContext {
            store: Some(&store),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
        })
        .expect("Cook reports the lifecycle materialization failure");

        assert_eq!(result.value.status, "durable_failure");
        assert!(store.recipe_exists(cook_id));
        assert!(!super::super::recipe_exists(cook_id).expect("ambient recipe lookup"));
    });
}

#[cfg(unix)]
#[test]
fn cook_claims_its_durable_attempt_before_slow_baseline_materialization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp source root");
        // Cook refuses provider execution against a primary checkout, so the
        // candidate workspace has to be a linked worktree of a primary.
        let primary = temp.path().join("primary");
        let source = temp.path().join("source");
        std::fs::create_dir(&primary).expect("create primary repository");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "agent@example.test"],
            vec!["config", "user.name", "Agent"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&primary)
                .status()
                .expect("run git")
                .success());
        }
        std::fs::write(primary.join("lib.rs"), "base\n").expect("write base");
        for args in [vec!["add", "lib.rs"], vec!["commit", "-m", "base"]] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&primary)
                .status()
                .expect("run git")
                .success());
        }
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                source.to_str().expect("UTF-8 source path"),
                "HEAD",
            ])
            .current_dir(&primary)
            .status()
            .expect("run git")
            .success());
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
        options.source_worktree_path = Some(source.clone());
        // Cook validates the destination before it stages a baseline, so the
        // fixture has to declare a real workspace rather than a bare handle.
        options.to_worktree = source.display().to_string();
        options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": "source@cook-slow-baseline",
            "root": source,
            "branch": "fix/cook-slow-baseline",
        });
        let resume_options = options.clone();
        let controller = std::thread::spawn(move || {
            run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
        });
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

        let resumed = run_cook(CookContext::new(resume_options, Arc::new(UnusedExecutor)))
            .expect("resume accepted handoff");
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

        let failure = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("transport preparation failure is durably reported");
        assert_eq!(failure.exit_code, 1);
        assert_eq!(failure.value.status, "durable_failure");
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

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("cook accepts detached handoff");

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
                    runtime_recovery: None,
                    phase: "controller_admission",
                }),
            );
        options.provider_command = Some("fixture-provider".to_string());
        let result = run_cook(CookContext::new(
            AgentTaskCookServiceOptions {
                initial_run_id: run_id.to_string(),
                ..options
            },
            Arc::new(UnusedExecutor),
        ))
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
                runtime_recovery: Some(
                    agent_task_lifecycle::AgentTaskLabRuntimeRecovery::refresh_homeboy(
                        "homeboy-lab",
                        "homeboy 1.2.3+required",
                        "required",
                    ),
                ),
                phase: "lab_staging_controller",
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = run_id.to_string();
        options.max_attempts = 2;

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("cook returns the persisted input failure");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(result.value.attempts.len(), 1);
        assert_eq!(result.value.history_run_ids, vec![run_id]);
        let context = result
            .value
            .failure_context
            .as_ref()
            .expect("failure context");
        let runtime_actions: Vec<_> = context
            .legal_actions
            .iter()
            .filter(|action| action.action == "refresh_lab_runtime")
            .collect();
        assert_eq!(runtime_actions.len(), 1);
        assert_eq!(
            runtime_actions[0].command,
            "homeboy runner refresh-homeboy homeboy-lab --ref required --reconnect"
        );
        assert!(context
            .next_actions
            .iter()
            .all(|action| action.action != "refresh_lab_runtime"));
        assert_eq!(
            result.value.terminal_phase.as_deref(),
            Some("lab_staging_controller")
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
fn cook_ignores_untrusted_or_malformed_lab_runtime_recovery_metadata() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-untrusted-runtime-recovery";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("persist current invocation");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &options.initial_run_id)
            .expect("index current invocation");

        for metadata in [
            serde_json::json!({
                "phase": "lab_staging_controller",
                "details": {
                    "homeboy_handoff_identity": {
                        "recovery_command": "arbitrary command"
                    }
                }
            }),
            serde_json::json!({
                "phase": "lab_staging_controller",
                "details": {
                    "lab_handoff_runtime_recovery": {
                        "schema": "homeboy/agent-task-lab-runtime-recovery/v1",
                        "runner_id": "homeboy-lab",
                        "requested_build_identity": "homeboy 1.2.3+required"
                    }
                }
            }),
            serde_json::json!({
                "phase": "controller_admission",
                "details": {
                    "lab_handoff_runtime_recovery": {
                        "schema": "homeboy/agent-task-lab-runtime-recovery/v1",
                        "runner_id": "homeboy-lab",
                        "requested_build_identity": "homeboy 1.2.3+required",
                        "build_ref": "required"
                    }
                }
            }),
        ] {
            agent_task_lifecycle::rewrite_record_for_test(&options.initial_run_id, |record| {
                record.metadata["pre_execution_failure"] = metadata.clone();
            })
            .expect("persist adversarial metadata");
            let report = cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: "pre_execution_failure",
                disposition: CookDisposition::Terminal,
                attempts: Vec::new(),
                finalization: None,
                stop_reason: None,
                exit_code: 1,
                invocation_latest_run_id: Some(&options.initial_run_id),
            });
            assert!(report
                .value
                .failure_context
                .expect("failure context")
                .legal_actions
                .iter()
                .all(|action| action.action != "refresh_lab_runtime"));
        }
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

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("cook records transport retries");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "pre_execution_failure");
        assert_eq!(result.value.attempts.len(), 2);
        assert_eq!(dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(result.value.history_run_ids.len(), 2);
        assert_eq!(
            result.value.history_run_ids[0],
            "cook-retryable-transport-attempt-1"
        );
        assert!(result.value.history_run_ids[1]
            .starts_with("cook-retryable-transport-attempt-1-transport-retry"));
        assert!(result
            .value
            .attempts
            .iter()
            .all(|attempt| attempt.attempt == 1));
        let recipe = super::super::load_recipe(cook_id).expect("transport retries are durable");
        assert_eq!(recipe.attempts.len(), 2);
        assert!(recipe.attempts.iter().all(|attempt| attempt.attempt == 1));
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
fn cook_reattests_each_initial_baseline_before_detached_transport_retry() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("temporary repository root");
        let primary = temp.path().join("primary");
        let source = temp.path().join("source");
        std::fs::create_dir(&primary).expect("create primary repository");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git fixture command");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(&primary, &["init", "--initial-branch=main"]);
        git(&primary, &["config", "user.email", "agent@example.test"]);
        git(&primary, &["config", "user.name", "Agent"]);
        std::fs::write(primary.join("fixture.txt"), "base\n").expect("write base fixture");
        git(&primary, &["add", "fixture.txt"]);
        git(&primary, &["commit", "-m", "base"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--detach",
                source.to_str().expect("UTF-8 source path"),
                "HEAD",
            ],
        );
        std::fs::write(source.join("fixture.txt"), "candidate\n").expect("write candidate fixture");

        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut options = batch_cook_options(
            "cook-baseline-attestation",
            Arc::new(AttestingRetryableTransportDispatcher {
                observations: Arc::clone(&observations),
            }),
        );
        options.initial_run_id = "cook-baseline-attestation-attempt-1".to_string();
        options.source_worktree_path = Some(source.clone());
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_plan.tasks[0].workspace.root = Some(source.display().to_string());
        let source_identity = crate::agent_task_workspace_identity::attest_workspace(&source)
            .expect("attest admitted source workspace");
        options.initial_plan.tasks[0].metadata["cook_workspace_identity"] = source_identity.clone();

        let result = run_cook(CookContext::new(options, Arc::new(UnusedExecutor)))
            .expect("record bounded transport retry");

        assert_eq!(result.value.attempts.len(), 2);
        let observations = observations.lock().expect("baseline observations");
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|(_, _, _, matches)| *matches));
        assert_ne!(observations[0].0, observations[1].0);
        assert!(observations
            .iter()
            .all(|(_, _, predecessor, _)| predecessor == &source_identity));
    });
}

#[test]
fn explicit_local_continuation_replaces_exhausted_auto_lab_transport_without_replaying_lab() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let config_root = homeboy_core::paths::homeboy().expect("resolve isolated config root");
        std::fs::create_dir_all(&config_root).expect("create isolated config root");
        std::fs::write(
            config_root.join("homeboy.json"),
            r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
        )
        .expect("disable host-capacity admission for local continuation");
        let (_checkout_guard, checkout) =
            homeboy_core::test_support::shared_committed_git_repo_fixture(
                "local-placement-override",
            );
        let worktree_parent = tempfile::tempdir().expect("create worktree parent");
        let worktree = worktree_parent.path().join("candidate");
        homeboy_core::test_support::run_git_fixture_command(
            &checkout,
            &[
                "worktree",
                "add",
                "-b",
                "local-placement-override",
                worktree.to_str().expect("worktree path"),
            ],
        );
        let lab_dispatches = Arc::new(AtomicUsize::new(0));
        let local_starts = Arc::new(AtomicUsize::new(0));
        let cook_id = "cook-local-placement-override";
        let mut options = batch_cook_options(
            cook_id,
            Arc::new(RetryableTransportFailingAttemptDispatcher {
                dispatches: Arc::clone(&lab_dispatches),
            }),
        );
        options.provider_command = Some("fixture-provider".to_string());
        options.initial_run_id = format!("{cook_id}-attempt-1");
        options.to_worktree = "fixture@local-placement-override".to_string();
        options.source_worktree_path = Some(worktree.clone());
        options.initial_plan.tasks[0].workspace.root = Some(worktree.display().to_string());
        options.initial_plan.tasks[0].workspace.kind = Some("homeboy-worktree".to_string());
        options.initial_plan.tasks[0].workspace.materialization = serde_json::json!({
            "kind": "homeboy-worktree",
            "id": options.to_worktree,
            "root": worktree,
            "branch": "local-placement-override",
        });
        homeboy_core::worktree::adopt(homeboy_core::worktree::WorktreeAdoptOptions {
            handle: options.to_worktree.clone(),
            path: worktree.display().to_string(),
            kind: Some("test-fixture".to_string()),
            provenance: None,
        })
        .expect("register original Cook worktree");
        let prior = homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
            "fixture-policy",
            "v1",
            homeboy_lab_runner_contract::ExecutionPlacementIdentity {
                repository: "fixture".to_string(),
                workspace: "fixture-worktree".to_string(),
                task: "provider".to_string(),
                candidate: Some("candidate-a".to_string()),
                base: Some("base-a".to_string()),
            },
            homeboy_lab_runner_contract::Placement::Auto,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
            homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab,
            Some(
                homeboy_lab_runner_contract::ExecutionPlacementRunnerSelection {
                    runner_id: "fixture-lab".to_string(),
                    source: homeboy_lab_runner_contract::RunnerSelectionSource::Policy,
                },
            ),
            homeboy_lab_runner_contract::ExecutionPlacementFallback {
                local_allowed: true,
                reason: Some("fixture policy allows local recovery".to_string()),
            },
            homeboy_lab_runner_contract::ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        );
        options.initial_plan.metadata["execution_placement_decision"] =
            serde_json::to_value(&prior).expect("serialize auto Lab decision");

        let exhausted = run_cook(CookContext::new(options.clone(), Arc::new(UnusedExecutor)))
            .expect("exhaust the bounded Lab transport retry");
        assert_eq!(exhausted.value.status, "pre_execution_failure");
        assert_eq!(lab_dispatches.load(Ordering::SeqCst), 2);
        let exhausted_run_id = exhausted
            .value
            .latest_run_id
            .expect("exhausted transport attempt is durable");
        let recipe_before = super::super::load_recipe(cook_id).expect("durable recipe");

        let local = homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
            prior.policy_id.clone(),
            prior.policy_revision.clone(),
            prior.identity.clone(),
            homeboy_lab_runner_contract::Placement::Local,
            homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
            homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local,
            None,
            homeboy_lab_runner_contract::ExecutionPlacementFallback {
                local_allowed: false,
                reason: None,
            },
            homeboy_lab_runner_contract::ExecutionPlacementOverrideAuthorization {
                authorized: true,
                authority: Some("operator --placement local".to_string()),
            },
        );
        agent_task_lifecycle::transition_execution_placement_for_continuation(
            &exhausted_run_id,
            local.clone(),
        )
        .expect("authorize local continuation after auto Lab preacceptance failure");

        options.initial_run_id = exhausted_run_id.clone();
        options.initial_plan = agent_task_lifecycle::load_controller_plan(&exhausted_run_id)
            .expect("load transitioned plan");
        options.attempt_dispatcher = None;
        let continued = run_cook(CookContext::new(
            options,
            Arc::new(RecordingImmediateSuccessExecutor {
                starts: Arc::clone(&local_starts),
            }),
        ))
        .expect("local continuation starts provider work");

        assert_eq!(
            lab_dispatches.load(Ordering::SeqCst),
            2,
            "no Lab connection after override"
        );
        assert_eq!(
            local_starts.load(Ordering::SeqCst),
            1,
            "one local provider start: {continued:#?}"
        );
        assert_eq!(continued.value.cook_id, cook_id);
        let recipe_after =
            super::super::load_recipe(cook_id).expect("durable recipe after override");
        assert_eq!(
            recipe_after.promotion_transport,
            recipe_before.promotion_transport
        );
        assert_eq!(recipe_after.gate_policy, recipe_before.gate_policy);
        assert_eq!(recipe_after.retry_budget, recipe_before.retry_budget);
        assert_eq!(recipe_after.finalization, recipe_before.finalization);
        assert_eq!(
            recipe_after.attempts[0].plan.tasks[0].instructions,
            recipe_before.attempts[0].plan.tasks[0].instructions
        );
        let transitioned =
            agent_task_lifecycle::status(&exhausted_run_id).expect("transition evidence");
        assert_eq!(
            transitioned.metadata["execution_placement_decision"]["decision_id"],
            local.decision_id
        );
        assert_eq!(
            transitioned.metadata["execution_placement_decision"]["identity"]["candidate"],
            "candidate-a"
        );
        assert_eq!(
            transitioned.metadata["transport_admission_reset"]["kind"],
            "placement_transition"
        );
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

/// Build a batch of children that observe their own dispatch.
///
/// Admission is materialized up front exactly as the sibling batch cases do:
/// the batch owns concurrent dispatch, not concurrent controller admission.
fn dispatch_counting_batch(
    cook_ids: &[&str],
    dispatched: &Arc<Mutex<Vec<String>>>,
) -> Vec<AgentTaskCookServiceOptions> {
    let cooks = cook_ids
        .iter()
        .map(|cook_id| {
            batch_cook_options(
                cook_id,
                Arc::new(DispatchCountingAttemptDispatcher {
                    dispatched: Arc::clone(dispatched),
                }),
            )
        })
        .collect::<Vec<_>>();
    for cook in &cooks {
        agent_task_lifecycle::submit_plan(&cook.initial_plan, Some(&cook.initial_run_id))
            .expect("submit attempt");
    }
    cooks
}

/// Leave a durable terminal record under a child's Cook id.
///
/// `batch_cook_options` deliberately gives a child a different `cook_id` and
/// `initial_run_id` so ordering bugs stay visible, while real fanout children
/// carry the same value for both (`cook-<id>`). The durable-terminality check
/// keys on `cook_id`, so a record has to exist there for these cases to
/// exercise what production exercises.
///
/// Terminality is reached through `cancel_run` rather than a synthesized state
/// because that is the one established path a durable owner uses to stop a
/// child, so the fixture and the mechanism under test cannot drift apart.
fn terminalize_cook_alias(cook: &AgentTaskCookServiceOptions) {
    agent_task_lifecycle::submit_plan(&cook.initial_plan, Some(&cook.cook_id))
        .expect("submit cook alias");
    agent_task_lifecycle::cancel_run(&cook.cook_id, Some("already finished"))
        .expect("terminalize the cook alias");
    assert!(
        agent_task_lifecycle::status(&cook.cook_id)
            .expect("cook alias is readable")
            .state
            .is_terminal(),
        "the fixture must actually be durably terminal, or it proves nothing"
    );
}

#[test]
fn a_daemon_owned_cook_batch_never_re_runs_a_durably_terminal_child() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
            .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                "agent_task_service::cook::tests::a_daemon_owned_cook_batch_never_re_runs_a_durably_terminal_child_process",
            ])
            .status()
            .expect("run process-isolated daemon-owned cook batch");
    assert!(status.success());
}

/// The correctness property that matters most. A coordinator recovered by its
/// durable owner must carry the wave forward, never start a child that already
/// finished — that would mean a second provider attempt, and for a finalized
/// child, a second pull request.
#[test]
#[ignore = "invoked by a_daemon_owned_cook_batch_never_re_runs_a_durably_terminal_child"]
fn a_daemon_owned_cook_batch_never_re_runs_a_durably_terminal_child_process() {
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let cooks = dispatch_counting_batch(&["done", "fresh"], &dispatched);
    terminalize_cook_alias(&cooks[0]);

    let result = run_cook_batch_with_control(
        AgentTaskCookBatchOptions {
            batch_id: "fixture-daemon-owned-batch".to_string(),
            cooks,
            max_concurrency: 2,
        },
        Arc::new(UnusedExecutor),
        AgentTaskCookBatchControl::daemon_owned(),
    )
    .expect("batch completes");

    // The dispatch boundary is the proof: the finished child was never started.
    let dispatched = dispatched.lock().expect("dispatched children").clone();
    assert_eq!(dispatched, vec!["fresh-run".to_string()]);

    // The finished child still occupies its slot in the report, with the state
    // it actually reached, so `total` and per-child ordering are unchanged.
    assert_eq!(result.value.total, 2);
    assert_eq!(result.value.cooks[0].cook_id, "done");
    assert_eq!(result.value.cooks[0].status, "cancelled");
    assert!(result.value.cooks[0].result.is_none());
    assert_eq!(result.value.cooks[1].cook_id, "fresh");
}

#[test]
fn an_unowned_cook_batch_still_runs_every_child() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
        .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
        .args([
            "--ignored",
            "--exact",
            "agent_task_service::cook::tests::an_unowned_cook_batch_still_runs_every_child_process",
        ])
        .status()
        .expect("run process-isolated unowned cook batch");
    assert!(status.success());
}

/// The other half of the contract, and the one that protects every existing
/// caller: without a durable owner the coordinator behaves exactly as it always
/// has. A coordinator that skipped durably-terminal children unconditionally
/// would silently change what a re-run of `fanout run-plan` does.
#[test]
#[ignore = "invoked by an_unowned_cook_batch_still_runs_every_child"]
fn an_unowned_cook_batch_still_runs_every_child_process() {
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let cooks = dispatch_counting_batch(&["done", "fresh"], &dispatched);
    terminalize_cook_alias(&cooks[0]);

    let result = run_cook_batch(
        AgentTaskCookBatchOptions {
            batch_id: "fixture-unowned-batch".to_string(),
            cooks,
            max_concurrency: 2,
        },
        Arc::new(UnusedExecutor),
    )
    .expect("batch completes");

    // Asserted on the cell's provenance rather than on the dispatch list,
    // because what must not change is that the unowned coordinator *ran the
    // child* — whatever the child then decided. A cell observed from durable
    // state is the one shape that carries neither a report nor an error, so its
    // absence is exactly the property.
    let done = &result.value.cooks[0];
    assert_eq!(done.cook_id, "done");
    assert!(
        done.result.is_some() || done.error.is_some(),
        "an unowned coordinator answers to nobody, so it must not start skipping \
         work on durable state it was never told to consult: {done:?}"
    );
}

#[test]
fn cancelling_a_daemon_owned_cook_batch_stops_it_starting_further_children() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
            .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                "agent_task_service::cook::tests::cancelling_a_daemon_owned_cook_batch_stops_it_starting_further_children_process",
            ])
            .status()
            .expect("run process-isolated cook batch cancellation");
    assert!(status.success());
}

/// Cancellation has to reach children that have no lifecycle record yet.
///
/// An unclaimed child has nothing for `cancel_run` to terminalize, so a
/// coordinator that only read child records would keep starting fresh cooks
/// after the wave was cancelled — which is why the durable marker exists and
/// why this asserts on the dispatch boundary rather than on the report.
#[test]
#[ignore = "invoked by cancelling_a_daemon_owned_cook_batch_stops_it_starting_further_children"]
fn cancelling_a_daemon_owned_cook_batch_stops_it_starting_further_children_process() {
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let cooks = dispatch_counting_batch(&["first", "second"], &dispatched);
    let batch_id = "fixture-cancelled-batch";
    crate::agent_task_batch::persist_fanout_run_batch(
        batch_id,
        batch_id,
        &cooks
            .iter()
            .map(|cook| crate::agent_task_batch::FanoutRunBatchChild {
                task_id: cook.cook_id.clone(),
                run_id: cook.initial_run_id.clone(),
            })
            .collect::<Vec<_>>(),
        serde_json::json!({}),
    )
    .expect("persist batch record");
    crate::agent_task_batch::record_coordinator_cancellation(batch_id, "operator cancelled")
        .expect("record cancellation");

    let result = run_cook_batch_with_control(
        AgentTaskCookBatchOptions {
            batch_id: batch_id.to_string(),
            cooks,
            max_concurrency: 2,
        },
        Arc::new(UnusedExecutor),
        AgentTaskCookBatchControl::daemon_owned(),
    )
    .expect("a cancelled batch still returns a complete report");

    assert!(
        dispatched.lock().expect("dispatched children").is_empty(),
        "no provider attempt may start after the wave is cancelled"
    );
    assert_eq!(result.value.total, 2);
    for cell in &result.value.cooks {
        assert_eq!(cell.status, "cancelled");
        assert_eq!(cell.exit_code, 1);
    }
    assert_eq!(result.value.cancelled, 2);
}

#[test]
fn a_daemon_owned_cook_batch_publishes_each_child_as_it_terminalizes() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let status = context
            .controller_runtime_command(homeboy_core::test_support::TestBinary::CurrentTest)
            .args([
                "--ignored",
                "--exact",
                "agent_task_service::cook::tests::a_daemon_owned_cook_batch_publishes_each_child_as_it_terminalizes_process",
            ])
            .status()
            .expect("run process-isolated cook batch publication");
    assert!(status.success());
}

/// The daemon supervises from durable state, so a child's outcome has to reach
/// the batch record as it happens rather than only when the whole coordinator
/// returns — otherwise a wave that died at child nine of ten would checkpoint
/// as having finished nothing.
#[test]
#[ignore = "invoked by a_daemon_owned_cook_batch_publishes_each_child_as_it_terminalizes"]
fn a_daemon_owned_cook_batch_publishes_each_child_as_it_terminalizes_process() {
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let cooks = dispatch_counting_batch(&["first", "second"], &dispatched);
    let batch_id = "fixture-published-batch";
    crate::agent_task_batch::persist_fanout_run_batch(
        batch_id,
        batch_id,
        &cooks
            .iter()
            .map(|cook| crate::agent_task_batch::FanoutRunBatchChild {
                task_id: cook.cook_id.clone(),
                run_id: cook.initial_run_id.clone(),
            })
            .collect::<Vec<_>>(),
        serde_json::json!({}),
    )
    .expect("persist batch record");

    run_cook_batch_with_control(
        AgentTaskCookBatchOptions {
            batch_id: batch_id.to_string(),
            cooks,
            max_concurrency: 1,
        },
        Arc::new(UnusedExecutor),
        AgentTaskCookBatchControl::daemon_owned(),
    )
    .expect("batch completes");

    let record = crate::agent_task_batch::read_batch_record(batch_id).expect("read batch record");
    let finalizations = record.metadata["child_finalizations"]
        .as_object()
        .expect("child finalizations are published");
    // Keyed the same way `resume_cook_batch` keys them, so a live coordinator
    // and a resumed one converge on one view rather than two.
    assert!(finalizations.contains_key("first-run"), "{finalizations:?}");
    assert!(
        finalizations.contains_key("second-run"),
        "{finalizations:?}"
    );
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
            Arc::new(UnusedExecutor),
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
        Arc::new(UnusedExecutor),
    )
    .expect("batch completes despite an individual cook failure");

    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert_eq!(result.exit_code, 0, "{:#?}", result.value);
    assert_eq!(result.value.status, "running", "{:#?}", result.value);
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
        error: (exit_code != 0).then(|| {
            AgentTaskCookCellError::declared(
                "agent_task.infrastructure_admission_denied",
                "infrastructure admission failed",
                false,
            )
        }),
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
                runtime_tools: Vec::new(),
                metadata: Value::Null,
            }],
        );
        let result = run_cook(CookContext::new(
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
                draft_pr: false,
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
            Arc::new(UnusedExecutor),
        ))
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
fn orphaned_recipe_materializes_once_and_replays_from_durable_inputs() {
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

        let recovered = run_cook(CookContext::new(options.clone(), Arc::new(UnusedExecutor)))
            .expect("recover orphan");
        assert_eq!(recovered.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        let record = agent_task_lifecycle::status(run_id).expect("materialized run record");
        assert_eq!(record.runner_job_id(), Some("recording-daemon-job"));

        let replayed = run_cook(CookContext::new(options.clone(), Arc::new(UnusedExecutor)))
            .expect("idempotent replay");
        assert_eq!(replayed.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        let replayed_record = agent_task_lifecycle::status(run_id).unwrap();
        assert_eq!(replayed_record.run_id, record.run_id);
        assert_eq!(replayed_record.state, record.state);
        assert_eq!(replayed_record.runner_id(), record.runner_id());
        assert_eq!(replayed_record.runner_job_id(), record.runner_job_id());

        let mut changed = options;
        changed.title = "changed immutable finalization title".to_string();
        let replayed = run_cook(CookContext::new(changed, Arc::new(UnusedExecutor)))
            .expect("durable recipe remains authoritative");
        assert_eq!(replayed.value.status, "in_flight");
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
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
        git(&source, &["init", "--initial-branch=main"]);
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
                    accept_inherited_failures: false,
                },
                |_| Ok(None),
                Arc::new(ReviewFormOnlyExecutor),
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
        assert!(
            backend.body.contains("Homeboy (fixture)"),
            "{}",
            backend.body
        );
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
        seed_substantive_candidate_aggregate(
            run_id,
            &options.initial_plan,
            &temp.path().join("candidate.patch"),
            "diff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-base\n+candidate\n",
        );
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
                accept_inherited_failures: false,
            },
            |_| Ok(None),
            Arc::new(ReviewFormOnlyExecutor),
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
        let selected_candidate = result
            .value
            .selected_candidate
            .as_ref()
            .expect("source patch remains the canonical candidate");
        assert_eq!(selected_candidate["run_id"], run_id);
        assert_eq!(
            result
                .value
                .completion()
                .expect("in-process completion")
                .state,
            "pr_finalized",
            "the form-only continuation owns the canonical source candidate receipt"
        );
        let source_record = agent_task_lifecycle::exact_record(run_id).expect("source record");
        assert_eq!(
            cook_completion(
                Some(selected_candidate),
                true,
                source_record.metadata.get("cook_finalization"),
                Some(run_id),
            )
            .expect("exact source status completion")
            .state,
            "pr_finalized",
            "exact source status resolves the bound form-only receipt"
        );
        agent_task_lifecycle::rewrite_record_for_test(follow_up_run_id, |record| {
            record
                .metadata
                .as_object_mut()
                .expect("record metadata object")
                .remove("cook_finalization");
        })
        .expect("remove form-only receipt");
        let awaiting = result.value.completion().expect("recoverable completion");
        assert_eq!(awaiting.state, "candidate_awaiting_finalization");
        assert_eq!(
            awaiting.next_action.expect("recovery action").command,
            format!("homeboy agent-task finalize-pr --recover {follow_up_run_id}"),
            "the bound form-only continuation owns failed-finalization recovery"
        );
        let unrelated_run_id = format!("{cook_id}-unrelated-continuation");
        super::super::record_recipe_attempt(cook_id, 3, &unrelated_run_id, &options.initial_plan)
            .expect("persist unrelated recipe attempt");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&unrelated_run_id))
            .expect("persist unrelated attempt record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 3, &unrelated_run_id)
            .expect("link unrelated attempt");
        assert_eq!(
            canonical_cook_recovery_run_id(cook_id).as_deref(),
            Some(follow_up_run_id.as_str()),
            "an unrelated newer continuation cannot replace the bound form-only recovery owner"
        );
        let follow_up_promotion = persisted_promotion_for_attempt(follow_up_run_id)
            .unwrap()
            .expect("form-only continuation carries promoted candidate");
        let source_promotion = persisted_promotion_for_attempt(run_id)
            .unwrap()
            .expect("source attempt retains its normalized gate proof");
        assert_eq!(
            follow_up_promotion.provenance["cook_follow_up"]["kind"],
            "review_form_only"
        );
        assert_eq!(
            canonical_cook_recovery_run_id(cook_id).as_deref(),
            Some(follow_up_run_id.as_str()),
            "Cook alias recovery remains bound to the form-only continuation"
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
        assert!(backend.body.contains("**Tool(s):**"), "{}", backend.body);
        assert!(
            backend.body.contains("fixture-provider"),
            "{}",
            backend.body
        );
        assert!(backend.body.contains("**Model:**"), "{}", backend.body);
        assert!(
            backend.body.contains("fixture-model-review"),
            "{}",
            backend.body
        );
        assert!(backend.body.contains("**Used for:** test"));
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
            .adopt(|_| Ok(None), Arc::new(ReviewFormOnlyExecutor), &mut backend)
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
fn adoption_review_child_plan_remains_one_bounded_execution() {
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
            .adopt(|_| Ok(None), Arc::new(ReviewFormOnlyExecutor), &mut backend)
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
            .adopt(|_| Ok(None), Arc::new(ReviewFormOnlyExecutor), &mut backend)
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
            .adopt(|_| Ok(None), Arc::new(ReviewFormOnlyExecutor), &mut backend)
            .expect("adoption review consumes its bounded allowance");
        let replay = fixture
            .adopt(
                |_| panic!("terminal adoption must not reconstruct a dispatcher"),
                Arc::new(UnusedExecutor),
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
            .adopt(|_| Ok(None), Arc::new(ReviewFormOnlyExecutor), &mut backend)
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
                Arc::new(ReviewFormOnlyExecutor),
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
                Arc::new(UnusedExecutor),
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
                Arc::new(UnusedExecutor),
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
                    Arc::new(UnusedExecutor),
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
                Arc::new(UnusedExecutor),
                &mut backend,
            )
            .expect("budget exhaustion is a durable Cook result");
        assert_eq!(exhausted.value.status, "execution_budget_exhausted");
        assert!(
            exhausted
                .value
                .stop_reason
                .as_deref()
                .is_some_and(|reason| reason
                    .contains("--max-provider-executions 2 --max-same-provider-retries 1")),
            "form-only remediation exhaustion must provide a copyable correction: {:#?}",
            exhausted.value.stop_reason
        );
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
                Arc::new(UnusedExecutor),
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
                run_cook(CookContext {
                    side_effects: Some(Box::new(DefaultCookSideEffects::new(
                        |_, options, run_id, promotion| {
                            finalize_cook_pr_with_backend(
                                options,
                                run_id,
                                promotion,
                                &mut resumed_backend,
                            )
                        },
                    ))),
                    ..CookContext::new(options, Arc::new(UnusedExecutor))
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
                Arc::new(UnusedExecutor),
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
    // One context, two stores: the ambient resolver built both roots from the
    // same environment, so a single hermetic root reproduces that topology
    // exactly while mutating no process state (#7505).
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adopt-existing-run";
    let run_id = "cook-adopt-existing-run-attempt-1";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = run_id.to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist lifecycle record");
    agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, run_id)
        .expect("link cook attempt");

    let (record, recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        run_id,
        None,
    )
    .expect("adoption resolves existing run");

    assert_eq!(recipe.cook_id, cook_id);
    assert_eq!(record.run_id, run_id);
    assert_eq!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Queued
    );
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
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adopt-equivalent-attempts";
    let first_run_id = "cook-adopt-equivalent-attempts-1";
    let second_run_id = "cook-adopt-equivalent-attempts-2";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = first_run_id.to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    recipe_store
        .record_recipe_attempt(cook_id, 2, second_run_id, &options.initial_plan)
        .expect("persist second recipe attempt");
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.initial_plan, first_run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist first lifecycle record");
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.initial_plan, second_run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist second lifecycle record");
    agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle_store, cook_id, 1, first_run_id)
        .expect("index first attempt");
    agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, second_run_id)
        .expect("make later failed attempt the mutable index target");

    let (record, recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        None,
    )
    .expect("equivalent attempts resolve deterministically");

    assert_eq!(recipe.cook_id, cook_id);
    assert_eq!(record.run_id, first_run_id);
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
                .expect("continuation follows the latest feedback attempt"),
            third_run_id
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
                .expect("recipe attempts preserve chronological continuation without an index"),
            third_run_id
        );
    });
}

#[test]
fn cook_alias_continuation_starts_from_failed_gate_feedback_attempt() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-alias-selected-attempt";
        // Attempt one deliberately shares the Cook ID, matching the durable
        // identity shape that previously bypassed continuation selection.
        let original_run_id = cook_id;
        let gate_feedback_run_id = "cook-alias-selected-attempt-attempt-2-gate-feedback";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.max_attempts = 3;
        options.initial_run_id = original_run_id.to_string();
        super::super::persist_initial_recipe(&options).expect("persist recipe");
        super::super::record_recipe_attempt(
            cook_id,
            2,
            gate_feedback_run_id,
            &options.initial_plan,
        )
        .expect("persist gate-feedback recipe attempt");
        for (attempt, run_id) in [(1, original_run_id), (2, gate_feedback_run_id)] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id)
                .expect("persist Cook attempt");
        }
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        seed_substantive_candidate_aggregate(
            original_run_id,
            &options.initial_plan,
            &temp.path().join("original.patch"),
            patch,
        );
        seed_review_form_aggregate(gate_feedback_run_id, &options.initial_plan);
        let mut gate_feedback = promotion(gate_feedback_run_id);
        gate_feedback.status = crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed;
        gate_feedback.deterministic_gates[0].status =
            crate::agent_task_gate::AgentTaskGateStatus::Failed;
        gate_feedback.deterministic_gates[0].exit_code = 1;
        agent_task_lifecycle::record_promotion(
            gate_feedback_run_id,
            serde_json::to_value(gate_feedback).expect("serialize failed gate-feedback evidence"),
        )
        .expect("persist failed gate-feedback evidence");

        let selection = agent_task_lifecycle::select_cook_candidate(cook_id)
            .expect("select earlier substantive candidate for promotion");
        assert_eq!(selection.run_id, original_run_id);
        assert_eq!(selection.attempt, 1);
        assert_eq!(selection.reason, "latest_substantive_candidate_pointer");

        let continuation_run_id = super::super::resolve_cook_continuation_run_id(cook_id)
            .expect("Cook alias resolves latest failed gate-feedback attempt");
        assert_eq!(continuation_run_id, gate_feedback_run_id);
        assert_eq!(
            super::super::resolve_cook_continuation_run_id(gate_feedback_run_id)
                .expect("exact distinct attempt remains addressable"),
            gate_feedback_run_id
        );
        let recipe = super::super::load_recipe(cook_id).expect("load recipe");
        assert_eq!(
            resumable_cook_run_id(&recipe, cook_id, &continuation_run_id, 2, false),
            None,
            "continuation resumes attempt 2 so the remaining budget reaches attempt 3"
        );
        assert!(agent_task_lifecycle::cook_attempt_run_id(cook_id, 3)
            .starts_with(&format!("{cook_id}-attempt-3-")));
        let record = agent_task_lifecycle::status(&continuation_run_id)
            .expect("failed gate-feedback attempt remains inspectable");
        super::super::validate_recipe_attempt_record(&recipe, &continuation_run_id, &record)
            .expect("matching Cook metadata passes the identity fence");
        let mut cross_cook_record = record;
        cross_cook_record.ensure_metadata_object().insert(
            "cook_id".to_string(),
            Value::String("other-cook".to_string()),
        );
        let error = super::super::validate_recipe_attempt_record(
            &recipe,
            &continuation_run_id,
            &cross_cook_record,
        )
        .expect_err("cross-Cook metadata must fail the identity fence");
        assert!(error.message.contains("observed Cook `other-cook`"));
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

/// Terminality must survive a status this binary has never seen.
///
/// The previous implementation inferred it from the status string, so a Cook
/// reporting an unrecognized status — which is reachable, because several
/// exits pass `finalization["status"]` straight through and fall back to
/// `"unknown"` — was classified by whichever way that inference happened to
/// guess. The declared disposition is what the orchestrator's completion
/// depends on, so it must not consult the vocabulary at all.
#[test]
fn terminality_is_declared_by_the_exit_not_read_from_the_status_string() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-disposition-authority";
        let unrecognized = "some_status_from_a_newer_binary";

        let terminal = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: unrecognized,
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: None,
        });
        assert!(terminal.value.disposition.is_terminal());
        assert_eq!(terminal.value.disposition.phase(), "terminal");
        // The unrecognized status is reported verbatim, never rewritten.
        assert_eq!(terminal.value.status, unrecognized);

        // The one exit that hands work to a durable owner stays non-terminal,
        // so no completion is announced while that owner is still working.
        let in_flight = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "in_flight",
            disposition: CookDisposition::InFlight,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: Some("provider attempt accepted by the runner daemon".to_string()),
            exit_code: 0,
            invocation_latest_run_id: None,
        });
        assert!(!in_flight.value.disposition.is_terminal());
        assert_eq!(in_flight.value.disposition.phase(), "in_flight");

        // Consumers reading the serialized report get the same answer without
        // having to pattern-match the open status vocabulary.
        let serialized = serde_json::to_value(&terminal.value).expect("serialize cook report");
        assert_eq!(serialized["disposition"], "terminal");
        let serialized = serde_json::to_value(&in_flight.value).expect("serialize cook report");
        assert_eq!(serialized["disposition"], "in_flight");
    });
}

#[test]
fn selection_required_serializes_as_a_known_terminal_partial_failure() {
    let report = cook_report(CookReportInput {
        cook_id: "cook-selection-lifecycle".to_string(),
        status: "selection_required",
        disposition: CookDisposition::Terminal,
        attempts: Vec::new(),
        finalization: None,
        stop_reason: None,
        exit_code: 1,
        invocation_latest_run_id: None,
    });

    let serialized = serde_json::to_value(&report.value).expect("serialize Cook report");
    assert_eq!(serialized["status"], "selection_required");
    assert_eq!(serialized["lifecycle_status"], "partial_failure");
    assert_eq!(serialized["terminal"], true);
    assert_eq!(serialized["retryable"], false);
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
        agent_task_lifecycle::record_promotion(
            latest_run_id,
            serde_json::to_value(promotion(latest_run_id))
                .expect("serialize newer otherwise eligible promotion"),
        )
        .expect("persist unrelated newer promotion");

        let report = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "completed",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 0,
            invocation_latest_run_id: Some(latest_run_id),
        });
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
            canonical_cook_recovery_run_id(cook_id).as_deref(),
            Some(selected_run_id),
            "Cook alias recovery follows the canonical older candidate rather than a newer unrelated eligible promotion"
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
        let finalized = serde_json::json!({ "status": "review_ready", "pr_number": 12291 });
        agent_task_lifecycle::record_cook_finalization(selected_run_id, finalized.clone())
            .expect("persist selected candidate receipt");
        assert_eq!(
            canonical_candidate_finalization(
                Some(&provenance),
                Some(&serde_json::json!({ "status": "review_ready", "pr_number": 99999 })),
                Some(latest_run_id),
            ),
            Some(finalized),
            "exact status for a newer non-substantive attempt resolves the canonical candidate receipt"
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

        let report = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "execution_budget_exhausted",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: Some(
                "provider execution stopped because max_provider_executions was exhausted"
                    .to_string(),
            ),
            exit_code: 1,
            invocation_latest_run_id: Some(&latest_empty_run_id),
        });

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
        assert_eq!(
            context
                .legal_actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<Vec<_>>(),
            ["status", "diagnose"],
            "an exhausted Cook must not advertise lifecycle commands that cannot advance it"
        );

        agent_task_lifecycle::rewrite_record_for_test(candidate_run_id, |record| {
            record.metadata["latest_promotion"]["command_evidence"][0]["exit_code"] =
                serde_json::json!(1);
        })
        .expect("remove destination proof");
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
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adopt-conflicting-attempts";
    let first_run_id = "cook-adopt-conflicting-attempts-1";
    let second_run_id = "cook-adopt-conflicting-attempts-2";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = first_run_id.to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    let mut conflicting_plan = options.initial_plan.clone();
    conflicting_plan.plan_id = "conflicting-plan".to_string();
    recipe_store
        .record_recipe_attempt(cook_id, 2, second_run_id, &conflicting_plan)
        .expect("persist conflicting second recipe attempt");
    lifecycle_store
        .submit_plan_with_runtime_admission(&conflicting_plan, second_run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist exact conflicting attempt record without Cook metadata");

    let error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        None,
    )
    .expect_err("conflicting recipe adoption requires an explicit run id");

    assert_eq!(error.details["field"], "cook_recipe.attempts");
    assert!(error.message.contains(first_run_id));
    assert!(error.message.contains(second_run_id));
    assert!(error
        .message
        .contains(&format!("homeboy agent-task adopt {cook_id} --attempt 1")));

    let (record, recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        second_run_id,
        None,
    )
    .expect("an exact existing attempt run id selects its owning recipe");
    assert_eq!(recipe.cook_id, cook_id);
    assert_eq!(record.run_id, second_run_id);
}

#[test]
fn adoption_attempt_selector_disambiguates_a_first_run_id_equal_to_its_cook_id() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adopt-attempt-id-collision";
    let second_run_id = "cook-adopt-attempt-id-collision-attempt-2";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = cook_id.to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    let mut conflicting_plan = options.initial_plan.clone();
    conflicting_plan.plan_id = "attempt-two-policy".to_string();
    recipe_store
        .record_recipe_attempt(cook_id, 2, second_run_id, &conflicting_plan)
        .expect("persist conflicting second recipe attempt");
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.initial_plan, cook_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist first lifecycle record");

    let error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        None,
    )
    .expect_err("conflicting attempts require an explicit selector");
    assert!(error.message.contains("--attempt 1"));
    assert!(error.message.contains("plan attempt-two-policy"));

    let (record, recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        Some(1),
    )
    .expect("attempt selector resolves the first attempt despite the ID collision");
    assert_eq!(recipe.cook_id, cook_id);
    assert_eq!(record.run_id, cook_id);
}

#[test]
fn adoption_attempt_selector_resolves_cook_and_child_run_ids_to_the_same_attempt() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-adopt-attempt-id-forms";
    let child_run_id = "cook-adopt-attempt-id-forms-attempt-2";
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = "cook-adopt-attempt-id-forms-attempt-1".to_string();
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");
    let mut second_plan = options.initial_plan.clone();
    second_plan.plan_id = "attempt-two-policy".to_string();
    recipe_store
        .record_recipe_attempt(cook_id, 2, child_run_id, &second_plan)
        .expect("persist second recipe attempt");
    lifecycle_store
        .submit_plan_with_runtime_admission(&second_plan, child_run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("persist second lifecycle record");

    let (cook_record, cook_recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        Some(2),
    )
    .expect("Cook id selects attempt two");
    let (run_record, run_recipe) = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        child_run_id,
        Some(2),
    )
    .expect("child attempt run id selects the same attempt");

    assert_eq!(cook_recipe.cook_id, cook_id);
    assert_eq!(run_recipe.cook_id, cook_recipe.cook_id);
    assert_eq!(cook_record.run_id, child_run_id);
    assert_eq!(run_record.run_id, cook_record.run_id);

    let cook_error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        Some(3),
    )
    .expect_err("Cook id rejects an undeclared attempt");
    let run_error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        child_run_id,
        Some(3),
    )
    .expect_err("child attempt run id rejects the same undeclared attempt");
    assert_eq!(run_error.details, cook_error.details);
    assert_eq!(run_error.message, cook_error.message);
}

#[test]
fn adoption_rejects_an_id_that_is_a_cook_and_another_cooks_attempt() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let shared_id = "cook-adoption-ambiguous-id";
    let mut cook_options =
        batch_cook_options(shared_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    cook_options.initial_run_id = "cook-adoption-ambiguous-id-attempt-1".to_string();
    recipe_store
        .persist_initial_recipe(&cook_options)
        .expect("persist Cook recipe");

    let foreign_cook_id = "cook-adoption-foreign-owner";
    let foreign_options =
        batch_cook_options(foreign_cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    recipe_store
        .persist_initial_recipe(&foreign_options)
        .expect("persist foreign Cook recipe");
    recipe_store
        .record_recipe_attempt(foreign_cook_id, 2, shared_id, &foreign_options.initial_plan)
        .expect("record foreign attempt with the colliding id");
    let foreign_recipe = recipe_store
        .load_recipe(foreign_cook_id)
        .expect("reload foreign Cook recipe");
    assert!(foreign_recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == shared_id));
    assert_eq!(
        recipe_store
            .load_recipe_for_attempt(shared_id)
            .expect("find foreign recipe by its attempt")
            .expect("foreign attempt is persisted")
            .cook_id,
        foreign_cook_id
    );

    let error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        shared_id,
        Some(1),
    )
    .expect_err("cross-Cook identifier collision fails closed");
    assert_eq!(error.details["field"], "run_or_cook_id");
    assert!(error.message.contains(shared_id));
    assert!(error.message.contains(foreign_cook_id));
}

#[test]
fn adoption_ambiguity_describes_policy_choices_without_sensitive_config() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
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
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist recipe");

    let mut second_plan = options.initial_plan.clone();
    second_plan.plan_id = "attempt-two-policy".to_string();
    second_plan.tasks[0].executor.backend = "provider-two".to_string();
    second_plan.tasks[0].executor.selector = Some("fallback".to_string());
    second_plan.tasks[0].executor.model = Some("model-two".to_string());
    second_plan.tasks[0].policy.apply = "publish".to_string();
    recipe_store
        .record_recipe_attempt(cook_id, 2, second_run_id, &second_plan)
        .expect("persist policy-different attempt");

    let error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        cook_id,
        None,
    )
    .expect_err("policies require selection");
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
}

#[test]
fn adoption_rejects_unknown_run_or_cook_ids() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let error = resolve_adoption_target_with_attempt_in_stores(
        &recipe_store,
        &lifecycle_store,
        "unknown-adoption-target",
        None,
    )
    .expect_err("unknown adoption target fails closed");

    assert_eq!(error.details["field"], "run_or_cook_id");
    assert!(error
        .message
        .contains("unknown agent-task run or durable cook id"));
}

#[derive(Default)]
struct CaptureBackend {
    body: String,
    committed: bool,
    pushed: bool,
    created: bool,
    candidate_state: Option<crate::agent_task_finalization::AgentTaskPrCandidateState>,
    committed_sha: Option<String>,
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
    fn hydrate_run_in_store(
        &mut self,
        lifecycle_store: &AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<RunLifecycleRecord> {
        if self.hydrate_run_id.is_some() {
            return RealAgentTaskPrFinalizationBackend
                .hydrate_run_in_store(lifecycle_store, run_id);
        }
        self.hydrate_run(run_id)
    }
    fn hydrate_gate_proof_in_store(
        &mut self,
        lifecycle_store: &AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<AgentTaskPrDurableGateProof> {
        if self.hydrate_run_id.is_some() || self.hydrate_gate_proof_run_id.is_some() {
            return RealAgentTaskPrFinalizationBackend
                .hydrate_gate_proof_in_store(lifecycle_store, run_id);
        }
        if let Some(mut promotion) = self.synthetic_gate_proof.clone() {
            promotion.source.run_id = Some(run_id.to_string());
            if let Ok(Some(persisted)) =
                persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)
            {
                if let Some(follow_up) = persisted.provenance.get("cook_follow_up") {
                    promotion.provenance["cook_follow_up"] = follow_up.clone();
                }
            }
            return Ok(AgentTaskPrDurableGateProof {
                run_id: run_id.to_string(),
                promotion,
            });
        }
        self.hydrate_gate_proof(run_id)
    }
    fn validate_candidate_in_store(
        &mut self,
        _lifecycle_store: &AgentTaskLifecycleStore,
        options: &crate::agent_task_finalization::AgentTaskPrFinalizationOptions,
    ) -> Result<()> {
        self.validate_candidate(options)
    }
    fn current_branch(&mut self, _path: &str) -> Result<String> {
        Ok("fix/8058".to_string())
    }
    fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
        Ok(vec!["src/lib.rs".to_string()])
    }
    fn candidate_state(
        &mut self,
        _path: &str,
        _base: &crate::agent_task_finalization::AgentTaskPrResolvedBase,
        _head: &str,
    ) -> Result<crate::agent_task_finalization::AgentTaskPrCandidateState> {
        Ok(self.candidate_state.clone().unwrap_or(
            crate::agent_task_finalization::AgentTaskPrCandidateState::Dirty {
                changed_files: vec!["src/lib.rs".to_string()],
            },
        ))
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
        proof.commit_sha = Some(
            self.committed_sha
                .clone()
                .unwrap_or_else(|| "candidate-sha".to_string()),
        );
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
        draft: bool,
    ) -> Result<AgentTaskPrRef> {
        self.created = true;
        self.body = body.to_string();
        Ok(AgentTaskPrRef {
            number: 8058,
            url: "https://github.com/Extra-Chill/homeboy/pull/8058".to_string(),
            is_draft: draft,
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

/// A real, existing directory for promotion fixtures to point at.
///
/// `--path` overrides are validated for existence (`a50702c9d`), so a fixture
/// that names a literal like `/repo` now fails resolution rather than being
/// treated as opaque test data. These tests already run inside
/// `with_isolated_home`, which sets `HOME`, so materialize the directory there
/// and let every fixture share it.
fn promotion_worktree_path() -> String {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let path = home.join("promotion-fixture-worktree");
    std::fs::create_dir_all(&path).expect("promotion fixture worktree");
    path.display().to_string()
}

fn promotion(run_id: &str) -> AgentTaskPromotionReport {
    let worktree_path = promotion_worktree_path();
    serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": {"kind": "aggregate", "task_id": "task", "run_id": run_id},
            "to_worktree": "homeboy@8058",
            "target": {"worktree": "homeboy@8058", "path": worktree_path},
            "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
            "changed_files": ["src/lib.rs"],
            "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-lc", "cargo test --locked agent_task_promotion --lib"], "exit_code": 0, "candidate_checkout": {"schema": "homeboy/agent-task-gate-candidate-checkout/v1", "commit": "candidate", "tree": "candidate-tree", "candidate_sha256": "candidate-sha"}}],
            "gate_results": [{"id": "gate", "name": "cargo test --locked agent_task_promotion --lib", "kind": "command", "status": "passed"}],
            "operator_notification": {"status": "completed", "message": "complete"},
            "verified_base": {"base": "main", "sha": "verified-base"},
            "provenance": {"worktree_path": worktree_path, "candidate_checkout": {"schema": "homeboy/agent-task-gate-candidate-checkout/v1", "commit": "candidate", "tree": "candidate-tree", "candidate_sha256": "candidate-sha"}}
        })).unwrap()
}

fn promotion_with_existing_path(run_id: &str, path: &std::path::Path) -> AgentTaskPromotionReport {
    let mut promotion = promotion(run_id);
    let path = path.display().to_string();
    promotion.target.path = Some(path.clone());
    promotion.provenance["worktree_path"] = serde_json::json!(path);
    promotion
}

#[test]
fn canonical_completion_accepts_green_and_recipe_authorized_inherited_gate_evidence() {
    let mut green = promotion("canonical-green");
    green.patch_artifact.sha256 = Some("canonical-sha".to_string());
    assert!(canonical_finalization_eligible(&green, false, true));

    let mut inherited = green.clone();
    inherited.status = AgentTaskPromotionStatus::GateFailed;
    inherited.deterministic_gates[0].status =
        crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure;
    inherited.deterministic_gates[0].exit_code = 1;
    inherited.deterministic_gates[0].baseline_comparison =
        Some(crate::agent_task_gate::AgentTaskGateBaselineComparison {
            base_ref: "main".to_string(),
            exit_code: 1,
            failure_fingerprint: "inherited".to_string(),
            matches_candidate_failure: true,
            result: crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed,
        });
    assert!(canonical_finalization_eligible(&inherited, true, true));
    assert!(!canonical_finalization_eligible(&inherited, false, true));
}

#[test]
fn canonical_completion_requires_a_valid_review_form() {
    let green = promotion("canonical-missing-review-form");
    assert!(!canonical_finalization_eligible(&green, false, false));
}

fn tracked_promotion_continuation_options(
    cook_id: &str,
    run_id: &str,
    target: &std::path::Path,
) -> AgentTaskCookServiceOptions {
    let mut options = promotion_claim_options(cook_id, run_id);
    options.initial_plan =
        batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher)).initial_plan;
    options.to_worktree = target.display().to_string();
    options.source_worktree_path = Some(target.to_path_buf());
    options
}

fn record_tracked_promotion_continuation(
    options: &AgentTaskCookServiceOptions,
    target: &std::path::Path,
) {
    if !CookRecipeStore::from_current_data_root()
        .unwrap()
        .recipe_exists(&options.cook_id)
    {
        persist_initial_recipe(options).unwrap();
    }
    agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
        .unwrap();
    agent_task_lifecycle::rewrite_record_for_test(&options.initial_run_id, |record| {
        record.metadata["cook_id"] = serde_json::json!(options.cook_id);
        record.metadata["cook_attempt"] = serde_json::json!(1);
    })
    .unwrap();
    agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, &options.initial_run_id)
        .unwrap();
    let mut checkpoint = serde_json::to_value(promotion(&options.initial_run_id)).unwrap();
    checkpoint["status"] = serde_json::json!("gate_failed");
    checkpoint["deterministic_gates"][0]["status"] = serde_json::json!("failed");
    checkpoint["deterministic_gates"][0]["exit_code"] = serde_json::json!(1);
    checkpoint["gate_results"][0]["status"] = serde_json::json!("failed");
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
        "branch": "cook-candidate",
        "head": fingerprint.head
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
        "worktree_path": target,
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
fn cook_owned_unpushed_candidate_requires_one_exact_promoted_commit() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let target = tempfile::tempdir().expect("target");
        for args in [
            vec!["init", "--quiet", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(target.path())
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(target.path().join("tracked.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["checkout", "--quiet", "-b", "cook-candidate"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(target.path().join("tracked.txt"), "promoted\n").unwrap();
        let options = tracked_promotion_continuation_options(
            "cook-owned-unpushed",
            "run-cook-owned-unpushed",
            target.path(),
        );
        record_tracked_promotion_continuation(&options, target.path());
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "cook: retain candidate"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        let continuation = tracked_promotion_continuation(&options)
            .unwrap()
            .expect("durable Cook attribution");
        assert!(cook_owned_unpushed_destination(&continuation)
            .unwrap()
            .is_some());

        let base = Command::new("git")
            .args(["rev-parse", "HEAD^"])
            .current_dir(target.path())
            .output()
            .unwrap();
        let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
        assert!(Command::new("git")
            .args(["branch", "side", &base])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["checkout", "--quiet", "side"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(target.path().join("side.txt"), "side\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "side.txt"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "side"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["checkout", "--quiet", "cook-candidate"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["merge", "--quiet", "--no-ff", "-s", "ours", "side", "-m", "merge"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        let error = cook_owned_unpushed_destination(&continuation).unwrap_err();
        assert_eq!(
            error.details["workspace"]["classification"],
            "workspace.cook_owned_unpushed_commit_mismatch"
        );
        assert_eq!(error.details["workspace"]["reason"], "merge_commit");
    });
}

#[test]
fn verify_replacement_gates_replays_completed_proof_without_rerunning_gates() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("candidate");
        std::fs::create_dir(&source).expect("create source");
        for args in [
            vec!["init", "--quiet", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git")
                .success());
        }
        std::fs::write(source.join("tracked.txt"), "base\n").expect("write base");
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&source)
            .status()
            .expect("stage base")
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(&source)
            .status()
            .expect("commit base")
            .success());
        assert!(Command::new("git")
            .args(["worktree", "add", "--quiet", "-b", "cook-candidate"])
            .arg(&target)
            .current_dir(&source)
            .status()
            .expect("create candidate")
            .success());
        std::fs::write(target.join("tracked.txt"), "promoted\n").expect("write candidate");

        let mut options = batch_cook_options(
            "cook-verify-replacement",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_run_id = "run-verify-replacement".to_string();
        options.to_worktree = "fixture@cook-candidate".to_string();
        options.source_worktree_path = Some(target.clone());
        persist_initial_recipe(&options).expect("persist recipe");
        record_tracked_promotion_continuation(&options, &target);
        agent_task_lifecycle::record_cook_attempt(
            "cook-verify-replacement",
            1,
            "run-verify-replacement",
        )
        .expect("link attempt");

        let patch_path = target
            .parent()
            .expect("candidate parent")
            .join("candidate.patch");
        let patch = std::fs::read_to_string(&patch_path).expect("read candidate patch");
        seed_patch_alias_aggregate(
            "run-verify-replacement",
            &options.initial_plan,
            &[("patch", &patch_path, &patch)],
        );
        let mut failed = serde_json::to_value(
            persisted_promotion_for_attempt("run-verify-replacement")
                .expect("read failed promotion")
                .expect("failed promotion"),
        )
        .expect("serialize failed promotion");
        failed["source"]["task_id"] =
            serde_json::json!(options.initial_plan.tasks[0].task_id.clone());
        failed["target"]["dirty"] = serde_json::json!(true);
        agent_task_lifecycle::record_promotion("run-verify-replacement", failed)
            .expect("align source task evidence");

        let gate_log = temp.path().join("replacement-gate-runs");
        let gate = format!(
            "test \"$(cat tracked.txt)\" = promoted; printf ran >> {}",
            gate_log.display()
        );
        let replacement = verify_replacement_gates(
            "cook-verify-replacement",
            VerifyGateOptions {
                verify: vec![gate.clone()],
                ..Default::default()
            },
            "Chris approved corrected gate evidence".to_string(),
        )
        .expect("replacement gates complete");

        assert_eq!(replacement.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(replacement.deterministic_gates.len(), 1);
        assert_eq!(
            replacement.deterministic_gates[0].command,
            vec!["sh".to_string(), "-lc".to_string(), gate]
        );
        let replay = verify_replacement_gates(
            "cook-verify-replacement",
            VerifyGateOptions {
                verify: vec![replacement.deterministic_gates[0].command[2].clone()],
                ..Default::default()
            },
            "Chris approved corrected gate evidence".to_string(),
        )
        .expect("completed replacement proof replays without rerunning gates");
        assert_eq!(replay.status, replacement.status);
        assert_eq!(replay.command_evidence, replacement.command_evidence);
        assert_eq!(
            std::fs::read_to_string(gate_log).expect("read gate log"),
            "ran"
        );
        let record = agent_task_lifecycle::status("run-verify-replacement").expect("read record");
        assert_eq!(record.metadata["promotions"].as_array().unwrap().len(), 3);
        assert_eq!(
            agent_task_lifecycle::operation_claim(
                "run-verify-replacement",
                "verify-replacement:run-verify-replacement"
            )
            .expect("read replacement claim")
            .expect("replacement claim")
            .state,
            agent_task_lifecycle::ClaimState::Completed
        );
        assert_eq!(
            record.metadata["latest_promotion"]["provenance"]["replacement_gate_proof"]
                ["original_history"]["status"],
            "gate_failed"
        );
    });
}

#[test]
fn interrupted_replacement_gate_fence_requires_external_proof_without_rerunning() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-replacement-fence";
        let run_id = "run-replacement-fence";
        let target = tempfile::tempdir().expect("target");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link attempt");

        let mut failed = promotion_with_existing_path(run_id, target.path());
        failed.status = AgentTaskPromotionStatus::GateFailed;
        failed.deterministic_gates[0].status = crate::agent_task_gate::AgentTaskGateStatus::Failed;
        failed.deterministic_gates[0].exit_code = 1;
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(failed).unwrap())
            .expect("record failed promotion");
        let lifecycle_store = AgentTaskLifecycleStore::from_current_environment().expect("store");
        mark_replacement_gate_execution_started(&lifecycle_store, run_id)
            .expect("persist start fence");

        let gate_log = target.path().join("must-not-run");
        let error = verify_replacement_gates(
            cook_id,
            VerifyGateOptions {
                verify: vec![format!("printf ran > {}", gate_log.display())],
                ..Default::default()
            },
            "Chris approved corrected gate evidence".to_string(),
        )
        .expect_err("interrupted execution must fail closed");

        assert!(error.message.contains("will not rerun shell gates"));
        assert_eq!(
            error.details["recovery"]["kind"],
            "external_candidate_bound_proof_required"
        );
        assert!(!gate_log.exists());
        assert!(
            agent_task_lifecycle::operation_claim(
                run_id,
                "verify-replacement:run-replacement-fence"
            )
            .expect("read claim")
            .expect("claim retained")
            .state
                == agent_task_lifecycle::ClaimState::Failed
        );
    });
}

#[test]
fn cook_continuation_authenticates_only_its_exact_tracked_promotion_candidate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let fixture = durable_cook_0_328_fixture();
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
            &fixture.cook.id,
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
                mutation_timeout_ms: 30_000,
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy_core::defaults::save_config(&config).expect("save provider config");
        crate::agent_task_candidate_baseline::register();

        // Fanout coordinators re-resolve the destination from the identity
        // persisted when their child was admitted; they do not retain a CWD.
        options.source_worktree_path = None;
        options.initial_plan.metadata["cook_provision"] = serde_json::json!({
            "workspace_identity": homeboy_core::worktree_providers::resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
                &options.to_worktree,
                "fixture",
                &config,
            )
            .expect("persisted provider identity")
        });

        assert!(
            tracked_promotion_continuation(&options).unwrap().is_some(),
            "a durably attributed gate-failed promotion re-enters without a route token"
        );
        options.initial_plan.metadata["cook_continue_context"] = serde_json::json!({
            "schema": "homeboy/agent-task-cook-continue-context/v1",
            "run_id": options.initial_run_id,
        });
        let continuation = tracked_promotion_continuation(&options)
            .unwrap()
            .expect("tracked promotion continuation");
        assert_eq!(continuation.baseline["patch_artifact"]["id"], "patch");

        let generic_preflight =
            crate::agent_task_promotion::preflight_configured_workspace_provider(
                &options.to_worktree,
            )
            .expect_err("generic provider preflight rejects every dirty destination");
        assert_eq!(
            generic_preflight.details["workspace"]["classification"],
            "workspace.resolved_but_dirty"
        );
        validate_cook_workspace(&options)
            .expect("exact gate-failed promoted provider destination resumes");

        std::fs::write(target.join("extra.txt"), "unattributed\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("extra drift is rejected");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(
            error
                .message
                .contains("differs from its exact tracked post-apply candidate"),
            "{error}"
        );
        std::fs::remove_file(target.join("extra.txt")).unwrap();

        std::fs::write(target.join("tracked.txt"), "changed\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("changed drift is rejected");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error
            .message
            .contains("differs from its exact tracked post-apply candidate"));

        std::fs::write(target.join("tracked.txt"), "base\n").unwrap();
        let error = validate_cook_workspace(&options).expect_err("missing candidate is rejected");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error
            .message
            .contains("differs from its exact tracked post-apply candidate"));

        // The historical admission branch is narrower than the workspace
        // validator: it requires a terminal timed-out review-form retry whose
        // copied applied promotion exactly matches its source promotion.
        std::fs::write(target.join("tracked.txt"), "promoted\n").unwrap();
        let mut historical = options.clone();
        historical.initial_plan = batch_cook_options(
            &fixture.cook.id,
            Arc::new(AcceptedDetachedAttemptDispatcher),
        )
        .initial_plan;
        historical.initial_run_id = "run-historical-review-form".to_string();
        historical.initial_plan.tasks[0].inputs = serde_json::json!({
            "cook_loop": {
                "review_form_required": true,
                "execution_budget_authority": {
                    "kind": "fresh_cook_review",
                    "max_same_provider_retries": 1
                }
            }
        });
        historical.no_finalize = false;
        let mut source_options = historical.clone();
        source_options.initial_run_id = "run-tracked-promotion-source".to_string();
        record_tracked_promotion_continuation(&source_options, &target);
        let mut source_promotion = serde_json::to_value(
            persisted_promotion_for_attempt(&source_options.initial_run_id)
                .unwrap()
                .expect("source promotion"),
        )
        .unwrap();
        source_promotion["status"] = serde_json::json!("applied");
        source_promotion["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("post_apply");
        agent_task_lifecycle::record_promotion(&source_options.initial_run_id, source_promotion)
            .unwrap();

        let mut copied_promotion = serde_json::to_value(
            persisted_promotion_for_attempt(&source_options.initial_run_id)
                .unwrap()
                .expect("applied source promotion"),
        )
        .unwrap();
        copied_promotion["source"]["run_id"] = serde_json::json!(historical.initial_run_id);
        copied_promotion["provenance"]["cook_follow_up"] = serde_json::json!({
            "kind": fixture.continuation_record.latest_promotion.follow_up_kind,
            "source_run_id": source_options.initial_run_id,
        });
        agent_task_lifecycle::submit_plan(
            &historical.initial_plan,
            Some(&historical.initial_run_id),
        )
        .unwrap();
        agent_task_lifecycle::record_promotion(
            &historical.initial_run_id,
            copied_promotion.clone(),
        )
        .unwrap();
        seed_timeout_review_form_aggregate(&historical.initial_run_id, &historical.initial_plan);
        agent_task_lifecycle::rewrite_record_for_test(&historical.initial_run_id, |record| {
            record.state = agent_task_lifecycle::AgentTaskRunState::PartialFailure;
        })
        .unwrap();

        let source = persisted_promotion_for_attempt(&source_options.initial_run_id)
            .unwrap()
            .expect("fixture source promotion is inspectable");
        let continuation = persisted_promotion_for_attempt(&historical.initial_run_id)
            .unwrap()
            .expect("fixture continuation promotion is inspectable");
        assert_eq!(
            serde_json::to_value(source.status).unwrap(),
            fixture.source_record.latest_promotion.status
        );
        assert_eq!(fixture.source_record.schema, "homeboy/agent-task-run/v1");
        assert_eq!(fixture.source_record.state, "succeeded");
        assert!(!fixture.source_record.latest_promotion.post_apply);
        assert_eq!(source.provenance.pointer("/post_apply"), None);
        assert_eq!(
            continuation.provenance["cook_follow_up"]["kind"],
            fixture.continuation_record.latest_promotion.follow_up_kind
        );
        assert_eq!(fixture.cook.source_attempt, 1);
        assert_eq!(fixture.cook.continuation_attempt, 2);
        let aggregate = agent_task_lifecycle::read_aggregate(&historical.initial_run_id)
            .expect("fixture continuation aggregate is inspectable");
        assert_eq!(
            serde_json::to_value(aggregate.status).unwrap(),
            fixture.continuation_record.aggregate.status
        );
        assert_eq!(
            serde_json::to_value(aggregate.outcomes[0].status).unwrap(),
            fixture.continuation_record.aggregate.outcome_status
        );
        assert_eq!(
            fixture.continuation_record.schema,
            "homeboy/agent-task-run/v1"
        );
        assert_eq!(fixture.continuation_record.state, "partial_failure");

        let before_preflight =
            serde_json::to_value(agent_task_lifecycle::status(&historical.initial_run_id).unwrap())
                .unwrap();
        assert!(
            authenticated_historical_review_form_workspace_with_trace(&historical, false).unwrap(),
            "the exact dirty candidate authorizes only this historical continuation"
        );
        assert_eq!(
            serde_json::to_value(agent_task_lifecycle::status(&historical.initial_run_id).unwrap())
                .unwrap(),
            before_preflight,
            "read-only admission must not persist a continuation trace"
        );
        assert!(
            authenticated_historical_review_form_workspace(&historical).unwrap(),
            "execution records its exact admission trace"
        );
        historical.attempt_dispatcher = None;
        let executions = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(RecordingReviewFormExecutor {
            executions: Arc::clone(&executions),
        });

        std::fs::write(target.join("tracked.txt"), "drifted\n").unwrap();
        assert!(
            !authenticated_historical_review_form_workspace(&historical).unwrap(),
            "candidate drift falls through to normal preflight"
        );
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                Ok(serde_json::json!({}))
            }))),
            allow_historical_terminal: true,
            ..CookContext::new(historical.clone(), executor.clone())
        })
        .expect("candidate drift returns durable failure evidence before local dispatch");
        assert_eq!(result.exit_code, 1);
        assert!(result.value.disposition.is_terminal());
        let trace = agent_task_lifecycle::status(&historical.initial_run_id)
            .unwrap()
            .metadata["cook_continuation_admission"]
            .clone();
        assert_eq!(
            trace["first_authoritative_denial"],
            "provider_baseline_verification"
        );
        assert_eq!(trace["predicates"][4]["outcome"], "fail");
        assert_eq!(executions.load(Ordering::SeqCst), 0);

        std::fs::write(target.join("tracked.txt"), "promoted\n").unwrap();
        agent_task_lifecycle::rewrite_record_for_test(&historical.initial_run_id, |record| {
            record.state = agent_task_lifecycle::AgentTaskRunState::Cancelled;
        })
        .unwrap();
        assert!(
            !authenticated_historical_review_form_workspace(&historical).unwrap(),
            "cancelled review-form attempts never authorize the bypass"
        );
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                Ok(serde_json::json!({}))
            }))),
            allow_historical_terminal: true,
            ..CookContext::new(historical.clone(), executor.clone())
        })
        .expect("cancelled attempt returns durable failure evidence before local dispatch");
        assert_eq!(result.exit_code, 1);
        assert!(result.value.disposition.is_terminal());
        let trace = agent_task_lifecycle::status(&historical.initial_run_id)
            .unwrap()
            .metadata["cook_continuation_admission"]
            .clone();
        assert_eq!(
            trace["first_authoritative_denial"],
            "terminal_review_form_eligibility"
        );
        assert_eq!(trace["predicates"][1]["outcome"], "fail");
        assert_eq!(executions.load(Ordering::SeqCst), 0);

        agent_task_lifecycle::rewrite_record_for_test(&historical.initial_run_id, |record| {
            record.state = agent_task_lifecycle::AgentTaskRunState::PartialFailure;
        })
        .unwrap();
        agent_task_lifecycle::rewrite_record_for_test(&historical.initial_run_id, |record| {
            record.metadata["latest_promotion"]["provenance"]
                .as_object_mut()
                .unwrap()
                .remove("candidate");
        })
        .unwrap();
        assert!(
            !authenticated_historical_review_form_workspace(&historical).unwrap(),
            "missing legacy candidate evidence is an authorization denial"
        );
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(|_, _, _, _| {
                Ok(serde_json::json!({}))
            }))),
            allow_historical_terminal: true,
            ..CookContext::new(historical.clone(), executor.clone())
        })
        .expect("malformed evidence returns durable failure evidence before local dispatch");
        assert_eq!(result.exit_code, 1);
        assert!(result.value.disposition.is_terminal());
        let trace = agent_task_lifecycle::status(&historical.initial_run_id)
            .unwrap()
            .metadata["cook_continuation_admission"]
            .clone();
        assert_eq!(
            trace["first_authoritative_denial"],
            "terminal_review_form_eligibility"
        );
        assert_eq!(trace["predicates"][1]["outcome"], "fail");
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        std::fs::write(target.join("tracked.txt"), "promoted\n").unwrap();
        let mut other_cook = tracked_promotion_continuation_options(
            "cook-other-promotion",
            "run-other-promotion",
            &target,
        );
        other_cook.to_worktree = options.to_worktree.clone();
        other_cook.initial_plan.metadata["cook_continue_context"] = serde_json::json!({
            "schema": "homeboy/agent-task-cook-continue-context/v1",
            "run_id": other_cook.initial_run_id,
        });
        assert!(
            tracked_promotion_continuation(&other_cook)
                .unwrap()
                .is_none(),
            "a different Cook cannot claim this attempt's promotion"
        );
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
            result: crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed,
        });
    accepted.normalize_gate_outcome();

    assert_eq!(
        accepted.status,
        crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
    );
    // #11460 preserves the inherited-failure truth instead of flattening it to
    // Failed; the sibling assertion in this file was updated there, this one was
    // missed.
    assert_eq!(
        accepted.gate_results[0].status,
        homeboy_core::gate::HomeboyGateStatus::AcceptedInheritedFailure
    );
    assert!(!accepted.has_visible_passed_gate_for_command(command));

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
fn closed_observer_pipe_does_not_stop_explicitly_accepted_inherited_gate_finalization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("repository");
        let root = temp.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Homeboy Test"],
            vec!["config", "user.email", "test@example.test"],
        ] {
            git_output(root, &args).expect("git setup");
        }
        std::fs::write(root.join("failure"), "inherited\n").unwrap();
        std::fs::write(root.join("candidate"), "base\n").unwrap();
        git_output(root, &["add", "."]).unwrap();
        git_output(root, &["commit", "-m", "base"]).unwrap();
        let base = git_output(root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("candidate"), "provider-produced\n").unwrap();
        git_output(root, &["add", "candidate"]).unwrap();
        git_output(root, &["commit", "-m", "provider candidate"]).unwrap();

        let cook_id = "cook-normal-inherited";
        let run_id = format!("{cook_id}-run");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.clone();
        options.no_finalize = false;
        options.to_worktree = root.display().to_string();
        options.source_worktree_path = Some(root.to_path_buf());
        options.max_attempts = 1;
        options.gates.accept_inherited_failures = true;
        options.gates.verify = vec!["cat failure >&2; exit 1".to_string()];
        options.initial_plan.options.execution_budget =
            crate::agent_task_scheduler::AgentTaskExecutionBudget::new(1, 0, 0);
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &run_id).unwrap();
        seed_review_form_aggregate(&run_id, &options.initial_plan);
        agent_task_lifecycle::rewrite_record_for_test(&run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        let gate = crate::agent_task_gate::AgentTaskGateReport::new(
            "verify-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                options.gates.verify[0].clone(),
            ],
            1,
            "",
            "inherited\n",
            None,
            homeboy_core::gate::HomeboyGateVisibility::Visible,
            crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence,
            crate::agent_task_gate::AgentTaskGateEnvironment::default(),
        );
        let promotion: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "gate_failed",
            "source": {"kind": "aggregate", "task_id": "provider", "run_id": run_id},
            "to_worktree": root,
            "target": {"worktree": root, "path": root},
            "patch_artifact": {"id": "provider-patch", "kind": "patch", "path": "provider.patch"},
            "changed_files": ["candidate"],
            "deterministic_gates": [gate],
            "verified_base": {"base": "main", "sha": base},
            "provenance": {"worktree_path": root},
            "operator_notification": {"status": "blocked", "message": "gate failed"}
        }))
        .unwrap();
        agent_task_lifecycle::record_promotion(&run_id, serde_json::to_value(promotion).unwrap())
            .unwrap();
        let finalized = Arc::new(AtomicUsize::new(0));
        let finalization_count = Arc::clone(&finalized);
        let expected_base = base.clone();
        let expected_run_id = run_id.clone();
        let observer_calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&observer_calls);
        let observer = move |event: &CookProgressEvent<'_>| {
            observed.fetch_add(1, Ordering::SeqCst);
            if event.phase == "promotion" {
                return Err(Error::internal_io(
                    "Broken pipe (os error 32)",
                    Some("write submitting client stdout".to_string()),
                ));
            }
            Ok(())
        };
        let result = run_cook(CookContext {
            side_effects: Some(Box::new(DefaultCookSideEffects::new(
                move |_, _, received_run, promotion| {
                    finalization_count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(received_run, expected_run_id);
                    assert_eq!(promotion.verified_base.as_ref().unwrap().sha, expected_base);
                    assert_eq!(
                        promotion.deterministic_gates[0]
                            .baseline_comparison
                            .as_ref()
                            .unwrap()
                            .result,
                        crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed
                    );
                    Ok(serde_json::json!({"status": "review_ready"}))
                },
            ))),
            durable_observer: Some(&observer),
            ..CookContext::new(options, Arc::new(UnusedExecutor))
        })
        .unwrap();
        assert_eq!(result.value.status, "review_ready", "{:#?}", result.value);
        assert!(result
            .value
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("accepted inherited baseline-red")));
        assert_eq!(finalized.load(Ordering::SeqCst), 1);
        assert!(observer_calls.load(Ordering::SeqCst) >= 1);
        let record = agent_task_lifecycle::status(&run_id).unwrap();
        assert_eq!(record.metadata["provider_executions_consumed"], 1);
        assert_eq!(
            record.metadata["cook_observer_events"][0]["kind"],
            "delivery_failed"
        );
        assert_eq!(
            record.metadata["cook_observer_events"][0]["phase"],
            "promotion"
        );
        assert_eq!(
            record.metadata["cook_progress"]["terminal_success"],
            serde_json::json!(true),
            "the disconnected observer can reconnect to the terminal durable result"
        );
        let persisted = persisted_promotion_for_attempt(&run_id).unwrap().unwrap();
        assert_eq!(persisted.status, AgentTaskPromotionStatus::GateFailed);
        assert_eq!(persisted.deterministic_gates[0].exit_code, 1);
        assert_eq!(
            persisted.deterministic_gates[0].status,
            crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
        );
        assert_eq!(
            persisted.gate_results[0].status,
            homeboy_core::gate::HomeboyGateStatus::AcceptedInheritedFailure
        );
        assert_eq!(
            persisted.deterministic_gates[0]
                .baseline_comparison
                .as_ref()
                .unwrap()
                .base_ref,
            base
        );
    });
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
        draft_pr: false,
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
fn promotion_claim_and_replay_isolate_identical_ids_across_lifecycle_stores() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-cook-promote-claim";
    let run_id = "same-run-promote-claim";
    let options = promotion_claim_options(cook_id, run_id);
    let operation_key = promotion_operation_key(run_id);

    for (store, artifact_id) in [(&left_store, "left-patch"), (&right_store, "right-patch")] {
        store
            .submit_plan_with_runtime_admission(
                &AgentTaskPlan::new(cook_id, Vec::new()),
                run_id,
                |_| Ok(serde_json::json!({})),
            )
            .unwrap();
        let mut rooted_promotion = promotion(run_id);
        rooted_promotion.patch_artifact.id = artifact_id.to_string();
        store
            .record_promotion(run_id, serde_json::to_value(rooted_promotion).unwrap())
            .unwrap();

        let mut side_effects = DefaultCookSideEffects::new(|_, _, _, _| {
            unreachable!("promotion isolation does not finalize")
        });
        let first = side_effects.promote(store, &options, run_id).unwrap();
        let replayed = side_effects.promote(store, &options, run_id).unwrap();
        assert_eq!(first.patch_artifact.id, artifact_id);
        assert_eq!(replayed.patch_artifact.id, first.patch_artifact.id);
        assert_eq!(
            store
                .operation_claim(run_id, &operation_key)
                .unwrap()
                .expect("rooted promotion claim")
                .state,
            agent_task_lifecycle::ClaimState::Completed
        );
    }

    assert_ne!(left_store.run_dir(run_id), right_store.run_dir(run_id));
}

#[test]
fn retry_dispatch_operation_key_claim_dispatches_once() {
    // #8357: the detached retry-dispatch path reserves a durable claim keyed by
    // the retry run id before the handoff and completes it after. A resumed pass
    // (or a concurrent one) observes the completed claim / held lease and must
    // not send a second handoff. This exercises that exactly-once contract at the
    // claim boundary without the full git-backed cook loop.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-dispatch-claim";
    let next_run_id = "run-dispatch-claim-attempt-2";
    let plan = AgentTaskPlan::new(cook_id, Vec::new());
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, next_run_id, |_| Ok(serde_json::json!({})))
        .unwrap();

    let operation_key = retry_dispatch_operation_key(next_run_id);
    let lease = std::time::Duration::from_secs(60);

    // First pass acquires the claim → performs the (modeled) dispatch → completes.
    assert_eq!(
        lifecycle_store
            .claim_cook_operation(next_run_id, &operation_key, lease)
            .unwrap(),
        agent_task_lifecycle::ClaimOutcome::Acquired
    );
    lifecycle_store
        .complete_cook_operation(
            next_run_id,
            &operation_key,
            serde_json::json!({ "dispatched_run_id": next_run_id }),
        )
        .unwrap();

    // A resumed pass observes AlreadyCompleted and must not re-dispatch.
    match lifecycle_store
        .claim_cook_operation(next_run_id, &operation_key, lease)
        .unwrap()
    {
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            assert_eq!(result["dispatched_run_id"], next_run_id);
        }
        other => panic!("expected AlreadyCompleted, got {other:?}"),
    }
}

#[test]
fn cook_follow_up_store_boundary_accepts_local_execution_and_rejects_split_roots() {
    let first = homeboy_core::test_support::HermeticTestContext::new();
    let second = homeboy_core::test_support::HermeticTestContext::new();
    let recipe_store = CookRecipeStore::new(first.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(first.path_roots());
    let other_lifecycle_store = AgentTaskLifecycleStore::new(second.path_roots());

    validate_cook_follow_up_stores(&recipe_store, &lifecycle_store).unwrap();

    let split_root_error =
        validate_cook_follow_up_stores(&recipe_store, &other_lifecycle_store).unwrap_err();
    assert!(split_root_error
        .to_string()
        .contains("recipe and lifecycle stores must share one data root"));
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
fn finalization_claims_isolate_identical_run_ids_across_lifecycle_stores() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-cook-finalize-claim";
    let run_id = "same-run-finalize-claim";
    let plan = AgentTaskPlan::new(cook_id, Vec::new());
    let options = promotion_claim_options(cook_id, run_id);
    let promotion = promotion(run_id);

    for (store, root) in [(&left_store, "left"), (&right_store, "right")] {
        store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(serde_json::json!({})))
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let mut finalize =
            move |_: &AgentTaskCookServiceOptions, _: &str, _: &AgentTaskPromotionReport| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "root": root }))
            };

        for _ in 0..2 {
            let result = finalize_with_operation_claim_in_store(
                store,
                &options,
                run_id,
                &promotion,
                &mut finalize,
            )
            .unwrap();
            assert_eq!(result["root"], root);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            store
                .operation_claim(run_id, &finalization_operation_key(run_id, &promotion))
                .unwrap()
                .expect("rooted finalization claim")
                .state,
            agent_task_lifecycle::ClaimState::Completed
        );
    }

    assert_ne!(left_store.run_dir(run_id), right_store.run_dir(run_id));
}

#[test]
fn review_form_follow_up_finalization_replays_its_durable_claim_after_restart() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let cook_id = "cook-review-form-restart";
    let run_id = "cook-review-form-restart-attempt-2";
    let plan = AgentTaskPlan::new(cook_id, Vec::new());
    submit_plan_in_test_store(&lifecycle_store, &plan, Some(run_id)).unwrap();
    agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle_store, cook_id, 2, run_id)
        .unwrap();
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
        finalize_with_operation_claim_in_store(
            &lifecycle_store,
            &options,
            run_id,
            &promotion,
            &mut finalize,
        )
        .unwrap();
    }

    let operation_key = finalization_operation_key(run_id, &promotion);
    let claim =
        agent_task_lifecycle::operation_claim_in_store(&lifecycle_store, run_id, &operation_key)
            .unwrap()
            .expect("review-form finalization claim");
    assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
    assert_eq!(claim.result.unwrap()["review_form"], true);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "restart only revalidates publication"
    );
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
        let lifecycle_store = AgentTaskLifecycleStore::from_current_environment().unwrap();
        for _ in 0..3 {
            let calls = Arc::clone(&finalize_calls);
            let mut side_effects = DefaultCookSideEffects::new(
                move |_: &AgentTaskLifecycleStore,
                      _: &AgentTaskCookServiceOptions,
                      rid: &str,
                      _: &AgentTaskPromotionReport| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!({"status": "review_ready", "run_id": rid}))
                },
            );
            let promotion = side_effects
                .promote(&lifecycle_store, &options, run_id)
                .unwrap();
            let finalization = side_effects
                .finalize(&lifecycle_store, &options, run_id, &promotion)
                .unwrap();
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
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let next_run_id = "same-concurrent-dispatch-attempt-2";
    for (store, plan_id) in [(&left_store, "left-plan"), (&right_store, "right-plan")] {
        store
            .submit_plan_with_runtime_admission(
                &AgentTaskPlan::new(plan_id, Vec::new()),
                next_run_id,
                |_| Ok(serde_json::json!({})),
            )
            .unwrap();
    }
    let operation_key = retry_dispatch_operation_key(next_run_id);
    let lease = std::time::Duration::from_secs(300);
    let left_acquired = Arc::new(AtomicUsize::new(0));
    let right_acquired = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();

    for (store, acquired) in [
        (left_store.clone(), Arc::clone(&left_acquired)),
        (right_store.clone(), Arc::clone(&right_acquired)),
    ] {
        for _ in 0..2 {
            let store = store.clone();
            let key = operation_key.clone();
            let acquired = Arc::clone(&acquired);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if let agent_task_lifecycle::ClaimOutcome::Acquired = store
                    .claim_cook_operation(next_run_id, &key, lease)
                    .unwrap()
                {
                    acquired.fetch_add(1, Ordering::SeqCst);
                    store
                        .complete_cook_operation(
                            next_run_id,
                            &key,
                            serde_json::json!({ "dispatched_run_id": next_run_id }),
                        )
                        .unwrap();
                }
            }));
        }
    }
    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(left_acquired.load(Ordering::SeqCst), 1);
    assert_eq!(right_acquired.load(Ordering::SeqCst), 1);
    for store in [&left_store, &right_store] {
        let claim = store
            .operation_claim(next_run_id, &operation_key)
            .unwrap()
            .expect("dispatch claim recorded");
        assert_eq!(claim.state, agent_task_lifecycle::ClaimState::Completed);
    }
    assert_ne!(
        left_store.run_dir(next_run_id),
        right_store.run_dir(next_run_id)
    );
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
        let target = tempfile::tempdir().expect("fixture target");
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
            draft_pr: false,
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
        let promotion = promotion_with_existing_path(run_id, target.path());
        let mut backend = CaptureBackend {
            synthetic_gate_proof: Some(promotion.clone()),
            ..Default::default()
        };
        finalize_cook_pr_with_backend(&options, run_id, &promotion, &mut backend).unwrap();
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
fn cook_finalization_adopts_validated_review_form_used_for_when_option_is_empty() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-empty-used-for-attempt-1";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(
            "cook-empty-used-for",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_run_id = run_id.to_string();
        options.ai_used_for.clear();
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        let plan = options.initial_plan.clone();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, run_id).unwrap();
        seed_review_form_aggregate(run_id, &plan);
        let promotion = promotion_with_existing_path(run_id, target.path());

        let finalization = cook_finalization_options(&options, run_id, &promotion, Vec::new())
            .expect("finalization options");
        assert_eq!(finalization.ai_used_for, test_review_form().used_for);
        assert_eq!(
            finalization.review_dossier.ai_assistance.used_for,
            test_review_form().used_for
        );

        options.ai_used_for = "Operator-authored disclosure.".to_string();
        let finalization = cook_finalization_options(&options, run_id, &promotion, Vec::new())
            .expect("finalization options with override");
        assert_eq!(finalization.ai_used_for, "Operator-authored disclosure.");
        assert_eq!(
            finalization.review_dossier.ai_assistance.used_for,
            "Operator-authored disclosure."
        );
    });
}

#[test]
fn finalization_dossier_and_backend_hydration_use_explicit_lifecycle_store() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
    let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
    let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
    let cook_id = "same-cook-finalization-dossier";
    let run_id = "same-run-finalization-dossier";
    let target = tempfile::tempdir().expect("fixture target");

    let mut left_options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    left_options.initial_run_id = run_id.to_string();
    left_options.initial_plan.tasks[0].executor.model = Some("left-model".to_string());
    let mut right_options = left_options.clone();
    right_options.initial_plan.tasks[0].executor.model = Some("right-model".to_string());

    for (recipe_store, lifecycle_store, options, artifact_id) in [
        (
            &left_recipe_store,
            &left_lifecycle_store,
            &left_options,
            "left-patch",
        ),
        (
            &right_recipe_store,
            &right_lifecycle_store,
            &right_options,
            "right-patch",
        ),
    ] {
        recipe_store.persist_initial_recipe(options).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
                Ok(serde_json::json!({}))
            })
            .unwrap();
        lifecycle_store
            .record_cook_attempt(cook_id, 1, run_id)
            .unwrap();
        lifecycle_store
            .record_run_aggregate(
                run_id,
                &options.initial_plan,
                &review_form_aggregate(&options.initial_plan),
            )
            .unwrap();
        let mut rooted_promotion = promotion_with_existing_path(run_id, target.path());
        rooted_promotion.patch_artifact.id = artifact_id.to_string();
        lifecycle_store
            .record_promotion(run_id, serde_json::to_value(rooted_promotion).unwrap())
            .unwrap();
    }

    let promotion = promotion_with_existing_path(run_id, target.path());
    let left = cook_finalization_options_with_stores(
        &left_recipe_store,
        &left_lifecycle_store,
        &left_options,
        run_id,
        &promotion,
        Vec::new(),
    )
    .unwrap();
    let right = cook_finalization_options_with_stores(
        &right_recipe_store,
        &right_lifecycle_store,
        &right_options,
        run_id,
        &promotion,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(left.review_dossier.ai_assistance.model, "left-model");
    assert_eq!(right.review_dossier.ai_assistance.model, "right-model");

    let mut backend = RealAgentTaskPrFinalizationBackend;
    let left_proof = backend
        .hydrate_gate_proof_in_store(&left_lifecycle_store, run_id)
        .unwrap();
    let right_proof = backend
        .hydrate_gate_proof_in_store(&right_lifecycle_store, run_id)
        .unwrap();
    assert_eq!(left_proof.promotion.patch_artifact.id, "left-patch");
    assert_eq!(right_proof.promotion.patch_artifact.id, "right-patch");

    for (recipe_store, lifecycle_store, options) in [
        (&left_recipe_store, &left_lifecycle_store, &left_options),
        (&right_recipe_store, &right_lifecycle_store, &right_options),
    ] {
        let mut backend = CaptureBackend {
            synthetic_gate_proof: Some(promotion.clone()),
            ..Default::default()
        };
        let finalization = finalize_or_load_cook_pr_with_backend_with_stores(
            recipe_store,
            lifecycle_store,
            options,
            run_id,
            &promotion,
            &mut backend,
        )
        .unwrap();

        assert_eq!(finalization["status"], "review_ready");
        assert!(backend.created);
        assert!(lifecycle_store
            .read_record(run_id)
            .unwrap()
            .metadata
            .get("cook_finalization")
            .is_some());
    }
}

#[test]
fn cook_observer_failures_write_only_to_the_explicit_lifecycle_store() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(left_context.data_dir(), right_context.data_dir());

    let left = AgentTaskLifecycleStore::new(left_context.path_roots());
    let right = AgentTaskLifecycleStore::new(right_context.path_roots());
    let run_id = "same-cook-observer-run";
    let plan = AgentTaskPlan::new("observer-root-proof", Vec::new());

    for store in [&left, &right] {
        store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(serde_json::json!({})))
            .expect("seed the same run id in each lifecycle root");
    }
    let right_before = right.read_record(run_id).expect("right record before");
    let observer = |_: &CookProgressEvent<'_>| {
        Err(Error::internal_io(
            "Broken pipe (os error 32)",
            Some("write submitting client stdout".to_string()),
        ))
    };

    report_cook_progress_with_activity(
        &left,
        Some(&observer),
        "same-cook-observer",
        run_id,
        "promotion",
        1,
        None,
        None,
    )
    .expect("observer failure remains non-authoritative");

    let left_record = left.read_record(run_id).expect("left record after");
    assert_eq!(
        left_record.metadata["cook_observer_events"][0]["kind"],
        "delivery_failed"
    );
    assert_eq!(
        left_record.metadata["cook_observer_events"][0]["phase"],
        "promotion"
    );
    let right_after = right.read_record(run_id).expect("right record after");
    assert_eq!(right_after.metadata, right_before.metadata);
    assert_eq!(right_after.updated_at, right_before.updated_at);
}

#[test]
fn cook_promotion_finalizes_into_the_injected_stores_across_split_recipe_and_lifecycle_roots() {
    let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(recipe_context.data_dir(), lifecycle_context.data_dir());

    let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
    let lifecycle_store = AgentTaskLifecycleStore::new(lifecycle_context.path_roots());

    let cook_id = "split-root-finalization-cook";
    let run_id = "split-root-finalization-run";
    let target = tempfile::tempdir().expect("fixture target");
    let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
    options.initial_run_id = run_id.to_string();
    options.initial_plan.tasks[0].executor.model = Some("split-root-model".to_string());

    // The durable recipe lineage is promotion's recipe half; seed it only in the
    // recipe root.
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist the recipe in the recipe root");
    // The run record, the Cook index, and the provider aggregate are promotion's
    // lifecycle half; seed them only in the lifecycle root.
    lifecycle_store
        .submit_plan_with_runtime_admission(&options.initial_plan, run_id, |_| {
            Ok(serde_json::json!({}))
        })
        .expect("seed the run record in the lifecycle root");
    lifecycle_store
        .record_cook_attempt(cook_id, 1, run_id)
        .expect("record the cook attempt in the lifecycle root");
    lifecycle_store
        .record_run_aggregate(
            run_id,
            &options.initial_plan,
            &review_form_aggregate(&options.initial_plan),
        )
        .expect("record the review-form aggregate in the lifecycle root");

    let promotion = promotion_with_existing_path(run_id, target.path());
    // The read half of promotion resolves both stores explicitly: the recipe
    // lineage from the recipe root, the provider identity from the lifecycle
    // root's controller plan.
    let finalization_options = cook_finalization_options_with_stores(
        &recipe_store,
        &lifecycle_store,
        &options,
        run_id,
        &promotion,
        Vec::new(),
    )
    .expect("build finalization options through the injected split-root store pair");
    assert_eq!(
        finalization_options.review_dossier.ai_assistance.model,
        "split-root-model"
    );

    let mut backend = CaptureBackend {
        synthetic_gate_proof: Some(promotion.clone()),
        ..Default::default()
    };
    let finalization = finalize_or_load_cook_pr_with_backend_with_stores(
        &recipe_store,
        &lifecycle_store,
        &options,
        run_id,
        &promotion,
        &mut backend,
    )
    .expect("finalize through the injected split-root store pair");

    assert_eq!(finalization["status"], "review_ready");
    assert!(backend.created);
    // The lineage read reached the recipe root: finalization requires the
    // finalizing run to be declared by the persisted recipe, and only the
    // recipe root holds one.
    assert!(recipe_store.recipe_exists(cook_id));
    assert_eq!(
        recipe_store
            .load_recipe(cook_id)
            .expect("read the recipe in the recipe root")
            .attempts[0]
            .run_id,
        run_id
    );

    // The finalization receipt and the promotion record landed in the lifecycle
    // root, not in whatever root ambient state would have resolved.
    let record = lifecycle_store
        .read_record(run_id)
        .expect("read the run record in the lifecycle root");
    assert!(record.metadata.get("cook_finalization").is_some());
    assert!(record.metadata.get("promotions").is_some());
    assert!(lifecycle_store
        .run_dir(run_id)
        .starts_with(lifecycle_context.data_dir()));
    assert_eq!(
        lifecycle_store
            .read_cook_index(cook_id)
            .expect("read the cook index in the lifecycle root")
            .latest_run_id,
        run_id
    );

    // The negatives: a store built on the opposite root sees neither half, so
    // the injected pair — not an ambient root — decided every read and write.
    let recipe_root_lifecycle_store = AgentTaskLifecycleStore::new(recipe_context.path_roots());
    assert!(!recipe_root_lifecycle_store
        .record_exists(run_id)
        .expect("no run record in the recipe root"));
    assert!(!recipe_root_lifecycle_store.cook_index_exists(cook_id));
    let lifecycle_root_recipe_store = CookRecipeStore::new(lifecycle_context.path_roots());
    assert!(!lifecycle_root_recipe_store.recipe_exists(cook_id));
    let error = cook_finalization_options_with_stores(
        &lifecycle_root_recipe_store,
        &lifecycle_store,
        &options,
        run_id,
        &promotion,
        Vec::new(),
    )
    .expect_err("the lifecycle root holds no recipe lineage for this Cook");
    assert_eq!(error.code.as_str(), "internal.io_error");
    assert!(error.details["context"]
        .as_str()
        .expect("the IO error names the recipe path it read")
        .starts_with(
            lifecycle_context
                .data_dir()
                .to_str()
                .expect("utf-8 lifecycle data root")
        ));
}

#[test]
fn manual_finalization_identity_resolves_cook_and_failed_attempt_or_reserves_fresh_id() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-11334";
        let attempt_id = "cook-11334-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = attempt_id.to_string();
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(attempt_id))
            .expect("submit attempt");
        agent_task_lifecycle::record_pre_execution_failure(
            attempt_id,
            &options.initial_plan,
            "test",
            &homeboy_core::Error::invalid_argument("test", "failed Cook attempt"),
        )
        .expect("fail attempt");

        assert_eq!(
            prepare_manual_finalization_identity(cook_id).expect("Cook ID resolves"),
            attempt_id
        );
        assert_eq!(
            prepare_manual_finalization_identity(attempt_id).expect("failed attempt resolves"),
            attempt_id
        );

        let fresh_id = "manual-11334";
        assert_eq!(
            prepare_manual_finalization_identity(fresh_id).expect("fresh ID reserves a record"),
            fresh_id
        );
        let fresh = agent_task_lifecycle::status(fresh_id).expect("reserved finalization record");
        assert_eq!(fresh.metadata["manual_finalization_identity"], true);
        assert_eq!(
            prepare_manual_finalization_identity(fresh_id)
                .expect("reserved identity remains reusable"),
            fresh_id
        );
    });
}

#[test]
fn standalone_manual_preflight_continuation_recovers_and_is_idempotent() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "manual-11974";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(
            "cook-11974-fixture",
            Arc::new(AcceptedDetachedAttemptDispatcher),
        );
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
            ..Default::default()
        };
        // Reuse the Cook fixture builder to construct a valid dossier, then
        // remove its recipe before recovery so this exercises the standalone
        // durable manual-record route used by a fresh manual identity.
        persist_initial_recipe(&options).expect("persist fixture recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
            .expect("submit standalone manual record");
        agent_task_lifecycle::record_metadata_value(
            run_id,
            "manual_finalization_identity",
            serde_json::json!(true),
        )
        .expect("mark manual identity");
        seed_review_form_aggregate(run_id, &options.initial_plan);
        let promotion = promotion_with_existing_path(run_id, target.path());
        let mut finalization = cook_finalization_options(&options, run_id, &promotion, Vec::new())
            .expect("manual finalization options");
        finalization.manual_finalization = true;
        let candidate = crate::agent_task_finalization::AgentTaskPrCandidateState::Committed {
            changed_files: vec!["src/lib.rs".to_string()],
            push_required: false,
        };
        let preflight = crate::agent_task_finalization::preflight_pr_with_backend(
            finalization,
            &mut CaptureBackend {
                candidate_state: Some(candidate.clone()),
                ..Default::default()
            },
        )
        .expect("manual preflight");
        persist_manual_finalization_intent(run_id, &preflight).expect("persist validated intent");
        std::fs::remove_file(
            homeboy_core::paths::homeboy_data()
                .expect("homeboy data")
                .join("agent-task-cooks/cook-11974-fixture/recipe.json"),
        )
        .expect("remove fixture recipe before continuation");

        let continuation = format!("homeboy agent-task finalize-pr --recover {run_id}");
        assert_eq!(
            continuation,
            "homeboy agent-task finalize-pr --recover manual-11974"
        );
        let mut publish_backend = CaptureBackend {
            candidate_state: Some(candidate),
            ..Default::default()
        };
        let published =
            recover_cook_pr_with_backend(run_id, Vec::new(), false, &mut publish_backend)
                .expect("continuation resolves standalone validated intent");
        assert_eq!(published["status"], "review_ready");
        assert!(publish_backend.created);

        let mut repeated_backend = CaptureBackend::default();
        assert_eq!(
            recover_cook_pr_with_backend(run_id, Vec::new(), false, &mut repeated_backend)
                .expect("continuation is idempotent after publication"),
            published
        );
        assert!(!repeated_backend.created);
    });
}

#[test]
fn verified_existing_candidate_no_change_recovery_finalizes_once() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-12836-existing-candidate";
        let run_id = "cook-12836-existing-candidate-attempt-1";
        let target = tempfile::tempdir().expect("candidate repository");
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(target.path())
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(target.path().join("lib.rs"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(target.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::write(target.path().join("lib.rs"), "candidate\n").unwrap();
        assert!(Command::new("git")
            .args(["commit", "-am", "candidate"])
            .current_dir(target.path())
            .status()
            .unwrap()
            .success());

        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.to_worktree = target.path().display().to_string();
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("record Cook attempt");
        let patch = target.path().join("candidate.patch");
        seed_substantive_candidate_aggregate(run_id, &options.initial_plan, &patch, "candidate\n");
        let mut aggregate =
            agent_task_lifecycle::read_attempt_aggregate(run_id).expect("read aggregate");
        aggregate.outcomes[0].outputs = serde_json::json!({
            "review_form": test_review_form(),
            "provider_run_result": { "intentional_no_change": {
                "schema": "homeboy/agent-task-intentional-no-change/v1",
                "verdict": "already_satisfied",
                "inspected_revision": "candidate",
                "source_evidence": ["fixture"]
            }}
        });
        agent_task_lifecycle::record_run_aggregate(run_id, &options.initial_plan, &aggregate)
            .expect("persist intentional no-change aggregate");
        let mut promotion = promotion_with_existing_path(run_id, target.path());
        promotion.status = AgentTaskPromotionStatus::VerifiedNoChanges;
        promotion.changed_files = vec!["lib.rs".to_string()];
        promotion.verified_base = Some(
            crate::agent_task_promotion::AgentTaskPromotionVerifiedBase {
                base: options.base.clone(),
                sha: base,
            },
        );
        promotion.provenance["candidate"] = serde_json::to_value(
            crate::agent_task_promotion::candidate_fingerprint(target.path().to_str().unwrap())
                .expect("candidate fingerprint"),
        )
        .unwrap();
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&promotion).unwrap())
            .expect("persist verified existing candidate");

        let mut backend = CaptureBackend {
            candidate_state: Some(
                crate::agent_task_finalization::AgentTaskPrCandidateState::Committed {
                    changed_files: vec!["lib.rs".to_string()],
                    push_required: false,
                },
            ),
            synthetic_gate_proof: Some(promotion),
            ..Default::default()
        };
        let recovered = recover_cook_pr_with_backend(run_id, Vec::new(), false, &mut backend)
            .expect("recover publishes the verified existing candidate");
        assert_eq!(recovered["status"], "review_ready");
        assert!(backend.created);

        let mut repeated = CaptureBackend::default();
        assert_eq!(
            recover_cook_pr_with_backend(run_id, Vec::new(), false, &mut repeated)
                .expect("recovery is exactly once"),
            recovered
        );
        assert!(!repeated.created);
    });
}

#[test]
fn manual_preflight_intent_does_not_block_normal_cook_finalization() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-10980-normal";
        let run_id = "cook-10980-normal-attempt-1";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
            ..Default::default()
        };
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        seed_review_form_aggregate(run_id, &options.initial_plan);
        let promotion = promotion_with_existing_path(run_id, target.path());

        let mut manual = cook_finalization_options(&options, run_id, &promotion, Vec::new())
            .expect("manual options");
        manual.manual_finalization = true;
        let mut preflight_backend = CaptureBackend {
            candidate_state: Some(
                crate::agent_task_finalization::AgentTaskPrCandidateState::Committed {
                    changed_files: vec!["src/lib.rs".to_string()],
                    push_required: false,
                },
            ),
            ..Default::default()
        };
        let intent = crate::agent_task_finalization::preflight_pr_with_backend(
            manual,
            &mut preflight_backend,
        )
        .expect("manual preflight");
        persist_manual_finalization_intent(run_id, &intent).expect("persist intent");
        assert!(agent_task_lifecycle::status(run_id)
            .expect("status")
            .metadata["cook_finalization"]
            .is_null());

        let mut normal_backend = CaptureBackend {
            synthetic_gate_proof: Some(promotion.clone()),
            ..Default::default()
        };
        let receipt = finalize_or_load_cook_pr_with_backend(
            &options,
            run_id,
            &promotion,
            &mut normal_backend,
        )
        .expect("normal Cook finalization continues");
        assert_eq!(receipt["status"], "review_ready");
        assert!(normal_backend.created);

        let generic_receipt =
            serde_json::json!({ "status": "draft_published", "pr": { "number": 42 } });
        agent_task_lifecycle::record_cook_finalization(run_id, generic_receipt.clone())
            .expect("persist generic normal receipt");
        let mut recovery_backend = CaptureBackend::default();
        let recovered =
            recover_cook_pr_with_backend(cook_id, Vec::new(), false, &mut recovery_backend)
                .expect("draft finalization receipt takes precedence over stale manual intent");
        assert_eq!(recovered, generic_receipt);
        assert!(!recovery_backend.created);
    });
}

#[test]
fn manual_preflight_recovers_without_a_persisted_promotion_and_rejects_tampering() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-10980";
        let run_id = "cook-10980-attempt-1";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
            ..Default::default()
        };
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        seed_review_form_aggregate(run_id, &options.initial_plan);
        let promotion = promotion_with_existing_path(run_id, target.path());

        let mut finalization = cook_finalization_options(&options, run_id, &promotion, Vec::new())
            .expect("manual finalization options");
        finalization.manual_finalization = true;
        let clean_candidate =
            crate::agent_task_finalization::AgentTaskPrCandidateState::Committed {
                changed_files: vec!["src/lib.rs".to_string()],
                push_required: false,
            };
        let mut preflight_backend = CaptureBackend {
            candidate_state: Some(clean_candidate.clone()),
            ..Default::default()
        };
        let preflight = crate::agent_task_finalization::preflight_pr_with_backend(
            finalization,
            &mut preflight_backend,
        )
        .expect("manual preflight");
        assert_eq!(preflight.status, "validated");
        assert!(
            !preflight_backend.committed && !preflight_backend.pushed && !preflight_backend.created
        );
        let mut copied_from_another_run = preflight.clone();
        copied_from_another_run.run_id = "another-cook-attempt".to_string();
        let error = persist_manual_finalization_intent(run_id, &copied_from_another_run)
            .expect_err("a dossier copied from another run cannot be persisted");
        assert!(error.message.contains("different durable run"));
        assert!(agent_task_lifecycle::status(run_id)
            .expect("status")
            .metadata["manual_finalization_intent"]
            .is_null());

        let mut malformed = preflight.clone();
        malformed.status = "review_ready".to_string();
        let error = persist_manual_finalization_intent(run_id, &malformed)
            .expect_err("a non-preflight dossier cannot be persisted");
        assert!(error
            .message
            .contains("dossier failed integrity validation"));
        assert!(agent_task_lifecycle::status(run_id)
            .expect("status")
            .metadata["manual_finalization_intent"]
            .is_null());

        let preflight = serde_json::to_value(preflight).expect("serialize preflight");

        let mut coherently_tampered = preflight.clone();
        for pointer in [
            "/changed_files",
            "/publication_intent/changed_files",
            "/finalization_outcome/changed_files",
        ] {
            *coherently_tampered
                .pointer_mut(pointer)
                .expect("changed-files field") = serde_json::json!(["tampered.rs"]);
        }
        agent_task_lifecycle::record_manual_finalization_intent(run_id, coherently_tampered)
            .expect("persist coherent tampering");
        let error = recover_cook_pr_with_backend(
            cook_id,
            Vec::new(),
            false,
            &mut CaptureBackend {
                candidate_state: Some(clean_candidate.clone()),
                ..Default::default()
            },
        )
        .expect_err("candidate scope tampering fails closed");
        assert!(error.message.contains("changed files no longer match"));

        let preflight_report = serde_json::from_value(preflight.clone()).expect("parse preflight");
        persist_manual_finalization_intent(run_id, &preflight_report)
            .expect("persist manual dossier");
        let error = recover_cook_pr_with_backend(
            cook_id,
            Vec::new(),
            false,
            &mut CaptureBackend {
                candidate_state: Some(clean_candidate.clone()),
                committed_sha: Some("different-candidate-sha".to_string()),
                ..Default::default()
            },
        )
        .expect_err("candidate SHA mismatch fails closed");
        assert!(error.message.contains("no longer matches"));

        let mut publish_backend = CaptureBackend {
            candidate_state: Some(clean_candidate),
            ..Default::default()
        };
        let published =
            recover_cook_pr_with_backend(cook_id, Vec::new(), false, &mut publish_backend)
                .expect("recover manual publication");
        assert_eq!(published["status"], "review_ready");
        assert!(publish_backend.created);
        assert!(!publish_backend.committed && !publish_backend.pushed);
        let mut repeated_backend = CaptureBackend::default();
        let repeated =
            recover_cook_pr_with_backend(cook_id, Vec::new(), false, &mut repeated_backend)
                .expect("completed manual recovery is idempotent");
        assert_eq!(repeated, published);
        assert!(!repeated_backend.created);

        let mut different_candidate: AgentTaskPrFinalizationReport =
            serde_json::from_value(published.clone()).expect("parse receipt");
        different_candidate.changed_files = vec!["different.rs".to_string()];
        different_candidate.path = "/different-worktree".to_string();
        different_candidate.base = "different-base".to_string();
        different_candidate.head = "different-head".to_string();
        different_candidate.publication_intent.changed_files =
            different_candidate.changed_files.clone();
        different_candidate.publication_intent.target.path = Some(different_candidate.path.clone());
        different_candidate.publication_intent.target.base = Some(different_candidate.base.clone());
        different_candidate.publication_intent.target.head = Some(different_candidate.head.clone());
        different_candidate.publication_proof.target =
            different_candidate.publication_intent.target.clone();
        different_candidate.finalization_outcome.target =
            different_candidate.publication_intent.target.clone();
        different_candidate.publication_proof.target.url = different_candidate.pr_url.clone();
        different_candidate.finalization_outcome.target.url = different_candidate.pr_url.clone();
        different_candidate.finalization_outcome.base = different_candidate.base.clone();
        different_candidate.finalization_outcome.head = different_candidate.head.clone();
        different_candidate.finalization_outcome.changed_files =
            different_candidate.changed_files.clone();
        let binding = different_candidate
            .publication_proof
            .binding
            .as_mut()
            .expect("publication binding");
        binding.candidate_sha = "different-candidate-sha".to_string();
        binding.remote_sha = binding.candidate_sha.clone();
        binding.pr_head_sha = binding.candidate_sha.clone();
        binding.changed_files = different_candidate.changed_files.clone();
        different_candidate
            .publication_proof
            .git_identity
            .as_mut()
            .expect("Git identity")
            .commit_sha = Some("different-candidate-sha".to_string());
        let before = agent_task_lifecycle::status(run_id).expect("receipt before rejection");
        let error = persist_manual_finalization_receipt(run_id, &different_candidate)
            .expect_err("a self-consistent receipt for a different candidate cannot persist");
        assert!(error.message.contains("controller validation"));
        assert_eq!(
            agent_task_lifecycle::status(run_id)
                .expect("receipt after rejection")
                .metadata,
            before.metadata
        );

        for (pointer, replacement) in [
            ("/path", serde_json::json!("/tampered")),
            ("/base", serde_json::json!("tampered-base")),
            ("/head", serde_json::json!("tampered-head")),
            ("/proof", serde_json::json!({"tampered": true})),
            ("/review_dossier", serde_json::json!({"tampered": true})),
        ] {
            let mut tampered = published.clone();
            *tampered
                .pointer_mut(pointer)
                .expect("authoritative receipt field") = replacement;
            agent_task_lifecycle::record_cook_finalization(run_id, tampered)
                .expect("persist coherent receipt tampering");
            let error = recover_cook_pr_with_backend(
                run_id,
                Vec::new(),
                false,
                &mut CaptureBackend::default(),
            )
            .expect_err("receipt tampering fails closed");
            assert!(error.message.contains("finalization"));
        }

        let mut reassigned_receipt: AgentTaskPrFinalizationReport =
            serde_json::from_value(published.clone()).expect("parse receipt");
        reassigned_receipt.run_id = "another-cook-attempt".to_string();
        let error = persist_manual_finalization_receipt(run_id, &reassigned_receipt)
            .expect_err("a reassigned receipt cannot be persisted");
        assert!(error.message.contains("controller validation"));

        let mut malformed_receipt: AgentTaskPrFinalizationReport =
            serde_json::from_value(published.clone()).expect("parse receipt");
        malformed_receipt.pr_action = "none".to_string();
        let error = persist_manual_finalization_receipt(run_id, &malformed_receipt)
            .expect_err("a non-publication receipt cannot be persisted");
        assert!(error.message.contains("controller validation"));

        let published_report = serde_json::from_value(published.clone()).expect("parse receipt");
        persist_manual_finalization_receipt(run_id, &published_report)
            .expect("restore valid manual receipt");
        let mut tampered = published;
        tampered["changed_files"] = serde_json::json!(["tampered.rs"]);
        agent_task_lifecycle::record_cook_finalization(run_id, tampered)
            .expect("persist tampering");
        let error =
            recover_cook_pr_with_backend(run_id, Vec::new(), false, &mut CaptureBackend::default())
                .expect_err("tampered dossier fails closed");
        assert!(error.message.contains("integrity validation"));
    });
}

#[test]
fn recovery_hydrates_adopted_baseline_gate_evidence_and_can_preflight_without_mutation() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let target = tempfile::tempdir().expect("fixture target");
        let cook_id = "cook-9750";
        let run_id = "cook-9750-attempt-1";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
            // The fixture below marks the gate AcceptedInheritedFailure, which
            // is only a finalizable (and therefore recoverable) state when the
            // cook actually accepted inherited failures.
            accept_inherited_failures: true,
            ..Default::default()
        };
        persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        seed_patch_alias_aggregate(
            run_id,
            &options.initial_plan,
            &[(
                "patch",
                &target.path().join("candidate.patch"),
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            )],
        );
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
                result: crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed,
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
                .expect_err("recovery without an executed model fails closed");
        // #11460 stopped treating an accepted inherited failure as a passed
        // gate (asserted directly in
        // adopted_baseline_gate_outcome_is_candidate_bound_and_recovery_safe),
        // so recovery now fails closed before the executed-model check: an
        // inherited red baseline cannot back a published test claim.
        assert_eq!(preflight.details["field"], "verification");
        assert!(preflight
            .message
            .contains("without matching successful visible durable gate evidence"));
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
        let publish = recover_cook_pr_with_backend(
            run_id,
            vec![crate::agent_task_review_dossier::AgentTaskReviewOverride {
                target: crate::agent_task_review_dossier::AgentTaskReviewOverrideTarget::Summary,
                value: "Recovered from durable Cook evidence.".to_string(),
                provenance: "reviewed issue #9750".to_string(),
            }],
            false,
            &mut publish_backend,
        )
        .expect_err("publication without backing gate evidence fails closed");
        // Same reason as the preflight above: the accepted inherited failure is
        // not a passed gate, so the claim is refused before the model check.
        assert_eq!(publish.details["field"], "verification");
        assert!(!publish_backend.committed);
        assert!(!publish_backend.pushed);
        assert!(!publish_backend.created);
    });
}

#[test]
fn recovered_cook_finalization_uses_latest_resumed_gate_contract() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-8307";
        let run_id = "cook-8307-attempt-2";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        options.gates = VerifyGateOptions {
            verify: vec!["cargo test stale-original-contract".to_string()],
            private_verify: vec!["private stale gate".to_string()],
            ..Default::default()
        };
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link recipe attempt");
        seed_patch_alias_aggregate(
            run_id,
            &options.initial_plan,
            &[(
                "patch",
                &target.path().join("candidate.patch"),
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            )],
        );

        let mut resumed = promotion_with_existing_path(run_id, target.path());
        resumed.deterministic_gates[0].command = vec![
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test resumed-gate-contract".to_string(),
        ];
        resumed.gate_results[0].name = "cargo test resumed-gate-contract".to_string();
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&resumed).unwrap())
            .expect("record latest applied promotion");

        let report = recover_cook_pr_with_backend(
            cook_id,
            Vec::new(),
            true,
            &mut CaptureBackend {
                synthetic_gate_proof: Some(resumed),
                ..Default::default()
            },
        )
        .expect("recovered preflight");
        assert_eq!(
            report["review_dossier"]["how_to_test"][0]["command"],
            "cargo test resumed-gate-contract"
        );
        assert_eq!(
            report["verification"]["targeted_checks_run"],
            serde_json::json!(["cargo test resumed-gate-contract"])
        );
        assert!(!report.to_string().contains("stale-original-contract"));
        assert!(!report.to_string().contains("private stale gate"));
    });
}

#[test]
fn replacement_gate_proof_recovers_failed_candidate_without_hiding_evidence_or_republishing() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-11290";
        let run_id = "cook-11290-attempt-1";
        let target = tempfile::tempdir().expect("fixture target");
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = run_id.to_string();
        options.head = Some("fix/8058".to_string());
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id)).expect("submit run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link recipe attempt");
        seed_patch_alias_aggregate(
            run_id,
            &options.initial_plan,
            &[(
                "patch",
                &target.path().join("candidate.patch"),
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            )],
        );

        let mut failed = promotion_with_existing_path(run_id, target.path());
        failed.status = crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed;
        failed.deterministic_gates[0].status = crate::agent_task_gate::AgentTaskGateStatus::Failed;
        failed.deterministic_gates[0].exit_code = 1;
        failed.deterministic_gates[0].stderr = "rustup infrastructure failure".to_string();
        failed.provenance["candidate"] = serde_json::json!({"kind": "git", "fingerprint": {"head": "candidate", "tree": "candidate-tree", "sha256": "candidate-sha"}});
        agent_task_lifecycle::record_promotion(run_id, serde_json::to_value(&failed).unwrap())
            .expect("record immutable failed evidence");

        let mut replacement = failed.clone();
        replacement.status = crate::agent_task_promotion::AgentTaskPromotionStatus::Applied;
        replacement.deterministic_gates[0].status =
            crate::agent_task_gate::AgentTaskGateStatus::Succeeded;
        replacement.deterministic_gates[0].exit_code = 0;
        replacement.deterministic_gates[0].stderr.clear();
        replacement.command_evidence = vec![
            crate::agent_task_promotion::AgentTaskPromotionCommandReport {
                command: vec![
                    "sh".to_string(),
                    "-lc".to_string(),
                    "cargo test --locked agent_task_promotion --lib".to_string(),
                ],
                exit_code: 0,
                stdout: "passed".to_string(),
                stderr: String::new(),
                capture: Default::default(),
            },
        ];
        let error = record_replacement_gate_proof(run_id, replacement.clone(), None)
            .expect_err("external proof requires operator authorization");
        assert!(error.message.contains("explicit operator authorization"));

        let mut drifted = replacement.clone();
        drifted.verified_base.as_mut().unwrap().sha = "other-base".to_string();
        let error = record_replacement_gate_proof(
            run_id,
            drifted,
            Some("Chris approved external proof".to_string()),
        )
        .expect_err("base drift is refused");
        assert!(error.message.contains("drifted"));

        let mut mismatched_evidence = replacement.clone();
        mismatched_evidence.command_evidence[0].command = vec![
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test unrelated".to_string(),
        ];
        let error = record_replacement_gate_proof(
            run_id,
            mismatched_evidence,
            Some("Chris approved external proof".to_string()),
        )
        .expect_err("each gate needs matching command evidence");
        assert!(error
            .message
            .contains("matching zero-exit command evidence"));

        let replacement_for_finalization = replacement.clone();
        let replacement_for_replay = replacement.clone();
        record_replacement_gate_proof(
            run_id,
            replacement,
            Some("Chris approved external proof".to_string()),
        )
        .expect("record bound green replacement proof");
        let replay = record_replacement_gate_proof(
            run_id,
            replacement_for_replay,
            Some("Chris approved external proof".to_string()),
        )
        .expect("identical proof replay is idempotent");
        assert_eq!(
            replay.status,
            crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
        );
        let record = agent_task_lifecycle::status(run_id).expect("read durable evidence");
        assert_eq!(record.metadata["promotions"].as_array().unwrap().len(), 2);
        let original_history = &record.metadata["promotions"][0];
        assert!(original_history["deterministic_gates"][0]["stderr"]
            .as_str()
            .unwrap()
            .contains("rustup infrastructure failure"));
        let original_reference = &record.metadata["latest_promotion"]["provenance"]
            ["replacement_gate_proof"]["original_history"];
        assert_eq!(original_reference["run_id"], run_id);
        assert_eq!(original_reference["metadata_key"], "promotions");
        assert_eq!(original_reference["index"], 0);
        assert_eq!(original_reference["status"], "gate_failed");
        assert_eq!(original_reference["deterministic_gate_count"], 1);
        assert_eq!(
            original_reference["sha256"],
            homeboy_engine_primitives::content_hash::sha256_hex(
                &homeboy_core::engine::canonical_json::canonical_json_bytes(original_history)
                    .expect("serialize original history")
            )
        );
        assert!(
            record.metadata["latest_promotion"]["provenance"]["replacement_gate_proof"]
                .get("original")
                .is_none()
        );
        assert_eq!(
            record.metadata["latest_promotion"]["provenance"]["replacement_gate_proof"]
                ["environment_policy"][0]["environment"]["mode"],
            "inherit"
        );

        let mut backend = CaptureBackend {
            synthetic_gate_proof: Some(replacement_for_finalization),
            ..Default::default()
        };
        let published = recover_cook_pr_with_backend(cook_id, Vec::new(), false, &mut backend)
            .expect("replacement proof finalizes existing candidate");
        assert_eq!(published["status"], "review_ready");
        assert!(backend.created);
        assert!(backend
            .body
            .contains("cargo test --locked agent_task_promotion --lib"));
        for evidence in [
            "Original infrastructure-invalid verification retained",
            "Replacement candidate-bound verification",
            "Explicit operator authorization for external replacement proof was recorded.",
        ] {
            assert!(
                backend.body.contains(evidence),
                "missing dossier evidence: {evidence}; body: {}",
                backend.body
            );
        }
        assert!(!backend.body.contains("Chris approved external proof"));
        let mut repeated = CaptureBackend::default();
        assert_eq!(
            recover_cook_pr_with_backend(cook_id, Vec::new(), false, &mut repeated).unwrap(),
            published
        );
        assert!(
            !repeated.created,
            "finalization receipt preserves exactly-once publication"
        );
    });
}

#[test]
fn cook_rejects_test_claim_without_matching_durable_gate() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let run_id = "cook-8058-mismatch";
    let plan = AgentTaskPlan::new("cook-8058", Vec::new());
    submit_plan_in_test_store(&lifecycle_store, &plan, Some(run_id)).unwrap();
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
        draft_pr: false,
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
    // Finalization eligibility is checked before the test-claim contract and
    // requires a non-empty gate set, so clearing the gates outright never
    // reaches the check this test names. Keep the gate green but private:
    // eligible to finalize, yet not visible evidence that can back a
    // published test claim.
    let mut unsupported = promotion(run_id);
    unsupported.deterministic_gates[0].visibility =
        homeboy_core::gate::HomeboyGateVisibility::Private;
    let error = finalize_cook_pr_with_backend_with_stores(
        &recipe_store,
        &lifecycle_store,
        &options,
        run_id,
        &unsupported,
        &mut CaptureBackend::default(),
    )
    .expect_err("unsupported test claim is rejected");
    assert!(
        error
            .message
            .contains("matching successful visible durable gate"),
        "{error}"
    );
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
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let patch_path = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("{cook_id}-candidate.patch"));
    std::fs::write(&patch_path, patch).expect("write terminal child candidate patch");
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
                artifacts: vec![crate::agent_task::AgentTaskArtifact {
                    id: "patch".to_string(),
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
        let result = resume_cook_batch(
            "batch-9525",
            Arc::new(UnusedExecutor),
            test_reconstruct_dispatcher,
        )
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
        let second = resume_cook_batch(
            "batch-9525",
            Arc::new(UnusedExecutor),
            test_reconstruct_dispatcher,
        )
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
        // Finalization authenticates the disclosure lineage against a concrete
        // executed model, and the seeded aggregate takes its model from the plan.
        options.initial_plan.tasks[0].executor.model = Some("fixture-model".to_string());
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
            Arc::new(UnusedExecutor),
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
            Arc::new(UnusedExecutor),
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
            Arc::new(UnusedExecutor),
            test_reconstruct_dispatcher,
        )
        .expect("resume returns a report even when a child cannot resume");

        assert_eq!(result.exit_code, 1);
        assert_eq!(result.value.status, "failed");
        assert_eq!(result.value.failed, 1);
        let cell = &result.value.cooks[0];
        assert_eq!(cell.exit_code, 1);
        // The structured envelope keeps the code an orchestrator routes on,
        // not just the prose it used to carry.
        let error = cell.error.as_ref().expect("unresumable child reports why");
        assert_eq!(error.code, "validation.invalid_argument");
        assert!(error.message.contains("no durable recipe"));
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

        let report = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "execution_budget_exhausted",
            disposition: CookDisposition::Terminal,
            attempts: vec![AgentTaskCookAttemptReport {
                attempt: 2,
                run_id: fresh_run_id.clone(),
                run_state: "Running".to_string(),
                aggregate_path: None,
                promotion: None,
                feedback: None,
            }],
            finalization: None,
            stop_reason: Some(
                "provider execution stopped because budget was exhausted".to_string(),
            ),
            exit_code: 1,
            invocation_latest_run_id: Some(&fresh_run_id),
        });

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
        assert!(!report.value.history_run_ids.contains(&fresh_run_id));
    });
}

/// #11114 (a): the controller-failure path passed `None` for the invocation run
/// id, so it reported whatever the cross-invocation Cook index happened to name
/// — and `cook_failure_context` stamped that same stale id into every recovery
/// command it emitted. The orchestrator was handed `status`/`diagnose` commands
/// for a prior session's run.
#[test]
fn durable_controller_failure_reports_this_invocation_run_not_the_stale_cook_index() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-11114-controller-failure";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).expect("persist durable recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("materialize this invocation's run");

        // A prior session left the cross-invocation Cook index pointing at its
        // own run. That is the value this path used to report.
        let stale_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, 1);
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&stale_run_id))
            .expect("materialize the prior session's run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &stale_run_id)
            .expect("index the prior session's run");
        assert_eq!(
            agent_task_lifecycle::cook_index(cook_id)
                .expect("cook index")
                .latest_run_id,
            stale_run_id,
            "fixture must leave the cross-invocation index on the prior session's run"
        );

        let report = durable_cook_error_report(
            &options,
            Error::internal_unexpected("controller failed after durable creation"),
        )
        .expect("controller failure converts into the Cook result contract");

        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(options.initial_run_id.as_str()),
            "a controller failure must report THIS invocation's run"
        );
        assert!(
            report
                .value
                .invocation_run_ids
                .contains(&options.initial_run_id),
            "invocation scope must not be empty just because no attempt report exists"
        );
        assert!(
            !report.value.invocation_run_ids.contains(&stale_run_id),
            "the prior session's run is history, not this invocation"
        );

        let context = report
            .value
            .failure_context
            .expect("durable failure context");
        assert_eq!(context.phase, "controller");
        assert_eq!(context.latest_run_id, options.initial_run_id);
        for action in context.legal_actions.iter().chain(&context.next_actions) {
            // Candidate-selection actions intentionally address the selected
            // cross-invocation candidate; the run-addressed diagnostics must not.
            if !matches!(action.action.as_str(), "status" | "diagnose" | "reconcile") {
                continue;
            }
            assert!(
                action.command.contains(&options.initial_run_id),
                "recovery command must address this invocation's run: {}",
                action.command
            );
            assert!(
                !action.command.contains(&stale_run_id),
                "recovery command must not address the prior session's run: {}",
                action.command
            );
        }
    });
}

#[test]
fn recovery_context_uses_current_gate_and_finalization_evidence_not_an_older_candidate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-recovery-phase-truth";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        let current_run_id = options.initial_run_id.clone();
        let older_run_id = "cook-recovery-phase-truth-older";
        let artifacts = tempfile::tempdir().expect("candidate artifacts");
        persist_initial_recipe(&options).expect("persist recipe");
        for (attempt, run_id) in [(1, older_run_id), (2, current_run_id.as_str())] {
            agent_task_lifecycle::submit_plan(&options.initial_plan, Some(run_id))
                .expect("persist lifecycle record");
            agent_task_lifecycle::record_cook_attempt(cook_id, attempt, run_id)
                .expect("persist Cook attempt");
        }

        // This older green candidate remains selectable across the Cook history.
        // It must not influence recovery for the current failed invocation.
        seed_substantive_candidate_aggregate(
            older_run_id,
            &options.initial_plan,
            &artifacts.path().join("older.patch"),
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+older\n",
        );
        agent_task_lifecycle::record_promotion(
            older_run_id,
            serde_json::to_value(promotion(older_run_id)).unwrap(),
        )
        .unwrap();
        let mut failed_gate = promotion(&current_run_id);
        failed_gate.status = AgentTaskPromotionStatus::GateFailed;
        agent_task_lifecycle::record_promotion(
            &current_run_id,
            serde_json::to_value(&failed_gate).unwrap(),
        )
        .unwrap();

        let gate_context = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "durable_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(&current_run_id),
        })
        .value
        .failure_context
        .expect("gate recovery context");
        assert_eq!(gate_context.phase, "deterministic_gate");
        assert_eq!(gate_context.reason_code, "gate_failed");
        assert_eq!(gate_context.selected_run_id.as_deref(), Some(older_run_id));
        assert!(gate_context.legal_actions.iter().all(|action| {
            action.command.contains(&current_run_id) && !action.command.contains(older_run_id)
        }));

        let applied = promotion(&current_run_id);
        agent_task_lifecycle::record_promotion(
            &current_run_id,
            serde_json::to_value(&applied).unwrap(),
        )
        .unwrap();
        let operation_key = format!("finalize:{current_run_id}");
        agent_task_lifecycle::claim_cook_operation(
            &current_run_id,
            &operation_key,
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        agent_task_lifecycle::fail_cook_operation(
            &current_run_id,
            &operation_key,
            serde_json::json!({ "code": "publication_rejected" }),
        )
        .unwrap();

        let finalization_context = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "durable_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(&current_run_id),
        })
        .value
        .failure_context
        .expect("finalization recovery context");
        assert_eq!(finalization_context.phase, "finalization");
        assert_eq!(finalization_context.reason_code, "publication_rejected");
        assert_eq!(
            finalization_context
                .diagnostic
                .expect("durable finalization reason")["code"],
            "publication_rejected"
        );
        assert!(finalization_context.legal_actions.iter().all(|action| {
            action.command.contains(&current_run_id) && !action.command.contains(older_run_id)
        }));
    });
}

#[test]
fn exact_checkpoint_destination_mismatch_projects_a_fork_replacement_response() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let cook_id = "cook-checkpoint-destination-mismatch";
        let options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        persist_initial_recipe(&options).expect("persist recipe");
        agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id))
            .expect("persist lifecycle record");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, &options.initial_run_id)
            .expect("index Cook attempt");
        let operation_key = format!("promote:{}", options.initial_run_id);
        agent_task_lifecycle::claim_cook_operation(
            &options.initial_run_id,
            &operation_key,
            std::time::Duration::from_secs(60),
        )
        .expect("claim promotion");
        agent_task_lifecycle::fail_cook_operation(
            &options.initial_run_id,
            &operation_key,
            serde_json::json!({
                "code": "ValidationInvalidArgument",
                "details": {
                    "field": "promotion",
                    "recovery": { "action": "fork_replacement" },
                },
                "deepest_cause": {
                    "code": "validation.invalid_argument",
                    "field": "promotion",
                    "message": "promotion resume target differs from the exact checkpointed applied candidate",
                },
            }),
        )
        .expect("persist exact checkpoint rejection");

        let context = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "durable_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(&options.initial_run_id),
        })
        .value
        .failure_context
        .expect("durable failure context");

        assert_eq!(context.phase, "promotion");
        assert!(context.legal_actions.iter().any(|action| {
            action.action == "fork_replacement"
                && action.command
                    == format!("homeboy agent-task retry {} --run", options.initial_run_id)
        }));
        assert!(context
            .legal_actions
            .iter()
            .chain(&context.next_actions)
            .all(|action| action.action != "resume" && !action.command.contains("cook-continue")));
    });
}

/// #11114: `select_cook_candidate` deliberately spans invocations, so a report
/// can name this invocation's `latest_run_id` while `selected_candidate` names a
/// prior attempt. `invocation_scoped` makes that difference legible instead of
/// leaving the orchestrator to guess.
#[test]
fn selected_candidate_provenance_flags_cross_invocation_selection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("candidate artifacts");
        let cook_id = "cook-11114-selection-scope";
        let selected_run_id = "cook-11114-selection-scope-1";
        let latest_run_id = "cook-11114-selection-scope-2";
        let mut options = batch_cook_options(cook_id, Arc::new(AcceptedDetachedAttemptDispatcher));
        options.initial_run_id = latest_run_id.to_string();
        persist_initial_recipe(&options).expect("persist current invocation recipe");
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
        let historical_recovery =
            agent_task_lifecycle::AgentTaskLabRuntimeRecovery::refresh_homeboy(
                "homeboy-lab",
                "homeboy 1.2.2+historical",
                "historical",
            );
        agent_task_lifecycle::rewrite_record_for_test(selected_run_id, |record| {
            record.metadata["pre_execution_failure"] = serde_json::json!({
                "phase": "lab_staging_controller",
                "details": { "lab_handoff_runtime_recovery": historical_recovery },
            });
        })
        .expect("persist historical recovery");
        let current_recovery = agent_task_lifecycle::AgentTaskLabRuntimeRecovery::refresh_homeboy(
            "homeboy-lab",
            "homeboy 1.2.3+current",
            "current",
        );
        agent_task_lifecycle::record_pre_execution_failure(
            latest_run_id,
            &options.initial_plan,
            "lab_staging_controller",
            &{
                let mut error = Error::validation_invalid_argument(
                    "runner",
                    "current Lab runtime is stale",
                    None,
                    None,
                );
                error.details["lab_handoff_runtime_recovery"] =
                    serde_json::to_value(current_recovery).expect("current recovery serializes");
                error
            },
        )
        .expect("persist current admission failure");

        let cross_invocation = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "pre_execution_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(latest_run_id),
        })
        .value
        .selected_candidate
        .expect("selected candidate provenance");
        assert_eq!(cross_invocation["run_id"], selected_run_id);
        assert_eq!(
            cross_invocation["invocation_scoped"],
            serde_json::json!(false),
            "a candidate selected from a prior attempt must be flagged as out of invocation scope"
        );
        let context = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "pre_execution_failure",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 1,
            invocation_latest_run_id: Some(latest_run_id),
        })
        .value
        .failure_context
        .expect("failure context");
        let runtime_actions: Vec<_> = context
            .legal_actions
            .iter()
            .filter(|action| action.action == "refresh_lab_runtime")
            .collect();
        assert_eq!(runtime_actions.len(), 1);
        assert_eq!(
            runtime_actions[0].command,
            "homeboy runner refresh-homeboy homeboy-lab --ref current --reconnect"
        );
        assert!(context
            .next_actions
            .iter()
            .all(|action| action.action != "refresh_lab_runtime"));

        let in_invocation = cook_report(CookReportInput {
            cook_id: cook_id.to_string(),
            status: "completed",
            disposition: CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: None,
            exit_code: 0,
            invocation_latest_run_id: Some(selected_run_id),
        })
        .value
        .selected_candidate
        .expect("selected candidate provenance");
        assert_eq!(
            in_invocation["invocation_scoped"],
            serde_json::json!(true),
            "a candidate this invocation produced must be flagged as invocation scoped"
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
            let report = cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status,
                disposition: CookDisposition::Terminal,
                attempts: Vec::new(),
                finalization: None,
                stop_reason: Some(
                    "private provider evidence remains in durable diagnostics".to_string(),
                ),
                exit_code: 1,
                invocation_latest_run_id: Some(&options.initial_run_id),
            });
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

/// Passes under `cargo nextest` (one process per test) and under
/// `cargo test -- --test-threads=1`. It can fail under multi-threaded
/// `cargo test`, at the `baseline_path.exists()` assertion.
///
/// That is a harness limitation, not a product defect. `HomeGuard` holds
/// `home_lock()` for its lifetime, which serializes *writers* — but as its own
/// doc states, readers never take it, including worker threads a test spawns.
/// This test materializes and then deliberately deletes a worktree, so a
/// concurrent test repointing the home root mid-flight can observe the path
/// after deletion and before re-materialization.
///
/// CI runs nextest, so it does not see this. Do not "fix" it by making the hot
/// path resolvers take the lock on every read.
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
        let report = cook_report(CookReportInput {
            cook_id: "cook-test".to_string(),
            status: "policy_failure",
            disposition: CookDisposition::Terminal,
            attempts: vec![AgentTaskCookAttemptReport {
                attempt: 1,
                run_id: fresh_run_id.clone(),
                run_state: "Succeeded".to_string(),
                aggregate_path: None,
                promotion: None,
                feedback: None,
            }],
            finalization: None,
            stop_reason: Some("policy failure".to_string()),
            exit_code: 1,
            invocation_latest_run_id: Some(&fresh_run_id),
        });

        assert_eq!(report.value.invocation_run_ids, vec![fresh_run_id.clone()]);
        assert_eq!(
            report.value.latest_run_id.as_deref(),
            Some(fresh_run_id.as_str())
        );
        assert_eq!(report.value.status, "policy_failure");
    });
}

#[test]
fn a_progress_event_carries_provider_activity_to_the_observer() {
    // The observer boundary used to drop everything but `(phase, cook_id,
    // run_id)`, which is why every heartbeat an operator saw was identical.
    // A heartbeat event now has to be able to say what the provider is doing
    // (#11482).
    let activity = CookProviderActivity {
        files_changed: Some(0),
        command: Some("cargo test -q -p homeboy-agents".to_string()),
        command_elapsed_seconds: Some(372),
        elapsed_seconds: Some(400),
        ..Default::default()
    };
    let event = CookProgressEvent {
        phase: "heartbeat",
        cook_id: "cook-1",
        run_id: "cook-1-attempt-1",
        attempt: 1,
        detail: Some("provider execution is still running"),
        activity: Some(&activity),
    };

    let summary = event
        .activity_summary()
        .expect("event renders its activity");

    assert!(summary.contains("no files written yet"));
    assert!(summary.contains("cargo test -q -p homeboy-agents"));
    assert_eq!(event.attempt, 1);
}

#[test]
fn a_progress_event_without_a_sample_renders_no_activity() {
    let event = CookProgressEvent {
        phase: "provider_start",
        cook_id: "cook-1",
        run_id: "cook-1-attempt-1",
        attempt: 1,
        detail: None,
        activity: None,
    };

    assert_eq!(event.activity_summary(), None);
}
