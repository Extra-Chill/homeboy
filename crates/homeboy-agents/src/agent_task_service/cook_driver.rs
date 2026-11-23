//! Cook runtime driver and durable error reporting.

use super::cook::*;

/// Process-local dependencies for one Cook invocation. Production callers must
/// bind every dependency explicitly; there are no environment-derived stores or
/// optional side-effect/observer fallbacks in the execution API.
pub struct CookRuntime<'a> {
    executor: SharedAgentTaskExecutor,
    store: &'a CookRecipeStore,
    lifecycle_store: &'a AgentTaskLifecycleStore,
    side_effects: CookSideEffects<'a>,
    durable_observer: &'a CookProgressObserver<'a>,
    replay_keeps_recorded_transport: bool,
}

impl<'a> CookRuntime<'a> {
    fn new(
        executor: SharedAgentTaskExecutor,
        store: &'a CookRecipeStore,
        lifecycle_store: &'a AgentTaskLifecycleStore,
        side_effects: CookSideEffects<'a>,
        durable_observer: &'a CookProgressObserver<'a>,
    ) -> Self {
        Self {
            executor,
            store,
            lifecycle_store,
            side_effects,
            durable_observer,
            replay_keeps_recorded_transport: false,
        }
    }

    /// Replaying an existing attempt keeps the attempt transport recorded in
    /// its recipe instead of resetting to a local placement override. Adoption
    /// and terminal continuation drive a detached attempt and set this.
    pub fn replay_keeping_recorded_transport(mut self) -> Self {
        self.replay_keeps_recorded_transport = true;
        self
    }

    /// Bind a caller-owned finalizer while retaining native promotion, recovery,
    /// and durable operation-claim behavior.
    pub fn with_finalizer<F>(
        executor: SharedAgentTaskExecutor,
        store: &'a CookRecipeStore,
        lifecycle_store: &'a AgentTaskLifecycleStore,
        finalize: F,
        durable_observer: &'a CookProgressObserver<'a>,
    ) -> Self
    where
        F: FnMut(
                &AgentTaskLifecycleStore,
                &CookRequest,
                &str,
                &AgentTaskPromotionReport,
            ) -> Result<Value>
            + 'a,
    {
        Self::new(
            executor,
            store,
            lifecycle_store,
            CookSideEffects::new(finalize),
            durable_observer,
        )
    }

    /// Explicit production wiring with the standard side-effect boundary and a
    /// no-op foreground observer. Store selection remains the caller's job.
    pub fn production(
        executor: SharedAgentTaskExecutor,
        store: &'a CookRecipeStore,
        lifecycle_store: &'a AgentTaskLifecycleStore,
    ) -> Self {
        Self::production_with_observer(
            executor,
            store,
            lifecycle_store,
            &noop_cook_progress_observer,
        )
    }

    pub fn production_with_observer(
        executor: SharedAgentTaskExecutor,
        store: &'a CookRecipeStore,
        lifecycle_store: &'a AgentTaskLifecycleStore,
        durable_observer: &'a CookProgressObserver<'a>,
    ) -> Self {
        Self::new(
            executor,
            store,
            lifecycle_store,
            CookSideEffects::new(move |lifecycle_store, options, run_id, promotion| {
                finalize_or_load_cook_pr_with_stores(
                    store,
                    lifecycle_store,
                    options,
                    run_id,
                    promotion,
                )
            }),
            durable_observer,
        )
    }
}

pub(super) fn noop_cook_progress_observer(_: &CookProgressEvent<'_>) -> Result<()> {
    Ok(())
}

// Tests may use a compact builder while asserting legacy durability cases. It
// is deliberately unavailable to production callers, which must supply a
// CookRuntime to CookService::run.
#[cfg(test)]
pub(crate) struct CookContext<'a> {
    pub options: CookRequest,
    pub executor: SharedAgentTaskExecutor,
    pub store: Option<&'a CookRecipeStore>,
    pub lifecycle_store: Option<&'a AgentTaskLifecycleStore>,
    pub side_effects: Option<CookSideEffects<'a>>,
    pub durable_observer: Option<&'a CookProgressObserver<'a>>,
    pub replay_keeps_recorded_transport: bool,
}

