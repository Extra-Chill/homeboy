//! Durable continuation state reads and execution transitions.

use super::run::*;

/// longer a public command of its own.
#[derive(Debug, Clone, Default)]
pub(crate) struct CookContinueArgs {
    /// Durable Cook ID or one of its provider attempt IDs.
    pub cook_or_attempt_id: String,
    /// Rearm one failed terminal continuation before consuming it.
    pub rearm: bool,

    /// Explicit provider timeout increase for a new retry attempt.
    pub timeout_ms: Option<u64>,
    /// Provider route for a rearmed retry.
    pub backend: Option<String>,
    pub selector: Option<String>,
    pub model: Option<String>,
    pub acknowledge_model_override: bool,
    /// Include the complete Cook report rather than the compact lifecycle view.
    pub full: bool,
}

/// Resume a Cook from its immutable recipe rather than asking the operator to
/// replay prompt, provider, gate, workspace, or disclosure arguments.
pub(crate) fn run_cook_continuation(args: CookContinueArgs) -> CmdResult<Value> {
    run_cook_continuation_with(
        args,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )
}

pub(crate) fn run_cook_continuation_with<F>(
    args: CookContinueArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    run_cook_continuation_execution(args, executor, reconstruct_dispatcher)
}

pub(super) fn explicit_local_continuation_decision(
    plan: &homeboy::agents::agent_task_scheduler::AgentTaskPlan,
) -> homeboy::core::Result<Option<homeboy_lab_contract::ExecutionPlacementDecision>> {
    if !homeboy::core::resource_policy_context::captured_context()
        .is_some_and(|context| context.local_override)
    {
        return Ok(None);
    }
    let prior: homeboy_lab_contract::ExecutionPlacementDecision = serde_json::from_value(
        plan.metadata["execution_placement_decision"].clone(),
    )
    .map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "execution_placement_decision",
            format!("durable Cook attempt has no valid placement decision: {error}"),
            None,
            None,
        )
    })?;
    Ok(Some(homeboy_lab_contract::ExecutionPlacementDecision::new(
        prior.policy_id,
        prior.policy_revision,
        prior.identity,
        homeboy_lab_contract::Placement::Local,
        homeboy_lab_contract::ExecutionPlacementRequirement::Either,
        homeboy_lab_contract::EffectiveExecutionPlacement::Local,
        None,
        homeboy_lab_contract::ExecutionPlacementFallback {
            local_allowed: false,
            reason: None,
        },
        homeboy_lab_contract::ExecutionPlacementOverrideAuthorization {
            authorized: true,
            authority: Some("operator --placement local".to_string()),
        },
    )))
}

pub(super) fn apply_explicit_local_continuation(
    run_id: &str,
    options: &mut homeboy::agents::agent_task_service::CookRequest,
) -> homeboy::core::Result<()> {
    let Some(decision) = explicit_local_continuation_decision(&options.identity.initial_plan)?
    else {
        return Ok(());
    };
    let lifecycle_store =
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    homeboy::agents::agent_task_lifecycle::transition_execution_placement_for_continuation_in_store(
        &lifecycle_store,
        run_id,
        decision.clone(),
    )?;
    apply_local_continuation_decision(options, decision)
}

pub(super) fn apply_local_continuation_decision(
    options: &mut homeboy::agents::agent_task_service::CookRequest,
    decision: homeboy_lab_contract::ExecutionPlacementDecision,
) -> homeboy::core::Result<()> {
    options.identity.initial_plan.metadata["execution_placement_decision"] =
        serde_json::to_value(decision)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    options.provider_transport.attempt_dispatcher = None;
    Ok(())
}

pub(super) fn cook_provider_route_override(
    backend: Option<String>,
    selector: Option<String>,
    model: Option<String>,
    acknowledge_model_override: bool,
) -> homeboy::agents::agent_task_service::CookProviderRouteOverride {
    homeboy::agents::agent_task_service::CookProviderRouteOverride {
        backend,
        selector,
        model,
        acknowledge_model_override,
    }
}

