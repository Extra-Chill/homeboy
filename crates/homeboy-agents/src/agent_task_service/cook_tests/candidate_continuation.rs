//! Candidate-only recovery regressions: real Git, durable records, no patch list.
use super::*;

fn make_multi_attempt(
    options: &CookRequest,
    promotion: &AgentTaskPromotionReport,
    target: &std::path::Path,
    green: bool,
) -> (CookRequest, AgentTaskPromotionReport) {
    let recipes = CookRecipeStore::from_current_data_root().unwrap();
    let store = test_lifecycle_store();
    let original = &options.identity.initial_run_id;
    let mut options = options.clone();
    options.identity.initial_run_id = format!("{original}-attempt-2");
    let run = &options.identity.initial_run_id;
    recipes
        .record_recipe_attempt(
            &options.identity.cook_id,
            2,
            run,
            &options.identity.initial_plan,
        )
        .unwrap();
    submit_plan_in_test_store(&store, &options.identity.initial_plan, Some(run)).unwrap();
    store
        .record_cook_attempt(&options.identity.cook_id, 2, run)
        .unwrap();
    let aggregate = store.read_aggregate(original).unwrap();
    store
        .record_run_aggregate(run, &options.identity.initial_plan, &aggregate)
        .unwrap();
    let execution = store
        .read_record(original)
        .unwrap()
        .lifecycle
        .provider_runtime;
    store
        .mutate_record(run, |record| {
            record.lifecycle.provider_runtime = execution.clone();
            true
        })
        .unwrap();
    let mut promotion = promotion.clone();
    promotion.source.run_id = Some(run.clone());
    let sha = promotion.provenance["candidate_commit"]["candidate_sha"]
        .as_str()
        .unwrap();
    git_output(
        target,
        &["update-ref", &format!("refs/homeboy/candidates/{run}"), sha],
    )
    .unwrap();
    promotion
        .provenance
        .as_object_mut()
        .unwrap()
        .remove("replacement_gate_proof");
    promotion.status = AgentTaskPromotionStatus::GateFailed;
    promotion.deterministic_gates[0].status = crate::agent_task_gate::AgentTaskGateStatus::Failed;
    promotion.deterministic_gates[0].exit_code = 1;
    promotion.normalize_gate_outcome();
    store
        .record_promotion(run, serde_json::to_value(&promotion).unwrap())
        .unwrap();
    let promotion = if green {
        verify_replacement_gates(
            run,
            VerifyGateOptions {
                verify: vec!["test \"$(cat tracked.txt)\" = promoted".into()],
                ..Default::default()
            },
            "operator authorizes candidate-only replacement".into(),
            None,
        )
        .unwrap()
    } else {
        promotion
    };
    assert!(aggregate.outcomes[0].artifacts.is_empty());
    assert!(aggregate.outcomes[0].candidate.is_some());
    assert_eq!(
        aggregate.outcomes[0].status,
        crate::agent_task::AgentTaskOutcomeStatus::CandidateRecoverable
    );
    (options, promotion)
}

struct PublishCandidate(std::sync::Mutex<CaptureBackend>);
impl crate::agent_task_service::CookRecoveryPerformer for PublishCandidate {
    fn continue_cook(
        &self,
        _: &str,
        _: bool,
        _: &crate::agent_task_service::CookRecoveryOverrides,
    ) -> Result<(Value, i32)> {
        panic!("must publish without provider redispatch")
    }
    fn preflight_continue_cook(&self, _: &str, _: bool) -> Result<(Value, i32)> {
        panic!("must publish")
    }
    fn retry(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not retry")
    }
    fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not reconcile")
    }
    fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
        panic!("must not adopt")
    }
    fn publish(
        &self,
        run: &str,
        _: &crate::agent_task_service::CookPublishRequest,
        preflight: bool,
    ) -> Result<(Value, i32)> {
        assert!(!preflight);
        let value =
            recover_cook_pr_with_backend(run, Vec::new(), false, &mut *self.0.lock().unwrap())?;
        Ok((value, 0))
    }
}

