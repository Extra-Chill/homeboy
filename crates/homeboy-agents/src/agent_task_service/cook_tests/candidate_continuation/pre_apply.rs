use super::*;
use serde_json::json;

struct RearmCandidate(CookRequest);
impl RearmCandidate {
    /// The admitted rearm step, as the daemon's execution-step adapter
    /// performs it through the Cook continuation handler.
    fn execute_admitted_rearm(&self, run: &str) -> Result<(Value, i32)> {
        use crate::agent_task_service::CookRecoveryPerformer;
        self.preflight_continue_cook(run, true)?;
        let recipes = CookRecipeStore::from_current_data_root()?;
        let store = test_lifecycle_store();
        let claim = crate::agent_task_service::cook_recipe::claim_continuation_for_recovery_and_clear_failure_in_store(
            &recipes, &store, &self.0.identity.cook_id, run,
        )?.expect("rearmed claim");
        let promotion = promote_or_load_attempt_in_store(&store, &self.0, run)?;
        claim.complete()?;
        Ok((serde_json::to_value(promotion).unwrap(), 0))
    }
}
impl crate::agent_task_service::CookRecoveryPerformer for RearmCandidate {
    fn admit_step(
        &self,
        report: &crate::agent_task_service::CookRecoveryReport,
        _: &crate::agent_task_service::CookRecoveryOverrides,
    ) -> Result<Option<Value>> {
        assert_eq!(
            report.step,
            crate::agent_task_service::CookRecoveryStep::Rearm
        );
        Ok(Some(json!({"status": "admitted"})))
    }
    fn preflight_continue_cook(&self, run: &str, rearm: bool) -> Result<(Value, i32)> {
        assert!(rearm);
        crate::agent_task_service::cook_recipe::preflight_continuation_claim(
            &self.0.identity.cook_id,
            run,
            rearm,
        )?;
        Ok((json!({}), 0))
    }
    fn retry(&self, _: &str) -> Result<(Value, i32)> {
        panic!("no provider retry")
    }
    fn reconcile(&self, _: &str) -> Result<(Value, i32)> {
        panic!("no reconcile")
    }
    fn adopt(&self, _: &str, _: &str) -> Result<(Value, i32)> {
        panic!("no adoption")
    }
}

#[test]
fn recover_rearms_gate_feedback_promotion_refused_before_apply() {
    with_corrected_gate_candidate(|mut options, target, promotion| {
        options.workspace.to_worktree = target.display().to_string();
        let run = options.identity.initial_run_id.clone();
        let store = test_lifecycle_store();
        let candidate = promotion.provenance["candidate_commit"].clone();
        let mut aggregate = store.read_aggregate(&run).unwrap();
        aggregate.outcomes[0].metadata["gate_feedback_baseline"] =
            json!({"candidate_commit": candidate});
        store
            .record_run_aggregate(&run, &options.identity.initial_plan, &aggregate)
            .unwrap();
        store
            .mutate_record(&run, |record| {
                record.metadata["cook_delivery"]["artifacts"]
                    .as_object_mut()
                    .unwrap()
                    .remove("promotion");
                true
            })
            .unwrap();
        std::fs::write(target.join("tracked.txt"), "operator residue\n").unwrap();
        let refusal = promote_or_load_attempt_in_store(&store, &options, &run).unwrap_err();
        let diagnostic = crate::agent_task_service::cook::bounded_error_diagnostic(&refusal);
        agent_task_lifecycle::record_cook_controller_failure_in_store(&store, &run, &diagnostic)
            .unwrap();
        let recipes = CookRecipeStore::from_current_data_root().unwrap();
        recipes
            .enqueue_terminal_continuation(&options.identity.cook_id, &run)
            .unwrap();
        recipes
            .claim_continuation_for(&options.identity.cook_id, &run)
            .unwrap()
            .unwrap()
            .fail(&refusal.message)
            .unwrap();
        git_output(
            &target,
            &["reset", "--hard", candidate["base_sha"].as_str().unwrap()],
        )
        .unwrap();
        options.gates.verify = vec!["test \"$(cat tracked.txt)\" = promoted".into()];
        let performer = RearmCandidate(options.clone());
        let plan =
            crate::agent_task_service::plan_cook_recovery(&options.identity.cook_id).unwrap();
        assert_eq!(
            plan.step,
            crate::agent_task_service::CookRecoveryStep::Rearm
        );
        let projected =
            crate::orchestration::project_record(&store.read_record(&run).unwrap(), None, None)
                .unwrap()
                .recovery
                .unwrap();
        assert_eq!(projected.step, plan.step.action_name().unwrap());
        let (preflight, exit) = crate::agent_task_service::recover_cook(
            &options.identity.cook_id,
            crate::agent_task_service::CookRecoveryRequest {
                preflight: true,
                ..Default::default()
            },
            &performer,
        )
        .unwrap();
        assert_eq!(exit, 0, "{preflight:?}");
        assert_eq!(preflight.step, plan.step);
        let (report, exit) = crate::agent_task_service::recover_cook(
            &options.identity.cook_id,
            Default::default(),
            &performer,
        )
        .unwrap();
        assert_eq!(exit, 0, "{report:?}");
        assert_eq!(report.status, "admitted");
        let (result, exit) = performer.execute_admitted_rearm(&report.run_id).unwrap();
        assert_eq!(exit, 0, "{result:?}");
        assert_eq!(result["status"], "applied");
        assert!(!result["deterministic_gates"].as_array().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(target.join("tracked.txt")).unwrap(),
            "promoted\n"
        );
        assert!(
            crate::agent_task_service::cook_recipe::preflight_continuation_claim(
                &options.identity.cook_id,
                &run,
                false
            )
            .is_err()
        );
        assert!(
            crate::agent_task_service::cook_recipe::preflight_continuation_claim(
                &options.identity.cook_id,
                &run,
                true
            )
            .is_err()
        );
    });
}