/// Queued replacement attempts belong to their committed execution step.
/// Continuation observes them until terminal, when the existing terminal Cook
/// continuation path can advance promotion, gates and finalization.
pub(super) fn run_cook_continuation_execution<F>(
    args: CookContinueArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    let recipe =
        agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        })?;
    let run_id = agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id)?;
    let record = agent_task_service::reconcile_recipe_attempt_for_continuation(&recipe, &run_id)?;
    if record
        .metadata
        .pointer("/pre_execution_failure/error_code")
        .and_then(Value::as_str)
        == Some("runner.job_lost")
    {
        let (_, retry) = reserve_retry_through_action(
            &run_id,
            None,
            false,
            &Default::default(),
            None,
            uuid::Uuid::new_v4().to_string(),
        )?;
        let recipe = agent_task_service::load_recipe(&recipe.cook_id)?;
        return Ok((cook_continuation_status(&recipe.cook_id, &retry.record), 0));
    }
    let record =
        if homeboy::agents::agent_task_lifecycle::disconnected_runner_recovery_available(&record) {
            let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
            homeboy::agents::agent_task_lifecycle::recover_disconnected_runner_in_store(
                &store, &run_id,
            )?;
            agent_task_service::reconcile_recipe_attempt_for_continuation(&recipe, &run_id)?
        } else {
            record
        };
    let route_override = cook_provider_route_override(
        args.backend.clone(),
        args.selector.clone(),
        args.model.clone(),
        args.acknowledge_model_override,
    );
    if !route_override.is_empty() {
        if !args.rearm || !record.state().is_terminal() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-route",
                "Cook provider-route overrides require a terminal attempt to rearm",
                Some(run_id),
                None,
            ));
        }
        let (_, retry) = reserve_retry_through_action(
            &record.run_id,
            None,
            false,
            &route_override,
            None,
            uuid::Uuid::new_v4().to_string(),
        )?;
        let recipe = agent_task_service::load_recipe(&recipe.cook_id)?;
        return Ok((cook_continuation_status(&recipe.cook_id, &retry.record), 0));
    }
    if args.timeout_ms.is_some() && !record.state().is_terminal() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "timeout-ms",
            "Cook timeout override requires a terminal provider timeout",
            Some(run_id),
            None,
        ));
    }
    if let Some(timeout_ms) = args.timeout_ms {
        let (_, retry) = reserve_retry_through_action(
            &run_id,
            None,
            false,
            &Default::default(),
            Some(timeout_ms),
            uuid::Uuid::new_v4().to_string(),
        )?;
        let recipe = agent_task_service::load_recipe(&recipe.cook_id)?;
        return Ok((cook_continuation_status(&recipe.cook_id, &retry.record), 0));
    }
    if record.state().is_terminal() && agent_task_service::retryable_pre_execution_failure(&record)
    {
        if !args.rearm {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "rearm",
                "a retryable pre-execution Cook failure must be rearmed to reserve a durable successor attempt; run `agent-task recover`",
                Some(run_id),
                None,
            ));
        }
        // A failed readiness or transport check did not reach provider work.
        // Reserve its replacement through the retry owner rather than rerunning
        // the terminal record. The normal queue owner then dispatches the
        // successor, preserving append-only Cook lineage and budget.
        let (_, retry) = reserve_retry_through_action(
            &record.run_id,
            None,
            false,
            &Default::default(),
            None,
            uuid::Uuid::new_v4().to_string(),
        )?;
        let recipe = agent_task_service::load_recipe(&recipe.cook_id)?;
        return Ok((cook_continuation_status(&recipe.cook_id, &retry.record), 0));
    }
    if !record.state().is_terminal() {
        return Ok((cook_continuation_status(&recipe.cook_id, &record), 0));
    }

    if matches!(
        record.state(),
        homeboy_lifecycle_contract::RunExecutionState::Succeeded
            | homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable
    ) {
        // Ordinary continuation consumes only the pending entry scheduled by
        // authoritative reconciliation or by the recovery step that selected
        // it (`schedule_recovery_continuation`). Failed and completed claims
        // require an explicit rearm and can never be silently replayed.
        // The recipe membership check, the claim, and the state read are one
        // decision, so all three have to name the same home (#7505).
        let recipe_store = agent_task_service::CookRecipeStore::from_current_data_root()?;
        let claim = if args.rearm {
            agent_task_service::claim_continuation_for_recovery_and_clear_failure_in_store(
                &recipe_store,
                &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
                &recipe.cook_id,
                &run_id,
            )?
        } else {
            recipe_store.claim_continuation_for(&recipe.cook_id, &run_id)?
        };
        let Some(claim) = claim else {
            return Ok((
                cook_terminal_continuation_status(
                    &recipe.cook_id,
                    &run_id,
                    &format!("{:?}", record.state()),
                    agent_task_service::continuation_state_in_store(
                        &recipe_store,
                        &recipe.cook_id,
                        &run_id,
                    )?,
                ),
                0,
            ));
        };
        let mut result = None;
        let dispatcher = reconstruct_dispatcher;
        let executor = executor.clone();
        let execute = |options| {
            let mut options = options;
            apply_explicit_local_continuation(&run_id, &mut options)?;
            agent_task_service::authorize_cook_continue_route(&options)?;
            let cook = run_cook_explicit(options, executor.clone())?;
            let outcome = (cook.exit_code, cook.value.disposition);
            result = Some(cook.value);
            Ok(outcome)
        };
        let exit_code =
            agent_task_service::consume_claimed_with_dispatcher(claim, dispatcher, execute)?;
        let value = cook_report_with_continuation(
            serde_json::to_value(result.ok_or_else(|| {
                homeboy::core::Error::internal_unexpected(
                    "claimed Cook continuation returned no result",
                )
            })?)
            .unwrap_or(Value::Null),
        );
        return Ok((
            super::status::compact_cook_report(value, args.full),
            exit_code,
        ));
    }

    let local_override = explicit_local_continuation_decision(
        &recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == run_id)
            .ok_or_else(|| {
                homeboy::core::Error::internal_unexpected("selected Cook attempt is absent")
            })?
            .plan,
    )?;
    let dispatcher = if local_override.is_some() {
        None
    } else {
        reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?
    };
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cook_or_attempt_id",
                "selected attempt is absent from its durable Cook recipe",
                Some(run_id.clone()),
                None,
            )
        })?;
    let mut options = if local_override.is_some() {
        agent_task_service::reconstruct_options_without_dispatcher(&recipe)?
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)?
    };
    options.identity.initial_run_id = attempt.run_id.clone();
    options.identity.initial_plan = attempt.plan.clone();
    if local_override.is_some() {
        apply_explicit_local_continuation(&run_id, &mut options)?;
    }
    agent_task_service::authorize_cook_continue_route(&options)?;
    let result = run_cook_explicit(options, executor)?;
    let value =
        cook_report_with_continuation(serde_json::to_value(result.value).unwrap_or(Value::Null));
    Ok((
        super::status::compact_cook_report(value, args.full),
        result.exit_code,
    ))
}