#[cfg(test)]
impl<'a> CookContext<'a> {
    pub(crate) fn new(options: CookRequest, executor: SharedAgentTaskExecutor) -> Self {
        Self {
            options,
            executor,
            store: None,
            lifecycle_store: None,
            side_effects: None,
            durable_observer: None,
            replay_keeps_recorded_transport: false,
        }
    }
}

#[cfg(test)]
pub(crate) fn run_cook(ctx: CookContext<'_>) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let store = match ctx.store {
        Some(store) => store.clone(),
        None => CookRecipeStore::from_current_data_root()?,
    };
    let lifecycle_store = match ctx.lifecycle_store {
        Some(store) => store.clone(),
        None => AgentTaskLifecycleStore::from_environment()?,
    };
    let mut side_effects = ctx.side_effects.unwrap_or_else(|| {
        CookSideEffects::new(|lifecycle_store, options, run_id, promotion| {
            finalize_or_load_cook_pr_with_stores(
                &store,
                lifecycle_store,
                options,
                run_id,
                promotion,
            )
        })
    });
    let observer = ctx.durable_observer.unwrap_or(&noop_cook_progress_observer);
    run_cook_with_runtime(
        ctx.options,
        ctx.executor,
        &store,
        &lifecycle_store,
        &mut side_effects,
        observer,
        ctx.replay_keeps_recorded_transport,
    )
}

/// Sole public Cook execution entrypoint.
pub struct CookService;

impl CookService {
    pub fn run(
        request: CookRequest,
        runtime: CookRuntime<'_>,
    ) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
        let CookRuntime {
            executor,
            store,
            lifecycle_store,
            mut side_effects,
            durable_observer,
            replay_keeps_recorded_transport,
        } = runtime;
        run_cook_with_runtime(
            request,
            executor,
            store,
            lifecycle_store,
            &mut side_effects,
            durable_observer,
            replay_keeps_recorded_transport,
        )
    }
}