#[test]
fn multi_attempt_candidate_recoverable_recover_publishes_candidate_sha() {
    with_corrected_gate_candidate(|options, target, promotion| {
        let (options, promotion) = make_multi_attempt(&options, &promotion, &target, true);
        let sha = promotion.provenance["candidate_commit"]["candidate_sha"]
            .as_str()
            .unwrap();
        assert_eq!(
            git_output(
                &target,
                &[
                    "rev-parse",
                    &format!(
                        "refs/homeboy/candidates/{}",
                        options.identity.initial_run_id
                    )
                ]
            )
            .unwrap(),
            sha
        );
        assert!(promotion
            .deterministic_gates
            .iter()
            .all(|gate| gate.exit_code == 0));
        let performer = PublishCandidate(std::sync::Mutex::new(CaptureBackend {
            hydrate_run_id: Some(options.identity.initial_run_id.clone()),
            committed_sha: Some(sha.to_string()),
            ..Default::default()
        }));
        let (report, exit) = crate::agent_task_service::recover_cook(
            &options.identity.cook_id,
            Default::default(),
            &performer,
        )
        .unwrap();
        assert_eq!(exit, 0, "{report:#?}");
        assert_eq!(
            report.step,
            crate::agent_task_service::CookRecoveryStep::Publish
        );
        assert_eq!(report.result.unwrap()["status"], "review_ready");
        let backend = performer.0.lock().unwrap();
        assert!(backend.created);
        assert!(backend.body.contains("gpt-6.1-sol"));
    });
}

#[test]
fn publication_failure_recover_publishes_same_candidate_without_provider_execution() {
    with_own_green_gates(|options, target| {
        let run = &options.identity.initial_run_id;
        let promotion = persisted_promotion_for_attempt(run).unwrap().unwrap();
        let sha = promotion.provenance["candidate_commit"]["candidate_sha"]
            .as_str()
            .unwrap()
            .to_string();
        let before_head = git_output(&target, &["rev-parse", "HEAD"]).unwrap();
        let before_runtime = test_lifecycle_store()
            .read_record(run)
            .unwrap()
            .lifecycle
            .provider_runtime;
        let mut failed = CaptureBackend {
            hydrate_run_id: Some(run.clone()),
            commit_error: true,
            ..Default::default()
        };
        recover_cook_pr_with_backend(run, Vec::new(), false, &mut failed)
            .expect_err("injected publication failure");
        assert!(!failed.created);
        let performer = PublishCandidate(std::sync::Mutex::new(CaptureBackend {
            hydrate_run_id: Some(run.clone()),
            committed_sha: Some(sha.clone()),
            ..Default::default()
        }));
        let (report, exit) =
            crate::agent_task_service::recover_cook(run, Default::default(), &performer).unwrap();
        assert_eq!(exit, 0, "{report:#?}");
        assert_eq!(
            report.step,
            crate::agent_task_service::CookRecoveryStep::Publish
        );
        assert!(performer.0.lock().unwrap().created);
        assert_eq!(
            git_output(&target, &["rev-parse", "HEAD"]).unwrap(),
            before_head
        );
        let after = persisted_promotion_for_attempt(run).unwrap().unwrap();
        assert_eq!(after.provenance["candidate_commit"]["candidate_sha"], sha);
        assert_eq!(
            serde_json::to_value(
                test_lifecycle_store()
                    .read_record(run)
                    .unwrap()
                    .lifecycle
                    .provider_runtime
            )
            .unwrap(),
            serde_json::to_value(before_runtime).unwrap()
        );
    });
}

#[test]
fn multi_attempt_substantive_adopt_uses_terminal_execution_lineage() {
    with_corrected_gate_candidate(|options, target, promotion| {
        let (options, promotion) = make_multi_attempt(&options, &promotion, &target, true);
        let sha = promotion.provenance["candidate_commit"]["candidate_sha"]
            .as_str()
            .unwrap();
        // The candidate commit, not destination HEAD or any patch evidence.
        assert_ne!(git_output(&target, &["rev-parse", "HEAD"]).unwrap(), sha);
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(options.identity.initial_run_id.clone()),
            committed_sha: Some(sha.to_string()),
            ..Default::default()
        };
        let result = adopt_cook_candidate_with_dispatcher_and_backend(
            &options.identity.cook_id,
            sha,
            AgentTaskCandidateAdoptionOptions::default(),
            |_| Ok(None),
            Arc::new(UnusedExecutor),
            &mut backend,
        )
        .unwrap();
        assert_eq!(result.exit_code, 0, "{result:#?}");
        assert_eq!(result.value.status, "review_ready");
        assert!(backend.created);
        assert!(backend.body.contains("gpt-6.1-sol"));
        let stored = persisted_promotion_for_attempt(&options.identity.initial_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.provenance["candidate_commit"]["candidate_sha"], sha);
        assert!(stored.provenance.get("cook_follow_up").is_none());
    });
}

