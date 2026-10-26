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
        let mut backend = CaptureBackend {
            hydrate_run_id: Some(options.identity.initial_run_id.clone()),
            ..Default::default()
        };
        let result = continue_terminal(&options, &mut backend).unwrap();
        assert_eq!(result.value.status, "review_ready", "{:#?}", result.value);
        assert_eq!(result.exit_code, 0);
        assert_eq!(backend.create_count, 1);
        assert!(backend.body.contains("gpt-6.1-sol"));
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
                && diagnostic.contains("durable run must have succeeded"),
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
