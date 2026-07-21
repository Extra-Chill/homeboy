//! Tests for the cook orchestration service (`super::cook`). Split from the
//! cook god file via #[path]; logically remains `cook::tests` so `super::`
//! paths are unchanged.

use super::super::cook_adoption::{
    adopt_cook_candidate, adopt_cook_candidate_with_dispatcher_and_backend,
    candidate_adoption_source, concrete_adoption_ai_model, resolve_adoption_target,
};
use super::super::cook_baseline::git_output;
use super::super::cook_promotion::{
    finalize_cook_pr_with_backend, moving_base_recovery_for_run,
    moving_base_recovery_from_promotion, moving_base_recovery_report, next_moving_base_recovery,
    persisted_promotion_for_attempt, recover_moving_base_cook_candidate,
    refreshed_moving_base_recovery, MovingBaseCookRecovery,
};
use super::*;
use crate::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
};
use crate::agent_task_finalization::{
    AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
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
use std::sync::{Barrier, Condvar};

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
                task_id: "provider".to_string(),
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
fn moving_base_recovery_report_retains_typed_evidence_and_exact_continuation() {
    let recovery = MovingBaseCookRecovery {
        schema: "homeboy/agent-task-cook-moving-base-recovery/v1".to_string(),
        cook_id: "cook-9267".to_string(),
        run_id: "run-9267".to_string(),
        promotion: promotion("run-9267"),
        prior_verified_base: "a".repeat(40),
        passed_gates: serde_json::json!([{"status": "passed"}]),
        blocker: "HEAD is behind or diverged from resolved base".to_string(),
        continuation: "homeboy agent-task run-next".to_string(),
        base_movements: 0,
    };
    let report = moving_base_recovery_report("cook-9267".to_string(), Vec::new(), recovery, true);

    assert_eq!(report.value.status, "candidate_recoverable");
    let recovery = report
        .value
        .moving_base_recovery
        .expect("typed recovery state");
    assert_eq!(recovery.run_id, "run-9267");
    assert_eq!(recovery.continuation, "homeboy agent-task run-next");
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
        let plan = AgentTaskPlan::new("moving-base-restart", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["provider_executions_consumed"] = serde_json::json!(1);
        })
        .unwrap();
        let recovery =
            moving_base_recovery_from_promotion("cook-restart", run_id, promotion(run_id));

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
    assert!(
        moving_base_recovery_report("cook-bound".to_string(), Vec::new(), divergent, false)
            .value
            .stop_reason
            .unwrap()
            .contains("exhausted")
    );

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
            move |_, _, promotion| {
                finalization_count_for_finalize.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    promotion.verified_base.as_ref().unwrap().sha,
                    "pinned-refreshed-base"
                );
                Ok(serde_json::json!({"status":"review_ready", "run_id": run_id}))
            },
            move |options, recovery| {
                rebase_count_for_recover.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    options.gates.private_gate_reveal,
                    crate::agent_task_gate::AgentTaskGateRevealPolicy::FullEvidence
                );
                let mut refreshed = recovery.promotion.clone();
                refreshed.verified_base.as_mut().unwrap().sha = "pinned-refreshed-base".to_string();
                refreshed.provenance["candidate"] =
                    serde_json::json!({"kind":"git","fingerprint":{"tree":"rebased"}});
                Ok(refreshed)
            },
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
            |_, _, _| panic!("failed rebased gates must not finalize"),
            |_, recovery| {
                let mut failed = recovery.promotion.clone();
                failed.status = AgentTaskPromotionStatus::GateFailed;
                Ok(failed)
            },
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
        std::fs::write(advance.join("base-advanced.txt"), "advanced\n").unwrap();
        git(&advance, &["add", "."]);
        git(&advance, &["commit", "-m", "advance base"]);
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
                "test -f base-advanced.txt && test \"$(cat src/lib.rs)\" = new".to_string(),
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