#[test]
fn replacement_gates_recover_attaches_proof_to_candidate_sha_without_artifacts() {
    with_corrected_gate_candidate(|options, target, promotion| {
        let (options, mut promotion) = make_multi_attempt(&options, &promotion, &target, false);
        let candidate = promotion.provenance["candidate_commit"].clone();
        promotion
            .provenance
            .as_object_mut()
            .unwrap()
            .remove("replacement_gate_proof");
        promotion.deterministic_gates[0].exit_code = 1;
        promotion.deterministic_gates[0].status =
            crate::agent_task_gate::AgentTaskGateStatus::Failed;
        promotion.normalize_gate_outcome();
        test_lifecycle_store()
            .record_promotion(
                &options.identity.initial_run_id,
                serde_json::to_value(promotion).unwrap(),
            )
            .unwrap();
        let (report, exit) = crate::agent_task_service::recover_cook(
            &options.identity.cook_id,
            crate::agent_task_service::CookRecoveryRequest {
                replace_gates: Some(crate::agent_task_service::CookReplaceGatesRequest {
                    proof: crate::agent_task_service::CookReplacementProof::Gates(
                        VerifyGateOptions {
                            verify: vec!["test \"$(cat tracked.txt)\" = promoted".into()],
                            ..Default::default()
                        },
                    ),
                    authorize_external_proof: "operator authorizes candidate-bound gates".into(),
                    authorize_interrupted_rerun: None,
                }),
                ..Default::default()
            },
            &ReplacementOnlyRecoveryPerformer,
        )
        .unwrap();
        assert_eq!(exit, 0, "{report:#?}");
        assert_eq!(
            report.step,
            crate::agent_task_service::CookRecoveryStep::ReplaceGates
        );
        let stored = persisted_promotion_for_attempt(&options.identity.initial_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.provenance["candidate_commit"], candidate);
        assert!(stored.provenance.get("replacement_gate_proof").is_some());
        assert_eq!(stored.deterministic_gates[0].exit_code, 0);
    });
}

/// #16441: the candidate's own continuation gates, green and bound to its
/// fingerprint, with no operator replacement proof. The provider runtime
/// failed but recorded its concrete model.
fn with_own_green_gates(test: impl FnOnce(CookRequest, std::path::PathBuf)) {
    with_corrected_gate_candidate(|options, target, replacement| {
        let mut own = replacement;
        own.provenance
            .as_object_mut()
            .unwrap()
            .remove("replacement_gate_proof");
        let store = test_lifecycle_store();
        let run_id = &options.identity.initial_run_id;
        store
            .record_promotion(run_id, serde_json::to_value(&own).unwrap())
            .unwrap();
        let record = store.read_record(run_id).unwrap();
        assert_eq!(
            record.lifecycle.execution.state,
            RunExecutionState::CandidateRecoverable
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            ProviderRuntimeState::Failed
        );
        test(options, target);
    });
}

fn continue_terminal(
    options: &CookRequest,
    backend: &mut CaptureBackend,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let recipe_store = CookRecipeStore::from_current_data_root().unwrap();
    let lifecycle_store = test_lifecycle_store();
    let runtime = CookRuntime::with_finalizer(
        Arc::new(UnusedExecutor),
        &recipe_store,
        &lifecycle_store,
        |store, options, run_id, promotion| {
            finalize_or_load_cook_pr_with_backend_with_stores(
                &recipe_store,
                store,
                options,
                run_id,
                promotion,
                backend,
            )
        },
        &noop_cook_progress_observer,
    );
    CookService::run(options.clone(), runtime, CookMode::ContinueTerminal)
}

#[test]
fn candidate_recoverable_with_own_green_bound_gates_publishes() {
    with_own_green_gates(|options, _| {
        let run = &options.identity.initial_run_id;
        let before = persisted_promotion_for_attempt(run).unwrap().unwrap();
        let performer = PublishCandidate(std::sync::Mutex::new(CaptureBackend {
            hydrate_run_id: Some(run.clone()),
            ..Default::default()
        }));
        let (report, exit) = crate::agent_task_service::recover_cook(
            &options.identity.cook_id,
            Default::default(),
            &performer,
        )
        .unwrap();
        assert_eq!(exit, 0, "{report:#?}");
        assert_eq!(
            report.step,
            crate::agent_task_service::CookRecoveryStep::Publish
        );
        assert_eq!(report.result.unwrap()["status"], "review_ready");
        let after = persisted_promotion_for_attempt(run).unwrap().unwrap();
        assert_eq!(before.deterministic_gates, after.deterministic_gates);
        assert_eq!(performer.0.lock().unwrap().create_count, 1);
    });
}