/// Probe the continuation admission boundary without claiming a continuation or
/// calling any execution, transport, or finalization API.
pub(crate) fn preflight_continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    let mut phases = Vec::new();
    let mut selected_run_id = None;
    let mut candidate_fingerprint = Value::Null;
    let mut continuation_reconciliation_required = false;
    let recipe =
        match agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        }) {
            Ok(recipe) => {
                phases.push(
                    serde_json::json!({ "phase": "recipe", "status": "passed", "reason": "ok" }),
                );
                recipe
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        None,
                        None,
                        Value::Null,
                        phases,
                        "recipe",
                        &error,
                    ),
                    1,
                ))
            }
        };
    let run_id =
        match agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id) {
            Ok(run_id) => {
                selected_run_id = Some(run_id.clone());
                phases.push(
                    serde_json::json!({ "phase": "selection", "status": "passed", "reason": "ok" }),
                );
                run_id
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        None,
                        Value::Null,
                        phases,
                        "selection",
                        &error,
                    ),
                    1,
                ))
            }
        };
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    if lifecycle_store.read_record(&run_id).is_ok_and(|record| {
        homeboy::agents::agent_task_lifecycle::disconnected_runner_recovery_available(&record)
    }) {
        return Ok((
            serde_json::json!({
                "run_id": run_id,
                "admitted": true,
                "recovery": "reattach_existing_runner_job",
                "provider_redispatch": false,
                "phases": phases,
            }),
            0,
        ));
    }
    let (record, aggregate) =
        match agent_task_service::preflight_recipe_attempt_for_continuation_in_store(
            &lifecycle_store,
            &recipe,
            &run_id,
        ) {
            Ok(observation) => {
                let record = &observation.0;
                candidate_fingerprint =
                    agent_task_service::recorded_promotion_report(&record.metadata)
                        .ok()
                        .flatten()
                        .and_then(|mut promotion| {
                            promotion
                                .pointer_mut("/provenance/candidate")
                                .map(Value::take)
                        })
                        .unwrap_or(Value::Null);
                phases.push(serde_json::json!({ "phase": "lifecycle", "status": "passed", "reason": "ok", "state": format!("{:?}", record.state()) }));
                observation
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        None,
                        candidate_fingerprint,
                        phases,
                        "lifecycle",
                        &error,
                    ),
                    1,
                ))
            }
        };
    let continuation_command =
        || agent_task_service::cook_recover_command_for_record(&record, &run_id);
    if let Some(error) =
        agent_task_service::cook_continuation_owner_denial(&record, &recipe.cook_id)?
    {
        let mut report = cook_continuation_preflight_report(
            selected_run_id,
            None,
            candidate_fingerprint,
            phases,
            "continuation_admission",
            &error,
        );
        report["continuation_command"] = Value::Null;
        report["failure_context"]["next_action"]["command"] = Value::Null;
        return Ok((report, 1));
    }
    if let Some(finalization) = agent_task_service::recorded_finalization_receipt(&record.metadata)
        .filter(|finalization| !finalization.is_null())
        .filter(|_| {
            matches!(
                record.state(),
                homeboy_lifecycle_contract::RunExecutionState::Succeeded
                    | homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable
            )
        })
    {
        phases.push(serde_json::json!({
            "phase": "finalization_receipt",
            "status": "passed",
            "reason": "authoritative_replay",
        }));
        let continuation_state = match agent_task_service::preflight_continuation_claim(
            &recipe.cook_id,
            &run_id,
            args.rearm,
        ) {
            Ok(state) => state,
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Some(continuation_command()),
                        candidate_fingerprint,
                        phases,
                        "continuation_claim",
                        &error,
                    ),
                    1,
                ));
            }
        };
        if continuation_state == agent_task_service::CookContinuationState::Absent {
            phases.push(serde_json::json!({
                "phase": "continuation_claim",
                "status": "blocked",
                "reason": "continuation_not_scheduled",
                "state": "Absent",
                "rearm": args.rearm,
            }));
            return Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-cook-continuation-preflight/v1",
                    "admitted": false,
                    "pre_dispatch_admitted": false,
                    "status": "continuation_not_scheduled",
                    "execution_required": false,
                    "run_id": selected_run_id,
                    "selected_attempt": { "run_id": run_id },
                    "candidate": record.metadata.get("candidate"),
                    "candidate_fingerprint": candidate_fingerprint,
                    "finalization": finalization,
                    "continuation_command": continuation_command(),
                    "continuation": {
                        "path": "finalization_receipt_replay",
                        "state": "Absent",
                        "provider_replay": false,
                        "provider_behavior": "not_replayed",
                    },
                    "execution_only_checks": [],
                    "phases": phases,
                    "side_effects": { "process_execution": false, "state_mutation": false, "provider_dispatch": false, "git_mutation": false, "git_index_mutation": false, "github_mutation": false, "finalization": false }
                }),
                0,
            ));
        }
        phases.push(serde_json::json!({
            "phase": "continuation_claim",
            "status": "passed",
            "reason": "ok",
            "state": format!("{continuation_state:?}"),
            "rearm": args.rearm,
        }));
        let dispatcher = match crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
            &recipe.promotion_transport["attempt_dispatch"],
        ) {
            Ok(dispatcher) => {
                phases.push(
                    serde_json::json!({ "phase": "transport", "status": "passed", "reason": "ok" }),
                );
                dispatcher
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Some(continuation_command()),
                        candidate_fingerprint,
                        phases,
                        "transport",
                        &error,
                    ),
                    1,
                ));
            }
        };
        if let Err(error) =
            agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)
        {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    Some(continuation_command()),
                    candidate_fingerprint,
                    phases,
                    "recipe",
                    &error,
                ),
                1,
            ));
        }
        return Ok(cook_finalization_receipt_preflight_report(
            selected_run_id,
            &run_id,
            continuation_command(),
            args.rearm,
            candidate_fingerprint,
            phases,
            finalization,
        ));
    }
    if !record.state().is_terminal() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "cook_or_attempt_id",
            "selected Cook attempt is not terminal and cannot be admitted to dispatch",
            Some(run_id.clone()),
            None,
        );
        return Ok((
            cook_continuation_preflight_report(
                selected_run_id,
                Some(continuation_command()),
                candidate_fingerprint,
                phases,
                "lifecycle",
                &error,
            ),
            1,
        ));
    }
    if matches!(
        record.state(),
        homeboy_lifecycle_contract::RunExecutionState::Succeeded
            | homeboy_lifecycle_contract::RunExecutionState::CandidateRecoverable
    ) {
        match agent_task_service::preflight_continuation_claim(&recipe.cook_id, &run_id, args.rearm)
        {
            Ok(state) => {
                continuation_reconciliation_required =
                    state == agent_task_service::CookContinuationState::Absent;
                phases.push(serde_json::json!({
                    "phase": "continuation_claim",
                    "status": if continuation_reconciliation_required { "pending" } else { "passed" },
                    "reason": if continuation_reconciliation_required { "authoritative_reconciliation_requires_execution" } else { "ok" },
                    "state": format!("{state:?}"),
                    "rearm": args.rearm,
                }));
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Some(continuation_command()),
                        candidate_fingerprint,
                        phases,
                        "continuation_claim",
                        &error,
                    ),
                    1,
                ));
            }
        }
    }
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .expect("continuation selection is recipe-bound");
    let local_override = match explicit_local_continuation_decision(&attempt.plan) {
        Ok(decision) => decision,
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    Some(continuation_command()),
                    candidate_fingerprint,
                    phases,
                    "transport",
                    &error,
                ),
                1,
            ))
        }
    };
    let dispatcher = match if local_override.is_some() {
        Ok(None)
    } else {
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
            &recipe.promotion_transport["attempt_dispatch"],
        )
    } {
        Ok(dispatcher) => {
            phases.push(
                serde_json::json!({ "phase": "transport", "status": "passed", "reason": "ok" }),
            );
            dispatcher
        }
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    Some(continuation_command()),
                    candidate_fingerprint,
                    phases,
                    "transport",
                    &error,
                ),
                1,
            ))
        }
    };
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .expect("continuation selection is recipe-bound");
    let mut options = match if local_override.is_some() {
        agent_task_service::reconstruct_options_without_dispatcher(&recipe)
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)
    } {
        Ok(options) => options,
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    Some(continuation_command()),
                    candidate_fingerprint,
                    phases,
                    "recipe",
                    &error,
                ),
                1,
            ))
        }
    };
    options.identity.initial_run_id = attempt.run_id.clone();
    options.identity.initial_plan = attempt.plan.clone();
    if let Some(decision) = local_override {
        if let Err(error) = apply_local_continuation_decision(&mut options, decision) {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    Some(continuation_command()),
                    candidate_fingerprint,
                    phases,
                    "transport",
                    &error,
                ),
                1,
            ));
        }
    }
    if let Some(finalization) = agent_task_service::recorded_finalization_receipt(&record.metadata)
        .filter(|finalization| !finalization.is_null())
    {
        phases.push(serde_json::json!({
            "phase": "finalization_receipt",
            "status": "passed",
            "reason": "authoritative_replay",
        }));
        return Ok(cook_finalization_receipt_preflight_report(
            selected_run_id,
            &run_id,
            continuation_command(),
            args.rearm,
            candidate_fingerprint,
            phases,
            finalization,
        ));
    }
    let mut execution_only_checks =
        match agent_task_service::preflight_cook_continuation_admission_for_observation(
            &options, &record,
        ) {
            Ok(checks) => checks.into_iter().map(str::to_string).collect::<Vec<_>>(),
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Some(continuation_command()),
                        candidate_fingerprint,
                        phases,
                        "continuation_admission",
                        &error,
                    ),
                    1,
                ));
            }
        };
    if continuation_reconciliation_required {
        execution_only_checks.push("continuation_reconciliation".to_string());
    }
    phases.push(serde_json::json!({ "phase": "continuation_admission", "status": "passed", "reason": "deterministic_checks_passed" }));
    let promotion_preflight =
        if agent_task_service::cook_continuation_requires_model_provenance(&record) {
            match agent_task_service::preflight_cook_promotion_for_observation_in_store(
                &lifecycle_store,
                &options,
                &run_id,
                None,
                &record,
                aggregate.as_ref(),
            ) {
                Ok(preflight) => {
                    candidate_fingerprint = preflight["candidate_fingerprint"].clone();
                    execution_only_checks.extend(
                        preflight["execution_only_checks"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_string),
                    );
                    preflight
                }
                Err(error) => {
                    return Ok((
                        cook_continuation_preflight_report(
                            selected_run_id,
                            Some(continuation_command()),
                            candidate_fingerprint,
                            phases,
                            "candidate_admission",
                            &error,
                        ),
                        1,
                    ));
                }
            }
        } else {
            serde_json::json!({
                "behavior": "not_started",
                "candidate_fingerprint": Value::Null,
                "execution_only_checks": ["provider_execution"],
            })
        };
    phases.push(
        serde_json::json!({ "phase": "candidate_admission", "status": "passed", "reason": "ok" }),
    );
    execution_only_checks.sort();
    execution_only_checks.dedup();
    let provider_replay = agent_task_service::cook_continuation_replays_provider(&record);
    if !execution_only_checks.is_empty() {
        phases.push(serde_json::json!({
            "phase": "execution_only",
            "status": "pending",
            "reason": "authoritative_checks_require_execution",
            "checks": execution_only_checks,
        }));
    }
    let execution_required = !execution_only_checks.is_empty();
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-continuation-preflight/v1",
            "admitted": true,
            "pre_dispatch_admitted": true,
            "status": "admitted",
            "execution_required": execution_required,
            "run_id": selected_run_id,
            "selected_attempt": { "run_id": run_id },
            "candidate": promotion_preflight.get("candidate"),
            "candidate_fingerprint": candidate_fingerprint,
            "promotion": promotion_preflight,
            "continuation_command": continuation_command(),
            "evidence_refs": [{
                "run_id": run_id,
                "ref": format!("homeboy://agent-task/run/{run_id}/evidence"),
            }],
            "continuation": {
                "path": "resume",
                "provider_replay": provider_replay,
                "provider_behavior": if provider_replay { "replay" } else { "not_replayed" }
            },
            "execution_only_checks": execution_only_checks,
            "phases": phases,
            "side_effects": { "process_execution": false, "state_mutation": false, "provider_dispatch": false, "git_mutation": false, "git_index_mutation": false, "github_mutation": false, "finalization": false }
        }),
        0,
    ))
}