#[derive(Debug)]
struct BatchAttemptDispatcher {
    barrier: Arc<Barrier>,
    entered: Arc<AtomicUsize>,
    fail: bool,
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
    assert_eq!(result.exit_code, 1);
    assert_eq!(result.value.status, "failed");
    assert_eq!(result.value.total, 2);
    assert_eq!(result.value.succeeded, 1);
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
        options.gates.verify = vec!["test \"$(cat lib.rs)\" = candidate".to_string()];
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
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("link recipe attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("typed handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
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
                },
                |_| Ok(None),
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

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.value.status, "review_ready");
        assert_eq!(result.value.attempts.len(), 1);
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
        assert!(backend.body.contains("- **Tool(s):** test"));
        assert!(backend.body.contains("- **Model:** openai/gpt-5.6-sol"));
        assert!(backend.committed && backend.pushed && backend.created);
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

        let error = adopt_cook_candidate(cook_id, "candidate")
            .expect_err("cancelled run without recovery evidence is rejected");
        assert_eq!(error.code, homeboy_core::ErrorCode::ValidationInvalidJson);
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

        let (record, recipe) = resolve_adoption_target(cook_id)
            .expect("equivalent attempts resolve deterministically");

        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, first_run_id);
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

        let error = resolve_adoption_target(cook_id)
            .expect_err("conflicting recipe adoption requires an explicit run id");

        assert_eq!(error.details["field"], "cook_recipe.attempts");
        assert!(error.message.contains(first_run_id));
        assert!(error.message.contains(second_run_id));
        assert!(error
            .message
            .contains(&format!("homeboy agent-task adopt {first_run_id}")));

        let (record, recipe) = resolve_adoption_target(second_run_id)
            .expect("an exact orphaned attempt run id selects its recipe");
        assert_eq!(recipe.cook_id, cook_id);
        assert_eq!(record.run_id, second_run_id);
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
}

impl AgentTaskPrFinalizationBackend for CaptureBackend {
    fn hydrate_run(&mut self, _run_id: &str) -> Result<RunLifecycleRecord> {
        if let Some(run_id) = self.hydrate_run_id.as_deref() {
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
        if self.hydrate_run_id.is_some() {
            return RealAgentTaskPrFinalizationBackend.hydrate_gate_proof(run_id);
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
            scope: "repository_local".to_string(),
        })
    }
    fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
        self.committed = true;
        Ok(())
    }
    fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
        self.pushed = true;
        Ok(())
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
            "deterministic_gates": [{"id": "gate", "visibility": "visible", "reveal_policy": "full_evidence", "status": "succeeded", "command": ["sh", "-lc", "cargo test --locked agent_task_promotion --lib"], "exit_code": 0}],
            "gate_results": [{"id": "gate", "name": "cargo test --locked agent_task_promotion --lib", "kind": "command", "status": "passed"}],
            "operator_notification": {"status": "completed", "message": "complete"},
            "verified_base": {"base": "main", "sha": "verified-base"},
            "provenance": {"worktree_path": "/repo"}
        })).unwrap()
}

#[test]
fn restarted_cook_uses_only_its_exact_persisted_promotion() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
        agent_task_lifecycle::record_promotion(
            "run-persisted",
            serde_json::to_value(promotion("run-persisted")).unwrap(),
        )
        .unwrap();

        let restored = persisted_promotion_for_attempt("run-persisted")
            .unwrap()
            .expect("durable promotion");
        assert_eq!(restored.source.run_id.as_deref(), Some("run-persisted"));
        assert_eq!(restored.patch_artifact.id, "patch");
    });
}

#[test]
fn persisted_promotion_from_another_attempt_is_rejected() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
        agent_task_lifecycle::record_promotion(
            "run-persisted",
            serde_json::to_value(promotion("different-run")).unwrap(),
        )
        .unwrap();

        let error = persisted_promotion_for_attempt("run-persisted").unwrap_err();
        assert!(error.message.contains("does not belong to this attempt"));
    });
}

#[test]
fn cook_successful_concrete_attempt_publishes_reviewer_body() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "cook-8058-attempt-1";
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
            ai_tool: "OpenCode".to_string(),
            ai_model: Some("openai/gpt-5.6-terra".to_string()),
            ai_used_for: "Drafted test coverage.".to_string(),
            attempt_dispatcher: None,
            harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
        };
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
            ai_tool: "OpenCode".to_string(),
            ai_model: Some("openai/gpt-5.6-terra".to_string()),
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
    let baseline = materialize_follow_up_baseline(&report, "first-run").expect("baseline");
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

    let error = match materialize_follow_up_baseline(&report, "first-run") {
        Ok(_) => panic!("target advancement rejects the stale promotion baseline"),
        Err(error) => error,
    };

    assert!(
        error.message.contains("target HEAD changed"),
        "unexpected error: {}",
        error.message
    );
}