#[test]
fn live_continuation_is_visible_and_recovery_leaves_its_claim_untouched() {
    with_own_green_gates(|options, _| {
        let (cook, run) = (&options.identity.cook_id, &options.identity.initial_run_id);
        let recipes = CookRecipeStore::from_current_data_root().unwrap();
        recipes.enqueue_terminal_continuation(cook, run).unwrap();
        let _claim = recipes.claim_continuation_for(cook, run).unwrap().unwrap();
        let store = test_lifecycle_store();
        let before = store.read_record(run).unwrap();
        let status = crate::orchestration::run_from_current_environment(cook).unwrap();
        assert_eq!(
            status.state,
            homeboy_control_plane_contract::ControlPlaneRunState::Running
        );
        assert_eq!(status.phase.as_deref(), Some("publishing"));
        assert_eq!(status.owner.unwrap().id, std::process::id().to_string());
        let performer = PublishCandidate(std::sync::Mutex::new(CaptureBackend::default()));
        let (report, exit) =
            crate::agent_task_service::recover_cook(cook, Default::default(), &performer).unwrap();
        assert_eq!(exit, 0);
        assert!(report.step.is_none());
        assert_eq!(
            report.result.unwrap()["next_action"],
            format!("homeboy agent-task status {cook} --watch")
        );
        let after = store.read_record(run).unwrap();
        assert_eq!(
            before.metadata["cook_continuation"],
            after.metadata["cook_continuation"]
        );
        assert_eq!(
            before.metadata["cook_delivery"],
            after.metadata["cook_delivery"]
        );
        assert!(!performer.0.lock().unwrap().created);
    });
}

/// #16441: a refused publication is deterministic, so a terminal continuation
/// whose green, bound gates cannot publish ends there, naming the refusal,
/// instead of scheduling another generation that re-runs the same gates.
#[test]
fn refused_publication_ends_the_terminal_continuation_without_regating() {
    with_own_green_gates(|options, _| {
        let (cook_id, run_id) = (&options.identity.cook_id, &options.identity.initial_run_id);
        let store = test_lifecycle_store();
        // The producing runtime records no concrete model: publication refuses.
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.lifecycle.provider_runtime[0].metadata = serde_json::json!({});
        })
        .unwrap();
        let promotions = || {
            store.read_record(run_id).unwrap().metadata["cook_delivery"]["artifacts"]["promotions"]
                .clone()
        };
        let before = promotions();
        let recipes = CookRecipeStore::from_current_data_root().unwrap();
        recipes
            .enqueue_terminal_continuation(cook_id, run_id)
            .unwrap();
        let claim = recipes
            .claim_continuation_for(cook_id, run_id)
            .unwrap()
            .unwrap();
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(run_id.clone()),
            ..Default::default()
        };
        let exit = crate::agent_task_service::consume_claimed_terminal_with_dispatcher(
            claim,
            |_| Ok(Some(Arc::new(AcceptedDetachedAttemptDispatcher))),
            |options| continue_terminal(&options, &mut backend).map(|result| result.exit_code),
        )
        .unwrap();

        assert_eq!(exit, 1);
        assert_eq!(backend.create_count, 0);
        let record = store.read_record(run_id).unwrap();
        let continuation = &record.metadata["cook_continuation"];
        assert_eq!(continuation["state"], "failed", "{continuation:#}");
        assert_eq!(continuation["generation"], 0, "{continuation:#}");
        let diagnostic = continuation["diagnostic"].as_str().unwrap();
        assert!(
            diagnostic.starts_with("publish_refused: ")
                && diagnostic.contains("publication refused:"),
            "{diagnostic}"
        );
        let blocker = crate::agent_task_service::CookDelivery::from_run_metadata(&record.metadata)
            .and_then(|delivery| delivery.blocker)
            .unwrap();
        assert_eq!(
            blocker.code,
            crate::agent_task_service::CookDeliveryBlockerCode::PublishRefused
        );
        // No gate ran again, and nothing is left to claim.
        assert_eq!(promotions(), before);
        assert!(recipes
            .claim_continuation_for(cook_id, run_id)
            .unwrap()
            .is_none());
    });
}

/// #17494: one stopped-Cook state per case, each read back through the public
/// recovery entry points. Every state names exactly one step that makes
/// progress, and performing it changes durable state rather than reporting a
/// no-op.
#[derive(Clone, Copy, Debug)]
enum WedgedCook {
    /// The candidate is applied but a Lab gate deferred, and an earlier
    /// manual publication intent is recorded.
    DeferredGateBeforePublish,
    /// Green gates, applied candidate, no AI-authored review form.
    MissingReviewForm,
    /// A provider liveness timeout left the attempt candidate-recoverable
    /// before any terminal continuation was scheduled.
    ContinuationNotScheduled,
    /// A terminal Cook whose driver exited still claims its destination.
    WedgedTerminalDriver,
}