pub(super) fn cook_finalization_receipt_preflight_report(
    selected_run_id: Option<String>,
    run_id: &str,
    continuation_command: String,
    _rearm: bool,
    candidate_fingerprint: Value,
    phases: Vec<Value>,
    finalization: &Value,
) -> (Value, i32) {
    let status = finalization["status"].as_str().unwrap_or("unknown");
    let replay_succeeded = matches!(
        status,
        "review_ready" | "draft_published" | "succeeded_partial"
    );
    (
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-continuation-preflight/v1",
            "admitted": replay_succeeded,
            "pre_dispatch_admitted": replay_succeeded,
            "status": status,
            "execution_required": false,
            "run_id": selected_run_id,
            "selected_attempt": { "run_id": run_id },
            "candidate_fingerprint": candidate_fingerprint,
            "promotion": {
                "behavior": "not_started",
                "reason": "authoritative_finalization_receipt",
                "candidate_fingerprint": Value::Null,
                "execution_only_checks": [],
            },
            "finalization": finalization,
            "continuation_command": continuation_command,
            "evidence_refs": [{
                "run_id": run_id,
                "ref": format!("homeboy://agent-task/run/{run_id}/evidence"),
            }],
            "continuation": {
                "path": "finalization_receipt_replay",
                "provider_replay": false,
                "provider_behavior": "not_replayed",
            },
            "execution_only_checks": [],
            "phases": phases,
            "side_effects": { "process_execution": false, "state_mutation": false, "provider_dispatch": false, "git_mutation": false, "git_index_mutation": false, "github_mutation": false, "finalization": false }
        }),
        i32::from(!replay_succeeded),
    )
}