pub(super) fn run_cook_with_runtime(
    mut options: CookRequest,
    executor: SharedAgentTaskExecutor,
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    side_effects: &mut CookSideEffects<'_>,
    durable_observer: &CookProgressObserver<'_>,
    replay_keeps_recorded_transport: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    // A repro also gates the candidate (#17653).
    options.gates.include_repro_in_candidate_gates();
    // One controller drives a Cook at a time (#15566). Held for the whole
    // run, released when this returns or the process dies.
    let Some(driver) = store.try_acquire_cook_driver(&options.identity.cook_id)? else {
        let driver = store
            .foreign_cook_driver(&options.identity.cook_id)?
            .unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "cook_id",
            format!(
                "Cook is already being driven by another live controller (pid {}); refusing to drive it concurrently",
                if driver.trim().is_empty() { "unknown" } else { driver.trim() }
            ),
            Some(options.identity.cook_id.clone()),
            Some(vec![format!(
                "Watch it with `homeboy agent-task status {}` instead of starting a second controller.",
                options.identity.cook_id
            )]),
        )
        .with_retryable(true));
    };
    if options
        .gates
        .gate_environment
        .admitted_component_id
        .is_none()
    {
        options.gates.gate_environment.admitted_component_id =
            cook_repository_identity_component_id(&options.identity.initial_plan);
    }
    let notification_options = options.clone();
    let timing_store = lifecycle_store
        .open_observation_initialized()
        .map_err(|error| {
            homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error);
        })
        .ok();
    let _execution = timing_store
        .as_ref()
        .map(|store| store.run_phase_guard(&options.identity.initial_run_id, "cook.execution"));
    let mut result = run_cook_reported(
        store,
        lifecycle_store,
        options,
        executor,
        side_effects,
        Some(durable_observer),
        replay_keeps_recorded_transport,
    );
    if let Ok(result) = &mut result {
        // A CI-deferred Cook resolves here, without an operator `recover`:
        // a settled receipt makes the report terminal and announced below.
        // Announce the CI wait before polling, so the submitting client can
        // release resources held only for provider work (the local dispatch
        // slot) while required checks run remotely.
        if result.value.status == homeboy_core::cook_status::CookStatus::AwaitingCi {
            if let Some(run_id) = result.value.latest_run_id.clone() {
                let attempt = result.value.attempts.len() as u32;
                if let Err(error) = super::cook::report_cook_progress(
                    lifecycle_store,
                    Some(durable_observer),
                    &result.value.cook_id,
                    &run_id,
                    super::cook_ci::AWAITING_CI_PHASE,
                    attempt,
                    Some("waiting for required PR checks"),
                ) {
                    homeboy_core::log_status!(
                        "agent-task",
                        "Cook CI wait progress failed: {}",
                        error
                    );
                }
            }
        }
        if let Err(error) = super::cook_ci::settle_awaiting_ci(
            store,
            &mut result.value,
            &mut result.exit_code,
            &homeboy_core::git::cook_ci::GithubCookCi,
            super::cook_ci::CookCiPollCadence::DEFAULT,
            &mut std::thread::sleep,
        ) {
            homeboy_core::log_status!("agent-task", "Cook CI polling failed: {}", error);
        }
        super::cook_promotion::bind_report_to_stores(
            &mut result.value,
            store,
            lifecycle_store,
            result.exit_code != 0,
        );
        if result.value.disposition.is_terminal() {
            if let Err(error) = finish_cook_terminal_with_report(
                lifecycle_store,
                &result.value.cook_id,
                Some((
                    &result.value,
                    cook_component(&notification_options).as_deref(),
                    result.exit_code,
                )),
                &[notification_options.identity.initial_run_id.clone()],
            ) {
                homeboy_core::log_status!(
                    "agent-task",
                    "Cook terminal persistence failed: {}",
                    error
                );
            }
        }
    }
    if result.is_err() {
        // This driver is the Cook's only settlement owner, and it is leaving
        // on an admission or execution error without a terminal report.
        // Release ownership first so settlement can observe that no
        // controller drives the Cook any more, then settle whatever already
        // ended. Without this, rows whose lifecycle terminated on the error
        // path stayed `running` forever.
        let cook_id = notification_options.identity.cook_id.clone();
        drop(driver);
        settle_after_driver_error(lifecycle_store, &cook_id);
    }
    result
}

/// Settle a Cook whose driver failed without a terminal report. The Cook-level
/// settlement acts only on a terminal, settled Cook observation; the per-row
/// backstop then settles rows whose own lifecycle ended, leaving any Cook that
/// still has live lifecycle work, or a new driver, untouched.
fn settle_after_driver_error(lifecycle: &AgentTaskLifecycleStore, cook_id: &str) {
    if let Err(error) = finish_cook_terminal(lifecycle, cook_id) {
        homeboy_core::log_status!(
            "agent-task",
            "Cook {} settlement after driver error failed: {}",
            cook_id,
            error
        );
    }
    if let Err(error) = super::unsettled_rows::settle_unowned_cook_rows(lifecycle, cook_id) {
        homeboy_core::log_status!(
            "agent-task",
            "Cook {} row settlement after driver error failed: {}",
            cook_id,
            error
        );
    }
}

/// Settle and announce a Cook only once its end-to-end outcome is terminal.
/// Safe for repeated delivery, including attempts that outlive their driver.
pub fn finish_cook_terminal(lifecycle: &AgentTaskLifecycleStore, cook_id: &str) -> Result<()> {
    finish_cook_terminal_with_report(lifecycle, cook_id, None, &[])
}