/// Continues by claiming the scheduled continuation, as the CLI handler
/// does; every other step is refused.
struct ClaimingContinuation;
impl crate::agent_task_service::CookRecoveryPerformer for ClaimingContinuation {
    fn continue_cook(
        &self,
        run: &str,
        rearm: bool,
        _: &crate::agent_task_service::CookRecoveryOverrides,
    ) -> Result<(Value, i32)> {
        assert!(!rearm);
        let recipes = CookRecipeStore::from_current_data_root().unwrap();
        let cook = recipes
            .load_recipe_for_attempt(run)
            .unwrap()
            .unwrap()
            .cook_id;
        let claimed = recipes
            .claim_continuation_for(&cook, run)
            .unwrap()
            .is_some();
        Ok((
            serde_json::json!({ "claimed": claimed }),
            i32::from(!claimed),
        ))
    }
    fn preflight_continue_cook(&self, _: &str, _: bool) -> Result<(Value, i32)> {
        panic!("recovery performs, it does not preflight")
    }
    fn retry(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not retry")
    }
    fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not reconcile")
    }
    fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
        panic!("must not adopt")
    }
    fn publish(
        &self,
        run: &str,
        _: &crate::agent_task_service::CookPublishRequest,
        _: bool,
    ) -> Result<(Value, i32)> {
        panic!("publication of {run} is refused in this state; it is not a next step")
    }
}

fn wedge(case: WedgedCook, options: &CookRequest) {
    let run = &options.identity.initial_run_id;
    let store = test_lifecycle_store();
    match case {
        WedgedCook::DeferredGateBeforePublish => {
            let mut promotion = persisted_promotion_for_attempt(run).unwrap().unwrap();
            promotion.status = AgentTaskPromotionStatus::GateDeferred;
            promotion.deterministic_gates[0].status =
                crate::agent_task_gate::AgentTaskGateStatus::Deferred;
            promotion.deterministic_gates[0].exit_code = 0;
            promotion.normalize_gate_outcome();
            store
                .record_promotion(run, serde_json::to_value(&promotion).unwrap())
                .unwrap();
            agent_task_lifecycle::record_manual_finalization_intent(
                run,
                serde_json::json!({ "run_id": run }),
            )
            .unwrap();
        }
        WedgedCook::MissingReviewForm => {
            let mut aggregate = store.read_aggregate(run).unwrap();
            aggregate.outcomes[0].outputs = Value::Null;
            store
                .record_run_aggregate(run, &options.identity.initial_plan, &aggregate)
                .unwrap();
        }
        WedgedCook::ContinuationNotScheduled => {
            agent_task_lifecycle::rewrite_record_for_test(run, |record| {
                crate::agent_task_service::set_cook_delivery_artifact_for_test(
                    &mut record.metadata,
                    "/promotion",
                    Value::Null,
                );
                crate::agent_task_service::set_cook_delivery_artifact_for_test(
                    &mut record.metadata,
                    "/promotions",
                    serde_json::json!([]),
                );
                record.ensure_metadata_object().remove("cook_continuation");
            })
            .unwrap();
        }
        WedgedCook::WedgedTerminalDriver => {}
    }
}