pub(super) fn cook_continuation_preflight_report(
    selected_run_id: Option<String>,
    continuation_command: Option<String>,
    candidate_fingerprint: Value,
    mut phases: Vec<Value>,
    phase: &str,
    error: &homeboy::core::Error,
) -> Value {
    phases.push(serde_json::json!({ "phase": phase, "status": "blocked", "reason": format!("{:?}", error.code), "message": error.message }));
    let run_id = selected_run_id.clone();
    let continuation_command = continuation_command.or_else(|| {
        run_id
            .as_deref()
            .map(|run_id| agent_task_service::cook_recover_command(None, run_id))
    });
    let evidence_refs = run_id
        .as_deref()
        .map(|run_id| {
            serde_json::json!([{
                "run_id": run_id,
                "ref": format!("homeboy://agent-task/run/{run_id}/evidence"),
            }])
        })
        .unwrap_or_default();
    serde_json::json!({
        "schema": "homeboy/agent-task-cook-continuation-preflight/v1",
        "admitted": false,
        "pre_dispatch_admitted": false,
        "status": "blocked",
        "run_id": run_id,
        "selected_attempt": { "run_id": selected_run_id },
        "candidate_fingerprint": candidate_fingerprint,
        "continuation_command": continuation_command,
        "failure_context": {
            "diagnostic": {
                "code": format!("{:?}", error.code),
                "message": error.message,
                "details": error.details,
            },
            "next_action": { "command": continuation_command },
        },
        "evidence_refs": evidence_refs,
        "phases": phases,
        "execution_only_checks": [],
        "side_effects": { "process_execution": false, "state_mutation": false, "provider_dispatch": false, "git_mutation": false, "git_index_mutation": false, "github_mutation": false, "finalization": false }
    })
}