pub(super) fn finish_cook_terminal_with_report(
    lifecycle: &AgentTaskLifecycleStore,
    cook_id: &str,
    reported: Option<(&AgentTaskCookReport, Option<&str>, i32)>,
    source_ids: &[String],
) -> Result<()> {
    // A live driver's terminal report is the Cook's verdict. Without one,
    // the settled Cook observation decides whether the Cook has ended.
    let mut fallback;
    let (report, component, exit_code) = match reported {
        Some(reported) => reported,
        None => {
            let record = cook_observation_in_store(lifecycle, cook_id)?;
            use agent_task_lifecycle::cook_run_state::cook_settlement_in_store;
            let recipes = CookRecipeStore::new(lifecycle.roots().clone());
            let state = agent_task_lifecycle::CookRunState::of(&record);
            if !state.terminal || recipes.cook_driver_held(cook_id)? {
                return Ok(());
            }
            if !cook_settlement_in_store(&recipes, cook_id, &record.run_id)?
                .is_settled(&record, false)
            {
                return Ok(());
            }
            fallback = crate::agent_task_notify::terminal_attempt_report(
                cook_id,
                &record.run_id,
                super::cook_projections::terminal_fallback_status(state.execution_state),
            );
            fallback.stop_reason = record.metadata["pre_execution_failure"]["message"]
                .as_str()
                .or_else(|| record.metadata["execution_step_failure"]["message"].as_str())
                .or_else(|| record.metadata["unmaterialized_cook_admission"]["reason"].as_str())
                .map(str::to_string);
            fallback.report_stores = Some((
                CookRecipeStore::new(lifecycle.roots().clone()),
                lifecycle.clone(),
            ));
            let failed =
                state.execution_state != homeboy_lifecycle_contract::RunExecutionState::Succeeded;
            (&fallback, None, i32::from(failed))
        }
    };
    let mut ids = source_ids.to_vec();
    ids.extend(report.history_run_ids.iter().cloned());
    ids.extend(report.invocation_run_ids.iter().cloned());
    ids.extend(report.latest_run_id.iter().cloned());
    agent_task_lifecycle::settle_cook_terminal_observations_in_store(lifecycle, cook_id, &ids)?;
    crate::agent_task_notify::cook_terminal_in_store(lifecycle, report, component, exit_code);
    Ok(())
}

/// The component a cook is working on, for notification attribution.
///
/// Read from the plan's own component contract rather than parsed out of the
/// worktree handle, so a renamed or detached worktree cannot mislabel it.
pub(super) fn cook_component(options: &CookRequest) -> Option<String> {
    options
        .identity
        .initial_plan
        .component_contracts
        .iter()
        .chain(
            options
                .identity
                .initial_plan
                .tasks
                .iter()
                .flat_map(|task| task.component_contracts.iter()),
        )
        .find_map(|contract| contract.slug.clone())
}

pub(crate) fn exhausted_budget_guidance(
    max_attempts: u32,
    budget: &AgentTaskExecutionBudget,
    reason: &str,
) -> String {
    format!(
        "Cook stopped during gate or provider remediation because {reason} was exhausted. Configured budget: --max-attempts {}, --max-provider-executions {}, --max-same-provider-retries {}, provider rotations {}. Start a new Cook with `--max-attempts {} --max-provider-executions {} --max-same-provider-retries {}`; rotations cannot fund same-provider gate retries.",
        max_attempts,
        budget.max_provider_executions,
        budget.max_same_provider_retries,
        budget.max_provider_rotations,
        max_attempts,
        max_attempts,
        max_attempts.saturating_sub(1),
    )
}