#[test]
fn every_wedged_cook_state_has_one_progressing_recovery_step() {
    use crate::agent_task_service::CookRecoveryStep;
    for case in [
        WedgedCook::DeferredGateBeforePublish,
        WedgedCook::MissingReviewForm,
        WedgedCook::ContinuationNotScheduled,
        WedgedCook::WedgedTerminalDriver,
    ] {
        // A deferred gate never produced replacement proof; every other state
        // starts from the candidate the corrected gates verified.
        let fixture = |test: &dyn Fn(CookRequest)| match case {
            WedgedCook::DeferredGateBeforePublish => {
                with_corrected_gate_failure(false, |options, _| test(options))
            }
            _ => with_corrected_gate_candidate(|options, _, _| test(options)),
        };
        fixture(&|options| {
            wedge(case, &options);
            let cook = &options.identity.cook_id;
            let run = &options.identity.initial_run_id;
            let plan = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
            match case {
                WedgedCook::DeferredGateBeforePublish => {
                    // The recipe's own gates re-run before anything publishes.
                    assert_eq!(plan.status, "gate_environment_unavailable", "{case:?}");
                    assert_eq!(plan.step, CookRecoveryStep::Regate, "{case:?}");
                    // The operator alternative is admitted for the same state.
                    let (report, exit) = crate::agent_task_service::recover_cook(
                        cook,
                        crate::agent_task_service::CookRecoveryRequest {
                            replace_gates: Some(
                                crate::agent_task_service::CookReplaceGatesRequest {
                                    proof: crate::agent_task_service::CookReplacementProof::Gates(
                                        VerifyGateOptions {
                                            verify: vec![
                                                "test \"$(cat tracked.txt)\" = promoted".into()
                                            ],
                                            ..Default::default()
                                        },
                                    ),
                                    authorize_external_proof: "operator re-runs deferred gates"
                                        .into(),
                                    authorize_interrupted_rerun: None,
                                },
                            ),
                            ..Default::default()
                        },
                        &ReplacementOnlyRecoveryPerformer,
                    )
                    .unwrap();
                    assert_eq!(exit, 0, "{case:?}: {report:#?}");
                    let stored = persisted_promotion_for_attempt(run).unwrap().unwrap();
                    assert!(stored.finalization_eligible(), "{case:?}");
                }
                WedgedCook::MissingReviewForm | WedgedCook::ContinuationNotScheduled => {
                    // A gate-green candidate missing only its form asks the
                    // continuation's review-only step for it (#16292).
                    let expected = match case {
                        WedgedCook::MissingReviewForm => CookRecoveryStep::RequestReviewForm {
                            missing: crate::agent_task_service::REVIEW_FORM_MISSING.into(),
                            rearm: false,
                        },
                        _ => CookRecoveryStep::Continue,
                    };
                    assert_eq!(plan.step, expected, "{case:?}: {plan:?}");
                    let (report, exit) = crate::agent_task_service::recover_cook(
                        cook,
                        Default::default(),
                        &ClaimingContinuation,
                    )
                    .unwrap();
                    assert_eq!(exit, 0, "{case:?}: {report:#?}");
                    assert_eq!(report.result.unwrap()["claimed"], true, "{case:?}");
                }
                WedgedCook::WedgedTerminalDriver => {
                    // Liveness is the kernel driver lock, observed from another
                    // controller. While a driver holds it, recovery names no
                    // step (as diagnose reports a running Cook); once it exits,
                    // the same Cook names its progressing step again.
                    let recipes = CookRecipeStore::from_current_data_root().unwrap();
                    let (held, release) = std::sync::mpsc::channel();
                    let (done, wait) = std::sync::mpsc::channel::<()>();
                    let driver = std::thread::spawn({
                        let recipes = recipes.clone();
                        let cook = cook.clone();
                        move || {
                            let _guard = recipes.try_acquire_cook_driver(&cook).unwrap().unwrap();
                            held.send(()).unwrap();
                            wait.recv().unwrap();
                        }
                    });
                    release.recv().unwrap();
                    let live = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
                    assert!(live.step.is_none(), "{case:?}: {live:?}");
                    done.send(()).unwrap();
                    driver.join().unwrap();
                    assert!(recipes.foreign_cook_driver(cook).unwrap().is_none());
                    let released = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
                    assert_eq!(released.step, plan.step, "{case:?}");
                    assert!(!released.step.is_none(), "{case:?}: {released:?}");
                }
            }
        });
    }
}