pub(super) fn cook_terminal_continuation_status(
    cook_id: &str,
    run_id: &str,
    provider_state: &str,
    state: agent_task_service::CookContinuationState,
) -> Value {
    let (status, guidance) = match state {
        agent_task_service::CookContinuationState::Pending => (
            "continuation_pending",
            serde_json::json!({
                "action": "await_continuation_claim",
                "command": agent_task_service::cook_recover_command(None, run_id),
            }),
        ),
        agent_task_service::CookContinuationState::Claimed => (
            "continuation_in_progress",
            serde_json::json!({
                "action": "await_claimed_continuation",
                "command": format!("homeboy agent-task status {run_id}"),
            }),
        ),
        agent_task_service::CookContinuationState::Failed => (
            "continuation_recovery_required",
            serde_json::json!({
                "action": "rearm_failed_continuation",
                "command": agent_task_service::cook_recover_command(None, run_id),
            }),
        ),
        agent_task_service::CookContinuationState::Completed => (
            "continuation_completed",
            serde_json::json!({
                "action": "inspect_completed_cook",
                "command": format!("homeboy agent-task status {run_id}"),
            }),
        ),
        // `agent-task recover` schedules the continuation its step names
        // before calling this handler; reconcile has nothing to apply here.
        agent_task_service::CookContinuationState::Absent => (
            "continuation_not_scheduled",
            serde_json::json!({
                "action": "schedule_continuation",
                "command": agent_task_service::cook_recover_command(None, run_id),
            }),
        ),
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": { "state": provider_state, "run_id": run_id },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

pub(super) fn cook_continuation_status(
    cook_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Value {
    let run_id = &record.run_id;
    let runner_id = record.runner_id();
    let waiting_for_capacity = record
        .metadata
        .pointer("/runner_queue/state")
        .and_then(Value::as_str)
        == Some("waiting_for_capacity");
    let runner_disconnected = record
        .metadata
        .get("runner_liveness")
        .and_then(Value::as_str)
        == Some("disconnected");
    let placement_authority = record
        .metadata
        .get("execution_placement_decision")
        .and_then(|decision| {
            let requested = decision.get("requested").and_then(Value::as_str);
            let runner_source = decision.pointer("/runner/source").and_then(Value::as_str);
            let operator_override = decision
                .pointer("/override_authorization/authorized")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(if operator_override {
                "operator_overridable"
            } else if runner_source == Some("explicit") {
                "operator_pinned"
            } else if requested == Some("auto") || runner_source == Some("policy") {
                "policy_pinned"
            } else {
                "recipe_pinned"
            })
        });
    let (status, guidance) = if runner_disconnected {
        (
            "recovery_required",
            serde_json::json!({
                "action": "reconcile_runner_authority",
                "command": format!("homeboy agent-task reconcile {run_id} --dry-run"),
                "message": "The runner could not be reached during bounded reconciliation; inspect the scoped recovery before retrying provider work."
            }),
        )
    } else if waiting_for_capacity
        || record.state() == homeboy_lifecycle_contract::RunExecutionState::Queued
    {
        let command = runner_id
            .map(|runner_id| format!("homeboy runner status {runner_id}"))
            .unwrap_or_else(|| format!("homeboy agent-task run {run_id}"));
        (
            "accepted_unscheduled",
            serde_json::json!({
                "action": if runner_id.is_some() { "await_runner_capacity" } else { "schedule_queued_run" },
                "command": command,
                "message": if runner_id.is_some() {
                    "The runner accepted this Cook and owns its FIFO queue entry; it will schedule provider execution when a capacity lease is available."
                } else {
                    "This Cook is durably queued but has no runner or provider boundary; schedule its queued run before expecting provider work."
                }
            }),
        )
    } else {
        (
            "observation_in_progress",
            serde_json::json!({
                "action": "watch_provider",
                "command": format!("homeboy runs logs {run_id}"),
                "message": "The runner reports active provider work; follow the durable observation until its terminal result is projected."
            }),
        )
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": {
            "state": format!("{:?}", record.state()),
            "run_id": run_id,
            "runner_id": runner_id,
            "runner_job_status": record.metadata.get("runner_job_status"),
        },
        "placement_authority": placement_authority,
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

pub(super) fn cook_report_with_continuation(mut value: Value) -> Value {
    if value.get("status").and_then(Value::as_str) != Some("in_flight") {
        return value;
    }
    let run_id = value
        .get("latest_run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_state = value
        .get("attempts")
        .and_then(Value::as_array)
        .and_then(|attempts| attempts.last())
        .and_then(|attempt| attempt.get("run_state"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Value::Object(report) = &mut value {
        report.insert(
            "provider".to_string(),
            serde_json::json!({ "state": provider_state, "run_id": run_id }),
        );
        report.insert(
            "remaining_phases".to_string(),
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"]),
        );
        report.insert(
            "continuation_command".to_string(),
            serde_json::json!(agent_task_service::cook_recover_command(None, &run_id)),
        );
    }
    value
}