/// Report the spine's outcome against explicit roots: convert a post-
/// materialization controller error into the durable Cook result contract, then
/// publish terminal progress for the attempt that produced it.
pub(super) fn run_cook_reported(
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: CookRequest,
    executor: SharedAgentTaskExecutor,
    side_effects: &mut CookSideEffects<'_>,
    durable_observer: Option<&CookProgressObserver<'_>>,
    replay_keeps_recorded_transport: bool,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let stores = CookStores {
        recipe: store,
        lifecycle: lifecycle_store,
    };
    let mut failure_options = options.clone();
    let mut result = match run_cook_spine(
        store,
        lifecycle_store,
        options,
        executor,
        side_effects,
        durable_observer,
        replay_keeps_recorded_transport,
    ) {
        Ok(result) => result,
        Err(mut error) => {
            if let Some(admitted) = error
                .details
                .get(super::cook_admission_key::ADMITTED_COOK_DETAIL)
                .cloned()
            {
                // The key's in-flight Cook is the answer: report its identity
                // and status instead of admitting another (#18450).
                let mut report = stores.report(CookReportInput {
                    cook_id: admitted["cook_id"].as_str().unwrap_or_default().to_string(),
                    status: admitted["status"]
                        .as_str()
                        .and_then(homeboy_core::cook_status::CookStatus::from_status)
                        .unwrap_or(homeboy_core::cook_status::CookStatus::InFlight),
                    disposition: CookDisposition::InFlight,
                    attempts: Vec::new(),
                    finalization: None,
                    stop_reason: Some(error.message.clone()),
                    exit_code: 0,
                    invocation_latest_run_id: admitted["run_id"].as_str(),
                });
                report.value.failure_context = None;
                return Ok(report);
            }
            if error.details["concurrent_cook_creation_loser"] == Value::Bool(true) {
                let mut report = stores.report(CookReportInput {
                        cook_id: failure_options.identity.cook_id.clone(),
                        status: homeboy_core::cook_status::CookStatus::DurableFailure,
                        disposition: CookDisposition::Terminal,
                        attempts: Vec::new(),
                        finalization: None,
                        stop_reason: Some(format!(
                            "another request already owns this Cook id; this request did not modify its durable plan: {}",
                            error.message
                        )),
                        exit_code: 1,
                        invocation_latest_run_id: None,
                    },
                );
                // The collided identifier belongs to the elected creator. A
                // loser may report the collision, but must not attach a
                // controller failure to, reconcile, or otherwise terminalize
                // the creator's lifecycle attempt.
                report.value.failure_context = None;
                return Ok(report);
            }
            let admission_run_id = error
                .details
                .as_object_mut()
                .and_then(|details| details.remove("cook_admission_run_id"))
                .and_then(|run_id| run_id.as_str().map(str::to_string));
            if let Some(run_id) = admission_run_id.as_deref() {
                if let Ok(recipe) = store.load_recipe(&failure_options.identity.cook_id) {
                    if let Some(attempt) = recipe
                        .attempts
                        .iter()
                        .find(|attempt| attempt.run_id == run_id)
                    {
                        failure_options.identity.initial_run_id = attempt.run_id.clone();
                        failure_options.identity.initial_plan = attempt.plan.clone();
                    }
                }
            }
            // Recipe persistence is the first half of Cook admission. If the
            // lifecycle row or index write failed transiently, complete the
            // same immutable attempt now rather than returning a partial Cook
            // whose advertised continuation has no record to address (#10849).
            //
            // Admission is incomplete only when the lifecycle row or the Cook
            // index is missing. Recovering unconditionally would also rewrite a
            // later controller-side rejection — a managed workspace that is no
            // longer active, say — whose durable failure is the correct answer.
            let admission_incomplete = !lifecycle_store
                .record_exists(&failure_options.identity.initial_run_id)
                .unwrap_or(false)
                || lifecycle_store
                    .read_cook_index(&failure_options.identity.cook_id)
                    .is_err();
            if admission_incomplete
                && store.recipe_exists(&failure_options.identity.cook_id)
                && super::cook_pre_execution::recover_recipe_attempt_with_stores(
                    store,
                    lifecycle_store,
                    &failure_options.identity.initial_run_id,
                )
                .ok()
                .flatten()
                .is_some()
            {
                error.details["cook_materialized_by_invocation"] = Value::Bool(true);
            }
            // Once the attempt exists, a controller-side validation failure has
            // not reached a provider and must retain the pre-execution contract.
            if let Ok(mut record) =
                lifecycle_store.read_record(&failure_options.identity.initial_run_id)
            {
                if store.data_root() == lifecycle_store.data_root()
                    && record.state() == homeboy_lifecycle_contract::RunExecutionState::Queued
                    && record.plan_id == failure_options.identity.initial_plan.plan_id
                {
                    let phase = pre_execution_failure_phase(&error, None);
                    record_pre_execution_failure(
                        lifecycle_store,
                        &failure_options.identity.initial_plan,
                        &failure_options.identity.initial_run_id,
                        &error,
                        phase,
                    )?;
                    record =
                        lifecycle_store.read_record(&failure_options.identity.initial_run_id)?;
                }
                if record.metadata.get("pre_execution_failure").is_some() {
                    return Ok(pre_execution_failure_report(
                        stores,
                        failure_options.identity.cook_id.clone(),
                        vec![AgentTaskCookAttemptReport {
                            attempt: 1,
                            run_id: failure_options.identity.initial_run_id.clone(),
                            run_state: format!("{:?}", record.state()),
                            aggregate_path: record.aggregate_path.clone(),
                            promotion: None,
                            feedback: None,
                        }],
                        pre_execution_failure_details(Some(&record), &error),
                        error,
                        Some(&failure_options.identity.initial_run_id),
                    ));
                }
            }
            return durable_cook_error_report_with_store(
                store,
                lifecycle_store,
                &failure_options,
                error,
            );
        }
    };
    // Recompute every durable projection before terminal progress is observed.
    // The spine can construct reports in legacy helpers, but its caller owns
    // these roots and terminal observers must never receive ambient evidence.
    super::cook_promotion::bind_report_to_stores(
        &mut result.value,
        store,
        lifecycle_store,
        result.exit_code != 0,
    );
    if let Some(run_id) = result.value.latest_run_id.as_deref() {
        let attempt = result
            .value
            .attempts
            .last()
            .map(|attempt| attempt.attempt)
            .unwrap_or(1);
        let phase = result.value.disposition.phase();
        if let Err(error) = report_cook_progress_with_activity(
            lifecycle_store,
            durable_observer,
            &result.value.cook_id,
            run_id,
            phase,
            attempt,
            Some(result.value.status.as_str()),
            None,
            (phase == "terminal").then_some(result.exit_code == 0),
            (phase == "terminal")
                .then(|| {
                    result.value.failure_context.as_ref().and_then(|context| {
                        context
                            .legal_actions
                            .iter()
                            .find(|action| action.action == "retry")
                            .map(|action| action.command.as_str())
                    })
                })
                .flatten(),
        ) {
            return durable_cook_error_report_with_store(
                store,
                lifecycle_store,
                &failure_options,
                error,
            );
        }
        if phase == "terminal" {
            if let Err(error) = agent_task_lifecycle::record_cook_terminal_result_in_store(
                lifecycle_store,
                run_id,
                result.value.status.as_str(),
                result.exit_code == 0,
                result.exit_code,
            )
            .and_then(|_| store.settle_continuations_driven_inline(&result.value.cook_id))
            {
                return durable_cook_error_report_with_store(
                    store,
                    lifecycle_store,
                    &failure_options,
                    error,
                );
            }
        }
    }
    Ok(result)
}