/// #17508: recovery step selection and the publication guard apply one
/// passing verdict to an applied candidate's gates. A proven inherited
/// failure passes, so the step recovery selects for it publishes instead of
/// deadlocking between a refused publish and refused replacement gates.
#[test]
fn applied_candidate_gate_verdicts_select_a_completing_recovery_step() {
    use crate::agent_task_gate::{
        AgentTaskGateBaselineComparison, AgentTaskGateDifferentialResult,
        AgentTaskGateFailureClassification, AgentTaskGateFailureEvidence, AgentTaskGateStatus,
    };
    use crate::agent_task_service::CookRecoveryStep;
    for second in [
        AgentTaskGateStatus::AcceptedInheritedFailure,
        AgentTaskGateStatus::Failed,
        AgentTaskGateStatus::Succeeded,
    ] {
        with_own_green_gates(|options, _| {
            let cook = &options.identity.cook_id;
            let run = &options.identity.initial_run_id;
            let store = test_lifecycle_store();
            let mut promotion = persisted_promotion_for_attempt(run).unwrap().unwrap();
            let mut gate = promotion.deterministic_gates[0].clone();
            gate.id = format!("{}-second", gate.id);
            gate.status = second;
            if second != AgentTaskGateStatus::Succeeded {
                gate.exit_code = 1;
                gate.failure_evidence = Some(AgentTaskGateFailureEvidence {
                    classification: AgentTaskGateFailureClassification::CandidateCode,
                    summary: "second gate failed".to_string(),
                    command: gate.command.join(" "),
                    exit_code: 1,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    agent_feedback: String::new(),
                    diagnostics: Vec::new(),
                });
            }
            if second == AgentTaskGateStatus::AcceptedInheritedFailure {
                gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                    base_ref: "main".to_string(),
                    exit_code: 1,
                    failure_fingerprint: "inherited".to_string(),
                    matches_candidate_failure: true,
                    result: AgentTaskGateDifferentialResult::BaselineRed,
                    diagnostic: None,
                });
            }
            promotion.deterministic_gates.push(gate);
            promotion.status = AgentTaskPromotionStatus::GateFailed;
            promotion.normalize_gate_outcome();
            store
                .record_promotion(run, serde_json::to_value(&promotion).unwrap())
                .unwrap();
            let plan = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
            if second == AgentTaskGateStatus::Failed {
                // A real failure never selects publication; replacement gates
                // are admitted for it.
                assert_ne!(plan.step, CookRecoveryStep::Publish, "{second:?}: {plan:?}");
                let (report, exit) = crate::agent_task_service::recover_cook(
                    cook,
                    crate::agent_task_service::CookRecoveryRequest {
                        replace_gates: Some(crate::agent_task_service::CookReplaceGatesRequest {
                            proof: crate::agent_task_service::CookReplacementProof::Gates(
                                VerifyGateOptions {
                                    verify: vec!["test \"$(cat tracked.txt)\" = promoted".into()],
                                    ..Default::default()
                                },
                            ),
                            authorize_external_proof: "operator re-runs the failed gate".into(),
                            authorize_interrupted_rerun: None,
                        }),
                        ..Default::default()
                    },
                    &ReplacementOnlyRecoveryPerformer,
                )
                .unwrap();
                assert_eq!(exit, 0, "{second:?}: {report:#?}");
                assert_eq!(report.step, CookRecoveryStep::ReplaceGates, "{second:?}");
                assert_eq!(report.result.unwrap()["status"], "applied", "{second:?}");
                return;
            }
            assert_eq!(plan.step, CookRecoveryStep::Publish, "{second:?}: {plan:?}");
            let sha = promotion.provenance["candidate_commit"]["candidate_sha"]
                .as_str()
                .unwrap()
                .to_string();
            let performer = PublishCandidate(std::sync::Mutex::new(CaptureBackend {
                hydrate_run_id: Some(run.clone()),
                committed_sha: Some(sha),
                ..Default::default()
            }));
            let (report, exit) =
                crate::agent_task_service::recover_cook(cook, Default::default(), &performer)
                    .unwrap();
            assert_eq!(exit, 0, "{second:?}: {report:#?}");
            assert_eq!(report.step, CookRecoveryStep::Publish, "{second:?}");
            assert_eq!(
                report.result.unwrap()["status"],
                "review_ready",
                "{second:?}"
            );
            assert!(performer.0.lock().unwrap().created, "{second:?}");
            // The inherited failure stays recorded as evidence, not erased.
            let stored = persisted_promotion_for_attempt(run).unwrap().unwrap();
            assert_eq!(stored.deterministic_gates[1].status, second);
        });
    }
}

/// Continues the way the CLI handler does when nothing can be claimed: it
/// reports `continuation_not_scheduled` and leaves durable state unchanged.
struct UnclaimedContinuation(std::sync::atomic::AtomicUsize);
impl crate::agent_task_service::CookRecoveryPerformer for UnclaimedContinuation {
    fn continue_cook(
        &self,
        run: &str,
        _: bool,
        _: &crate::agent_task_service::CookRecoveryOverrides,
    ) -> Result<(Value, i32)> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok((
            serde_json::json!({ "latest_run_id": run, "status": "continuation_not_scheduled" }),
            0,
        ))
    }
    fn preflight_continue_cook(&self, _: &str, _: bool) -> Result<(Value, i32)> {
        panic!("recovery performs, it does not preflight")
    }
    fn retry(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not retry")
    }
    fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not reconcile")
    }
    fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
        panic!("must not adopt")
    }
}