/// Convert an error that occurs after recipe materialization into the normal
/// Cook result contract. Errors before materialization still return unchanged:
/// they have no durable identity and therefore no legal recovery command.
#[cfg(test)]
pub(super) fn durable_cook_error_report(
    options: &CookRequest,
    error: Error,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let store = CookRecipeStore::from_current_data_root()?;
    durable_cook_error_report_with_store(&store, &store.lifecycle_store(), options, error)
}

/// The durable failure report, bound to the roots its caller owns.
///
/// This function reads the recipe from an
/// injected `CookRecipeStore` and used to write the controller failure through
/// ambient `agent_task_lifecycle` shims, so a Cook whose recipe lives in an
/// injected home had its failure diagnostic recorded in whatever home the
/// environment pointed at — and nothing failed while doing it: the report still
/// returned, the recipe still read back correct, and only the cause went
/// missing. That is the split `KNOWN_MIXED_STORE_FUNCTIONS` was written for; it
/// stayed invisible to the scanner because it reached through wrappers instead
/// of naming a store constructor (#7505).
pub(super) fn durable_cook_error_report_with_store(
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    error: Error,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let stores = CookStores {
        recipe: store,
        lifecycle: lifecycle_store,
    };
    // The attempt record is presentation only — a missing one degrades to an
    // empty attempts list, exactly as the ambient read's `.ok()` already did.
    let attempt_record = |run_id: &str| {
        agent_task_lifecycle::reconcile_status_in_store(
            lifecycle_store,
            run_id,
            agent_task_lifecycle::AgentTaskStatusOptions::default(),
            false,
        )
        .ok()
        .map(|outcome| outcome.record)
    };
    if store.recipe_exists(&options.identity.cook_id) {
        let attempts = store
            .load_recipe(&options.identity.cook_id)
            .ok()
            .and_then(|recipe| recipe.attempts.last().cloned())
            .and_then(|attempt| {
                attempt_record(&attempt.run_id).map(|record| AgentTaskCookAttemptReport {
                    attempt: attempt.attempt,
                    run_id: attempt.run_id,
                    run_state: format!("{:?}", record.state()),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                })
            })
            .into_iter()
            .collect();
        let mut report = stores.report(CookReportInput {
            cook_id: options.identity.cook_id.clone(),
            status: homeboy_core::cook_status::CookStatus::DurableFailure,
            disposition: CookDisposition::Terminal,
            attempts,
            finalization: None,
            stop_reason: Some(
                "Cook stopped after durable creation; use the recovery actions in \
                 failure_context to inspect or continue it."
                    .to_string(),
            ),
            exit_code: 1,
            // A controller failure still belongs to THIS invocation. Without
            // this, the report — and every recovery command
            // `cook_failure_context` stamps with it — falls back to the
            // cross-invocation Cook index and can name a prior session's run.
            invocation_latest_run_id: Some(&options.identity.initial_run_id),
        });
        // Failure-context projection can fail (for example while reading runner-only
        // evidence). Recording the cause must not depend on that projection.
        let diagnostic = bounded_error_diagnostic(&error);
        if agent_task_lifecycle::run_record_exists_in_store(
            lifecycle_store,
            &options.identity.initial_run_id,
        )? {
            agent_task_lifecycle::record_cook_controller_failure_in_store(
                lifecycle_store,
                &options.identity.initial_run_id,
                &diagnostic,
            )?;
        }
        if let Some(context) = &mut report.value.failure_context {
            // This is a controller failure, not a provider attempt. Keep a
            // bounded, redacted cause so continuation never needs redispatch.
            context.phase = "controller".to_string();
            context.reason_code = error.code.as_str().to_string();
            context.diagnostic = Some(bounded_error_diagnostic(&error));
        }
        return Ok(report);
    }
    Err(error)
}

/// [`agent_task_lifecycle::reconcile_status`] against an explicitly injected lifecycle
/// root.
///
/// The ambient `status()` is exactly this call with a store resolved from the
/// process environment, so the two agree on everything except which
/// installation they read — which is the only difference that matters on a
/// spine whose caller chose its own roots.
///
/// One asymmetry is worth naming rather than hiding: `reconcile_status_in_store` derives
/// its recipe store from `lifecycle_store.data_root()`, so a caller that passes
/// a recipe store rooted somewhere else still gets a recipe view derived from
/// the lifecycle root. That is not made worse here — the ambient form derived it
/// from the ambient root, which agreed with neither — and closing it means
/// giving `reconcile_status_in_store` both roots, which is its own change.
pub(super) fn rooted_status(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    Ok(agent_task_lifecycle::reconcile_status_in_store(
        lifecycle_store,
        run_id,
        agent_task_lifecycle::AgentTaskStatusOptions::default(),
        true,
    )?
    .record)
}