/// #17650: a step that left its durable state unchanged is not performed
/// again. The next `recover`, its preflight and the status planner all name
/// the same typed `blocked` step and its blocker; `--unblock` performs it once
/// more after the operator resolved the blocker.
#[test]
fn cook_recovery_blocks_a_repeated_non_progressing_step_on_every_surface() {
    use crate::agent_task_service::{CookRecoveryRequest, CookRecoveryStep};
    with_corrected_gate_candidate(|options, _, _| {
        wedge(WedgedCook::ContinuationNotScheduled, &options);
        let cook = &options.identity.cook_id;
        let performer = UnclaimedContinuation(Default::default());
        let performed = || performer.0.load(std::sync::atomic::Ordering::SeqCst);

        let (first, _) =
            crate::agent_task_service::recover_cook(cook, Default::default(), &performer).unwrap();
        assert_eq!(first.step, CookRecoveryStep::Continue, "{first:#?}");
        assert_eq!(performed(), 1);

        let (second, exit) =
            crate::agent_task_service::recover_cook(cook, Default::default(), &performer).unwrap();
        assert_eq!(exit, 1, "{second:#?}");
        let CookRecoveryStep::Blocked { repeated, blocker } = &second.step else {
            panic!("a repeated non-progressing step must be blocked: {second:#?}");
        };
        assert_eq!(repeated, "resume");
        assert!(blocker.contains("continuation_not_scheduled"), "{blocker}");
        assert_eq!(
            second.failure.as_ref().unwrap()["code"],
            "agent_task.recover_blocked"
        );
        assert_eq!(performed(), 1, "the blocked step is not performed again");

        let (preflight, _) = crate::agent_task_service::recover_cook(
            cook,
            CookRecoveryRequest {
                preflight: true,
                ..Default::default()
            },
            &performer,
        )
        .unwrap();
        assert_eq!(preflight.step, second.step);
        let plan = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
        let context =
            crate::agent_task_service::cook_failure_context(cook, Some(&plan.run_id), &plan.status)
                .unwrap();
        assert_eq!(context.recovery_step, Some(second.step.clone()));

        let (unblocked, _) = crate::agent_task_service::recover_cook(
            cook,
            CookRecoveryRequest {
                unblock: true,
                ..Default::default()
            },
            &performer,
        )
        .unwrap();
        assert_eq!(unblocked.step, CookRecoveryStep::Continue);
        assert_eq!(performed(), 2);
    });
}

/// Records whether publication was preflighted or performed.
struct RecordingPublish(std::sync::Mutex<Vec<bool>>);
impl crate::agent_task_service::CookRecoveryPerformer for RecordingPublish {
    fn continue_cook(
        &self,
        run: &str,
        rearm: bool,
        _: &crate::agent_task_service::CookRecoveryOverrides,
    ) -> Result<(Value, i32)> {
        panic!("recover must publish {run}, not continue it (rearm: {rearm})")
    }
    fn preflight_continue_cook(&self, run: &str, rearm: bool) -> Result<(Value, i32)> {
        panic!("preflight must validate publication of {run}, not continuation (rearm: {rearm})")
    }
    fn retry(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not retry")
    }
    fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
        panic!("must not reconcile")
    }
    fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
        panic!("must not adopt")
    }
    fn publish(
        &self,
        _: &str,
        _: &crate::agent_task_service::CookPublishRequest,
        preflight: bool,
    ) -> Result<(Value, i32)> {
        self.0.lock().unwrap().push(preflight);
        Ok((serde_json::json!({ "status": "validated" }), 0))
    }
}

/// #17641: the operator's recorded publication intent is a planner input, not
/// a `recover`-only override. Status, diagnose, `recover --preflight` and
/// `recover` therefore all name `publish` for it, and `recover` performs the
/// exact step its preflight validated rather than a continuation.
#[test]
fn cook_recovery_preflight_status_and_recover_name_one_step() {
    use crate::agent_task_service::{CookRecoveryRequest, CookRecoveryStep};
    with_corrected_gate_candidate(|options, _, _| {
        wedge(WedgedCook::MissingReviewForm, &options);
        let cook = &options.identity.cook_id;
        let run = &options.identity.initial_run_id;
        agent_task_lifecycle::record_manual_finalization_intent(
            run,
            serde_json::json!({ "run_id": run }),
        )
        .unwrap();

        let plan = crate::agent_task_service::plan_cook_recovery(cook).unwrap();
        assert_eq!(plan.step, CookRecoveryStep::Publish, "{plan:?}");
        let context =
            crate::agent_task_service::cook_failure_context(cook, Some(&plan.run_id), &plan.status)
                .unwrap();
        assert_eq!(context.recovery_step, Some(CookRecoveryStep::Publish));
        assert!(context
            .next_actions
            .iter()
            .any(|action| action.action == "publish"));

        let performer = RecordingPublish(Default::default());
        let (preflight, _) = crate::agent_task_service::recover_cook(
            cook,
            CookRecoveryRequest {
                preflight: true,
                ..Default::default()
            },
            &performer,
        )
        .unwrap();
        let (performed, exit) =
            crate::agent_task_service::recover_cook(cook, Default::default(), &performer).unwrap();
        assert_eq!(exit, 0, "{performed:#?}");
        assert_eq!(preflight.step, CookRecoveryStep::Publish);
        assert_eq!(performed.step, CookRecoveryStep::Publish);
        assert_eq!(*performer.0.lock().unwrap(), vec![true, false]);
    });
}
