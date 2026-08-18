//! Cook attempt materialization, pre-execution failure classification, and
//! terminal-executor identity.
//!
//! Extracted from `cook.rs`: `materialize_initial_cook_attempt` (durable first
//! attempt submission), the pre-execution failure boundary
//! (`retryable_pre_execution_failure`/`with_pre_execution_phase`/
//! `pre_execution_failure_*`/`record_pre_execution_failure`) that keeps a
//! lifecycle-owned failure distinct from a provider result, and the
//! terminal-executor identity helpers (`terminal_executor_matches`/
//! `provider_rotation_attempts`/`terminal_executor_identity`) used to decide
//! follow-up rotation. These are attempt-setup and terminal-classification
//! helpers around the `run_cook` loop; grouping them out of the loop keeps the
//! spine focused. This is one of the clusters the recent cook-retry fixes grew.

use serde_json::Value;

use crate::agent_task::AgentTaskExecutor;
use crate::agent_task_lifecycle::{self, AgentTaskLifecycleStore};
use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::cook_status::CookDisposition;
use homeboy_core::{Error, Result};

use super::cook::{
    AgentTaskCookAttemptDispatcher, AgentTaskCookAttemptReport, AgentTaskCookReport,
    AgentTaskCookServiceOptions,
};
use super::cook_promotion::{cook_report, CookReportInput};
use super::cook_recipe::CookRecipeStore;
use super::AgentTaskRunResult;

type AdmissionStatus<'a> = Option<&'a dyn Fn(&str) -> Option<Value>>;

/// Durable preparation boundary for one Cook execution attempt.
///
/// Recipe and lifecycle authority travel together so callers cannot
/// materialize a recipe in one root while publishing its run in another.
pub(crate) struct CookExecutionPreparation<'a> {
    recipe_store: &'a CookRecipeStore,
    lifecycle_store: &'a AgentTaskLifecycleStore,
}

impl<'a> CookExecutionPreparation<'a> {
    pub(crate) fn new(
        recipe_store: &'a CookRecipeStore,
        lifecycle_store: &'a AgentTaskLifecycleStore,
    ) -> Self {
        Self {
            recipe_store,
            lifecycle_store,
        }
    }

    pub(crate) fn materialize_with_admission(
        &self,
        cook_id: &str,
        run_id: &str,
        plan: &AgentTaskPlan,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        self.materialize_with_runtime(
            cook_id,
            run_id,
            plan,
            None,
            None,
            admit_runtime,
            reconcile_reserved_cancellation,
        )
    }

    fn materialize_with_runtime(
        &self,
        cook_id: &str,
        run_id: &str,
        plan: &AgentTaskPlan,
        admission_status: AdmissionStatus<'_>,
        execution_runner_id: Option<String>,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        materialize_cook_attempt_with_stores_and_runtime(
            self.recipe_store,
            self.lifecycle_store,
            cook_id,
            run_id,
            plan,
            admission_status,
            execution_runner_id,
            admit_runtime,
            reconcile_reserved_cancellation,
        )
    }

    pub(crate) fn record_pre_execution_failure(
        &self,
        cook_id: &str,
        run_id: &str,
        phase: &str,
        error: &Error,
    ) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
        record_materialized_cook_pre_execution_failure(
            self.recipe_store,
            self.lifecycle_store,
            cook_id,
            run_id,
            phase,
            error,
        )
    }

    pub(crate) fn recover_with_admission(
        &self,
        cook_or_attempt_id: &str,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<Option<agent_task_lifecycle::AgentTaskRunRecord>> {
        self.recover_with_runtime(
            cook_or_attempt_id,
            None,
            None,
            admit_runtime,
            reconcile_reserved_cancellation,
        )
    }

    pub(crate) fn recover_for_adoption_with_admission(
        &self,
        cook_id: &str,
        run_id: &str,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
        self.recover_for_adoption_with_runtime(
            cook_id,
            run_id,
            None,
            None,
            admit_runtime,
            reconcile_reserved_cancellation,
        )
    }

    pub(crate) fn recover_for_adoption_with_runtime(
        &self,
        cook_id: &str,
        run_id: &str,
        admission_status: AdmissionStatus<'_>,
        execution_runner_id: Option<String>,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
        self.recover_with_runtime(
            run_id,
            admission_status,
            execution_runner_id,
            admit_runtime,
            reconcile_reserved_cancellation,
        )?
        .ok_or_else(|| Error::internal_unexpected("Cook adoption recovery returned no record"))?;
        self.record_pre_execution_failure(
            cook_id,
            run_id,
            "transport_dispatcher_prepare",
            &Error::internal_unexpected(
                "recovered orphaned durable cook recipe before provider dispatch".to_string(),
            ),
        )
    }

    fn recover_with_runtime(
        &self,
        cook_or_attempt_id: &str,
        admission_status: AdmissionStatus<'_>,
        execution_runner_id: Option<String>,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
        reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
    ) -> Result<Option<agent_task_lifecycle::AgentTaskRunRecord>> {
        let recipe = match self.recipe_store.load_recipe(cook_or_attempt_id) {
            Ok(recipe) => recipe,
            Err(recipe_error) => match self
                .recipe_store
                .load_recipe_for_attempt(cook_or_attempt_id)?
            {
                Some(recipe) => recipe,
                None => return Err(recipe_error),
            },
        };
        let run_id = if recipe.cook_id == cook_or_attempt_id {
            recipe
                .attempts
                .last()
                .expect("validated recipe has an attempt")
                .run_id
                .clone()
        } else {
            cook_or_attempt_id.to_string()
        };
        let attempt = recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == run_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "cook_or_attempt_id",
                    "requested run is absent from its immutable Cook recipe",
                    Some(run_id.clone()),
                    None,
                )
            })?;

        self.materialize_with_runtime(
            &recipe.cook_id,
            &attempt.run_id,
            &attempt.plan,
            admission_status,
            execution_runner_id,
            admit_runtime,
            reconcile_reserved_cancellation,
        )?;
        let record = self.lifecycle_store.read_record(&attempt.run_id)?;
        let controller_plan = self.lifecycle_store.read_controller_plan(&attempt.run_id)?;
        super::cook_recipe::validate_recipe_attempt_record_with_controller_plan(
            &recipe,
            &attempt.run_id,
            &record,
            &controller_plan,
        )?;
        Ok(Some(record))
    }
}

/// Persist the controller-owned initial attempt before transport preparation so
/// runner eligibility failures remain addressable through the cook alias.
pub(crate) fn materialize_initial_cook_attempt(
    options: &AgentTaskCookServiceOptions,
) -> Result<()> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    materialize_initial_cook_attempt_with_stores(&recipe_store, &lifecycle_store, options)
}

/// Persist the controller-owned initial attempt through explicit recipe and
/// lifecycle stores so the run record and Cook index always land beside the
/// recipe's authority instead of the ambient environment.
pub(crate) fn materialize_initial_cook_attempt_with_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
) -> Result<()> {
    materialize_initial_cook_attempt_with_store_and_lifecycle(
        recipe_store,
        lifecycle_store,
        options,
    )
    .map(|_| ())
}

pub(crate) fn materialize_initial_cook_attempt_with_stores_outcome(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
) -> Result<bool> {
    materialize_initial_cook_attempt_with_store_and_lifecycle(
        recipe_store,
        lifecycle_store,
        options,
    )
}

fn materialize_initial_cook_attempt_with_store_and_lifecycle(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &AgentTaskCookServiceOptions,
) -> Result<bool> {
    let created = !recipe_store.recipe_exists(&options.cook_id)
        && !lifecycle_store.record_exists(&options.initial_run_id)?;
    CookExecutionPreparation::new(recipe_store, lifecycle_store).materialize_with_runtime(
        &options.cook_id,
        &options.initial_run_id,
        &options.initial_plan,
        Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
        agent_task_lifecycle::execution_runner_id(),
        production_runtime_admission(lifecycle_store),
        |cook_id| reconcile_reserved_cancellation_in_store(lifecycle_store, cook_id),
    )?;
    Ok(created)
}

/// Complete recipe, run-record, and index registration for an attempt. Each
/// write is independently durable, so replay must repair whichever suffix of
/// the sequence was interrupted.
pub(crate) fn materialize_cook_attempt(
    cook_id: &str,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<()> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    materialize_cook_attempt_with_stores(&recipe_store, &lifecycle_store, cook_id, run_id, plan)
}

/// Complete recipe, run-record, and index registration through explicit recipe
/// and lifecycle stores so the record and Cook index always land beside the
/// recipe's authority instead of the ambient environment.
pub(crate) fn materialize_cook_attempt_with_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<()> {
    materialize_cook_attempt_with_stores_and_runtime(
        recipe_store,
        lifecycle_store,
        cook_id,
        run_id,
        plan,
        Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
        agent_task_lifecycle::execution_runner_id(),
        production_runtime_admission(lifecycle_store),
        |cook_id| reconcile_reserved_cancellation_in_store(lifecycle_store, cook_id),
    )
}

fn materialize_cook_attempt_with_stores_and_runtime(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
    plan: &AgentTaskPlan,
    admission_status: AdmissionStatus<'_>,
    execution_runner_id: Option<String>,
    admit_runtime: impl FnOnce(&str) -> Result<Value>,
    reconcile_reserved_cancellation: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if !lifecycle_store.record_exists(run_id)? {
        agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
            lifecycle_store,
            cook_id,
            run_id,
        )?;
        let submission = match admission_status {
            Some(project) => lifecycle_store.submit_plan_with_runtime_admission_status(
                plan,
                run_id,
                execution_runner_id,
                project,
                admit_runtime,
            ),
            None => lifecycle_store.submit_plan_with_runtime_admission(plan, run_id, admit_runtime),
        };
        if let Err(error) = submission {
            // `submit_plan` persists admission failures before returning them.
            if lifecycle_store.record_exists(run_id)? {
                ensure_cook_attempt_index(recipe_store, lifecycle_store, cook_id, run_id)?;
            }
            return Err(error);
        }
    }
    ensure_cook_attempt_index(recipe_store, lifecycle_store, cook_id, run_id)?;
    // If cancellation won while this first attempt was being submitted, index
    // it before cancelling so the durable child remains reachable by Cook ID.
    reconcile_reserved_cancellation(cook_id)?;
    Ok(())
}

pub(crate) fn reconcile_reserved_cancellation(cook_id: &str) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    reconcile_reserved_cancellation_in_store(&lifecycle_store, cook_id)
}

pub(crate) fn reconcile_reserved_cancellation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<()> {
    lifecycle_store
        .cancel_reserved_detached_cook_handoff_attempt_if_cancelled(cook_id)
        .map(|_| ())
}

pub(crate) fn production_runtime_admission(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> impl FnOnce(&str) -> Result<Value> + '_ {
    move |run_id| {
        homeboy_core::controller_runtime::admit_current_for_with_cancellation_check(run_id, || {
            Ok(lifecycle_store.read_record(run_id)?.state.is_terminal())
        })
        .map(|admission| admission.runtime)
    }
}

/// Complete the second half of initial-attempt materialization after a crash.
/// The recipe and run record are independent durable writes, so their exact
/// identities must agree before repairing the alias/index projection.
fn ensure_cook_attempt_index(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<()> {
    let recipe = recipe_store.load_recipe(cook_id)?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable cook recipe does not declare the run id",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let record = lifecycle_store.read_record(run_id)?;
    if let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) {
        if cook_id != recipe.cook_id {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                "durable run record belongs to a different Cook",
                Some(run_id.to_string()),
                None,
            ));
        }
    }
    if let Some(recorded_attempt) = record.metadata.get("cook_attempt").and_then(Value::as_u64) {
        if recorded_attempt != u64::from(attempt.attempt) {
            return Err(Error::validation_invalid_argument(
                "cook_attempt",
                "durable run record belongs to a different Cook attempt",
                Some(run_id.to_string()),
                None,
            ));
        }
    }
    let controller_plan = lifecycle_store.read_controller_plan(run_id)?;
    super::cook_recipe::validate_recipe_attempt_record_with_controller_plan(
        &recipe,
        &attempt.run_id,
        &record,
        &controller_plan,
    )?;
    lifecycle_store
        .record_cook_attempt(&recipe.cook_id, attempt.attempt, &attempt.run_id)
        .map(|_| ())
}

/// Record a failure for an already materialized Cook attempt using its exact
/// recipe and lifecycle roots.
///
/// `agent_task_lifecycle::status` is reconciliation, not a pure rooted read,
/// so it is intentionally outside this exact-store seam.
fn record_materialized_cook_pre_execution_failure(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    error: &Error,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    let recipe = recipe_store.load_recipe(cook_id)?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable cook recipe does not declare the run id",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let record = lifecycle_store.read_record(run_id)?;
    let controller_plan = lifecycle_store.read_controller_plan(run_id)?;
    super::cook_recipe::validate_recipe_attempt_record_with_controller_plan(
        &recipe,
        run_id,
        &record,
        &controller_plan,
    )?;
    lifecycle_store.record_pre_execution_failure(run_id, &attempt.plan, phase, error)
}

/// Recover the latest immutable recipe attempt into its lifecycle record and
/// Cook index. This controller-only saga never prepares or dispatches a
/// provider; it only makes a recipe that survived an interrupted initial write
/// status-addressable again.
pub fn recover_recipe_attempt(
    cook_or_attempt_id: &str,
) -> Result<Option<agent_task_lifecycle::AgentTaskRunRecord>> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    recover_recipe_attempt_with_stores(&recipe_store, &lifecycle_store, cook_or_attempt_id)
}

/// Recover the latest immutable recipe attempt through explicit recipe and
/// lifecycle stores so the recovered record always lands beside the recipe's
/// authority instead of the ambient environment.
pub(crate) fn recover_recipe_attempt_with_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_or_attempt_id: &str,
) -> Result<Option<agent_task_lifecycle::AgentTaskRunRecord>> {
    CookExecutionPreparation::new(recipe_store, lifecycle_store).recover_with_runtime(
        cook_or_attempt_id,
        Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
        agent_task_lifecycle::execution_runner_id(),
        production_runtime_admission(lifecycle_store),
        |cook_id| reconcile_reserved_cancellation_in_store(lifecycle_store, cook_id),
    )
}

pub(crate) fn recover_adoption_attempt(
    cook_id: &str,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    recover_adoption_attempt_with_stores(&recipe_store, &lifecycle_store, cook_id, run_id)
}

/// Recover an adopted attempt through explicit recipe and lifecycle stores so
/// the adoption record always lands beside the recipe's authority instead of
/// the ambient environment.
pub(crate) fn recover_adoption_attempt_with_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    CookExecutionPreparation::new(recipe_store, lifecycle_store).recover_for_adoption_with_runtime(
        cook_id,
        run_id,
        Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
        agent_task_lifecycle::execution_runner_id(),
        production_runtime_admission(lifecycle_store),
        |cook_id| reconcile_reserved_cancellation_in_store(lifecycle_store, cook_id),
    )
}

pub(crate) fn retryable_pre_execution_failure(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    record.metadata["pre_execution_failure"]["retryable"] == Value::Bool(true)
}

#[derive(Debug)]
pub(crate) struct PreExecutionFailureDetails {
    pub(crate) retryable: bool,
    pub(crate) phase: Option<String>,
    pub(crate) classification: Option<String>,
}

pub(crate) fn with_pre_execution_phase(mut error: Error, phase: &str) -> Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["pre_execution_phase"] = Value::String(phase.to_string());
    error
}

pub(crate) fn pre_execution_failure_phase<'a>(
    error: &'a Error,
    dispatcher: Option<&dyn AgentTaskCookAttemptDispatcher>,
) -> &'a str {
    error
        .details
        .get("pre_execution_phase")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            dispatcher
                .map(|dispatcher| dispatcher.pre_execution_failure_phase())
                .unwrap_or("cook_pre_execution")
        })
}

pub(crate) fn pre_execution_failure_details(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    error: &Error,
) -> PreExecutionFailureDetails {
    let failure = record.and_then(|record| record.metadata.get("pre_execution_failure"));
    PreExecutionFailureDetails {
        retryable: failure
            .and_then(|failure| failure.get("retryable"))
            .and_then(Value::as_bool)
            .unwrap_or(error.retryable == Some(true)),
        phase: failure
            .and_then(|failure| failure.get("phase"))
            .and_then(Value::as_str)
            .map(str::to_string),
        classification: failure
            .and_then(|failure| failure.get("failure_classification"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

pub(crate) fn pre_execution_failure_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    failure: PreExecutionFailureDetails,
    error: Error,
    invocation_latest_run_id: Option<&str>,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let phase = failure.phase.as_deref().unwrap_or("cook_pre_execution");
    let classification = failure.classification.as_deref().unwrap_or("unknown");
    let mut report = cook_report(CookReportInput {
        cook_id,
        status: "pre_execution_failure",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some(format!(
            "pre-provider failure in phase `{phase}` classified as `{classification}`: {error}"
        )),
        exit_code: 1,
        invocation_latest_run_id,
    });
    report.value.terminal_phase = failure.phase;
    report.value.terminal_failure_classification = failure.classification;
    report
}

/// Pre-execution failures happen before a provider can receive work. Persist a
/// normal terminal run so the Cook alias can expose its complete retry history.
pub(crate) fn record_pre_execution_failure(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    run_id: &str,
    error: &Error,
    phase: &str,
) -> Result<()> {
    if !lifecycle_store.record_exists(run_id)? {
        lifecycle_store.submit_plan_with_current_runtime(plan, run_id)?;
    }
    lifecycle_store.record_pre_execution_failure(run_id, plan, phase, error)?;
    Ok(())
}

pub(crate) fn terminal_executor_matches(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
    plan: &AgentTaskPlan,
    durable_provider_executions: Option<&Value>,
    follow_up: &AgentTaskExecutor,
) -> Option<bool> {
    let outcome = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    })?;
    let terminal = terminal_executor_identity(outcome, plan, durable_provider_executions)?;
    Some(
        terminal.backend == follow_up.backend
            && terminal.selector == follow_up.selector
            && terminal.model.as_deref() == follow_up.model(),
    )
}

pub(crate) fn provider_rotation_attempts(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<Vec<crate::agent_task_scheduler::AgentTaskProviderRotationAttempt>> {
    serde_json::from_value(
        outcome
            .metadata
            .pointer("/provider_rotation/attempts")?
            .clone(),
    )
    .ok()
}

pub(crate) struct TerminalExecutorIdentity {
    pub(crate) backend: String,
    pub(crate) selector: Option<String>,
    pub(crate) model: Option<String>,
}

pub(crate) fn terminal_executor_identity(
    outcome: &crate::agent_task::AgentTaskOutcome,
    plan: &AgentTaskPlan,
    durable_provider_executions: Option<&Value>,
) -> Option<TerminalExecutorIdentity> {
    // Rotation evidence is the only persisted source with all three executor
    // fields after a provider swap. A durable execution ledger, when present,
    // must corroborate its backend/model rather than silently selecting the
    // initial plan executor.
    if outcome
        .metadata
        .pointer("/provider_rotation/attempts")
        .is_some()
    {
        let attempt = provider_rotation_attempts(outcome)?.last()?.clone();
        let terminal = TerminalExecutorIdentity {
            backend: attempt.backend,
            selector: attempt.selector,
            model: attempt
                .candidate_producing_model
                .or(attempt.attempted_model)
                .or(attempt.model),
        };
        if durable_provider_executions.is_some() {
            let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
            return (durable.backend == terminal.backend && durable.model == terminal.model)
                .then_some(terminal);
        }
        return Some(terminal);
    }

    // Preserve the normalized outcome identity used before durable execution
    // evidence was introduced. When both sources exist they must agree.
    if let Some(executor) = outcome.metadata.get("executor") {
        let terminal = TerminalExecutorIdentity {
            backend: executor.get("backend")?.as_str()?.to_string(),
            selector: executor
                .get("selector")
                .and_then(Value::as_str)
                .map(str::to_string),
            model: executor
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        if durable_provider_executions.is_some() {
            let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
            return (durable.backend == terminal.backend && durable.model == terminal.model)
                .then_some(terminal);
        }
        return Some(terminal);
    }

    // Without rotation, the source task is authoritative for the selector only
    // if the durable terminal execution proves it remained the executor.
    let task = plan
        .tasks
        .iter()
        .find(|task| task.task_id == outcome.task_id)?;
    let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
    (durable.backend == task.executor.backend && durable.model.as_deref() == task.executor.model())
        .then_some(TerminalExecutorIdentity {
            backend: task.executor.backend.clone(),
            selector: task.executor.selector.clone(),
            model: task.executor.model().map(str::to_string),
        })
}

struct DurableTerminalExecution {
    backend: String,
    model: Option<String>,
}

fn terminal_provider_execution(
    outcome: &crate::agent_task::AgentTaskOutcome,
    durable_provider_executions: Option<&Value>,
) -> Option<DurableTerminalExecution> {
    let executions = durable_provider_executions?.as_array()?;
    let terminal_attempt = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && matches!(
                    execution["state"].as_str(),
                    Some(
                        "succeeded"
                            | "failed"
                            | "cancelled"
                            | "timed_out"
                            | "candidate_recoverable"
                    )
                )
        })
        .filter_map(|execution| {
            execution["attempt"]
                .as_u64()
                .map(|attempt| (attempt, execution))
        })
        .max_by_key(|(attempt, _)| *attempt)?
        .0;
    let identities: Vec<_> = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && execution["attempt"].as_u64() == Some(terminal_attempt)
                && matches!(
                    execution["state"].as_str(),
                    Some(
                        "succeeded"
                            | "failed"
                            | "cancelled"
                            | "timed_out"
                            | "candidate_recoverable"
                    )
                )
        })
        .map(|execution| {
            Some(DurableTerminalExecution {
                backend: execution["backend"].as_str()?.to_string(),
                model: execution["model"].as_str().map(str::to_string),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let terminal = identities.first()?;
    identities
        .iter()
        .all(|identity| identity.backend == terminal.backend && identity.model == terminal.model)
        .then(|| DurableTerminalExecution {
            backend: terminal.backend.clone(),
            model: terminal.model.clone(),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_lifecycle::AgentTaskLifecycleStore;
    use crate::agent_task_service::cook_recipe::{
        AgentTaskCookRecipe, AgentTaskCookRecipeAttempt, COOK_RECIPE_SCHEMA,
    };

    fn recipe(cook_id: &str, run_id: &str, plan: AgentTaskPlan) -> AgentTaskCookRecipe {
        AgentTaskCookRecipe {
            schema: COOK_RECIPE_SCHEMA.to_string(),
            cook_id: cook_id.to_string(),
            attempts: vec![AgentTaskCookRecipeAttempt {
                attempt: 1,
                run_id: run_id.to_string(),
                plan: plan.clone(),
            }],
            promotion_transport: serde_json::json!({}),
            gate_policy: serde_json::json!({}),
            retry_budget: serde_json::json!({}),
            finalization: serde_json::json!({}),
            source_refs: vec![format!("{cook_id}-source")],
            runtime_generation: "test".to_string(),
            sensitive_mappings: Vec::new(),
            harvest_context: Default::default(),
        }
    }

    fn plan(plan_id: &str, marker: &str) -> AgentTaskPlan {
        AgentTaskPlan::new(
            plan_id,
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: marker.to_string(),
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

    fn options_for(
        cook_id: &str,
        run_id: &str,
        initial_plan: AgentTaskPlan,
    ) -> AgentTaskCookServiceOptions {
        AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan,
            to_worktree: format!("fixture@{cook_id}"),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: crate::agent_task_gate::VerifyGateOptions::default(),
            max_attempts: 1,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Paired-store materialization".to_string(),
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
    fn explicit_preparations_recover_identical_recipe_ids_in_parallel() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
        let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
        let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
        let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
        let cook_id = "same-recovery-cook";
        let run_id = "same-recovery-run";
        let barrier = Arc::new(Barrier::new(2));
        let mut left_plan = plan("left-recovery-plan", "left");
        left_plan.metadata = serde_json::json!({ "store": "left" });
        let mut right_plan = plan("right-recovery-plan", "right");
        right_plan.metadata = serde_json::json!({ "store": "right" });
        left_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, left_plan))
            .expect("persist left recipe");
        right_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, right_plan))
            .expect("persist right recipe");

        let (left_record, right_record) = std::thread::scope(|scope| {
            let left_barrier = Arc::clone(&barrier);
            let left_recipe_store = left_recipe_store.clone();
            let left_lifecycle_store = left_lifecycle_store.clone();
            let left = scope.spawn(move || {
                CookExecutionPreparation::new(&left_recipe_store, &left_lifecycle_store)
                    .recover_with_admission(
                        cook_id,
                        |_| {
                            left_barrier.wait();
                            Ok(serde_json::json!({ "store": "left" }))
                        },
                        |received_cook_id| {
                            assert_eq!(
                                left_lifecycle_store
                                    .read_cook_index(received_cook_id)
                                    .expect("left recovered index")
                                    .latest_run_id,
                                run_id
                            );
                            Ok(())
                        },
                    )
                    .expect("recover left")
                    .expect("left record")
            });
            let right_barrier = Arc::clone(&barrier);
            let right_recipe_store = right_recipe_store.clone();
            let right_lifecycle_store = right_lifecycle_store.clone();
            let right = scope.spawn(move || {
                CookExecutionPreparation::new(&right_recipe_store, &right_lifecycle_store)
                    .recover_with_admission(
                        run_id,
                        |_| {
                            right_barrier.wait();
                            Ok(serde_json::json!({ "store": "right" }))
                        },
                        |received_cook_id| {
                            assert_eq!(
                                right_lifecycle_store
                                    .read_cook_index(received_cook_id)
                                    .expect("right recovered index")
                                    .latest_run_id,
                                run_id
                            );
                            Ok(())
                        },
                    )
                    .expect("recover right")
                    .expect("right record")
            });
            (
                left.join().expect("left thread"),
                right.join().expect("right thread"),
            )
        });

        assert_eq!(left_record.run_id, run_id);
        assert_eq!(right_record.run_id, run_id);
        assert_eq!(left_record.metadata["controller_runtime"]["store"], "left");
        assert_eq!(
            right_record.metadata["controller_runtime"]["store"],
            "right"
        );
        assert_eq!(
            left_lifecycle_store
                .read_controller_plan(run_id)
                .expect("left recovered plan")
                .plan_id,
            "left-recovery-plan"
        );
        assert_eq!(
            right_lifecycle_store
                .read_controller_plan(run_id)
                .expect("right recovered plan")
                .plan_id,
            "right-recovery-plan"
        );
    }

    #[test]
    fn reserved_cancellation_reconciliation_mutates_only_the_explicit_lifecycle_root() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_store = AgentTaskLifecycleStore::new(left_context.path_roots());
        let right_store = AgentTaskLifecycleStore::new(right_context.path_roots());
        let cook_id = "same-reserved-cancellation-cook";
        let run_id = "same-reserved-cancellation-run";

        for (store, root) in [(&left_store, "left"), (&right_store, "right")] {
            store
                .submit_plan_with_runtime_admission(
                    &AgentTaskPlan::new(format!("{root}-parent"), Vec::new()),
                    cook_id,
                    |_| Ok(serde_json::json!({ "root": root })),
                )
                .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record.metadata["detached_cook_handoff"] = serde_json::json!({
                        "state": "pending",
                        "cook_id": cook_id,
                        "cancellation_fence": { "state": "open" },
                    });
                    true
                })
                .unwrap();
            agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
                store, cook_id, run_id,
            )
            .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record.state = agent_task_lifecycle::AgentTaskRunState::Cancelled;
                    record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"] =
                        serde_json::json!("cancelled");
                    true
                })
                .unwrap();
            store
                .submit_plan_with_runtime_admission(
                    &plan(&format!("{root}-child"), root),
                    run_id,
                    |_| Ok(serde_json::json!({ "root": root })),
                )
                .unwrap();
            store.record_cook_attempt(cook_id, 1, run_id).unwrap();
        }

        reconcile_reserved_cancellation_in_store(&left_store, cook_id).unwrap();

        let left = left_store.read_record(run_id).unwrap();
        let right = right_store.read_record(run_id).unwrap();
        assert_eq!(
            left.state,
            agent_task_lifecycle::AgentTaskRunState::Cancelled
        );
        assert_eq!(
            left.metadata["cancel_reason"],
            "detached Cook handoff cancelled"
        );
        assert_eq!(right.state, agent_task_lifecycle::AgentTaskRunState::Queued);
        assert!(right.metadata.get("cancelled_at").is_none());
    }

    #[test]
    fn explicit_stores_materialize_identical_cook_attempts_in_parallel() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
        let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
        let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
        let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
        let cook_id = "same-cook";
        let run_id = "same-run";
        let barrier = Arc::new(Barrier::new(2));
        let left_reconciled = Arc::new(AtomicBool::new(false));
        let right_reconciled = Arc::new(AtomicBool::new(false));

        let mut left_plan = plan("left-plan", "left");
        left_plan.metadata = serde_json::json!({ "store": "left" });
        let mut right_plan = plan("right-plan", "right");
        right_plan.metadata = serde_json::json!({ "store": "right" });
        left_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, left_plan.clone()))
            .expect("persist left recipe");
        right_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, right_plan.clone()))
            .expect("persist right recipe");

        std::thread::scope(|scope| {
            let left_barrier = Arc::clone(&barrier);
            let left_recipe_store = left_recipe_store.clone();
            let left_lifecycle_store = left_lifecycle_store.clone();
            let left_reconciled = Arc::clone(&left_reconciled);
            scope.spawn(move || {
                CookExecutionPreparation::new(&left_recipe_store, &left_lifecycle_store)
                    .materialize_with_admission(
                        cook_id,
                        run_id,
                        &left_plan,
                        |_| {
                            left_barrier.wait();
                            Ok(serde_json::json!({ "store": "left" }))
                        },
                        |received_cook_id| {
                            assert_eq!(received_cook_id, cook_id);
                            assert_eq!(
                                left_lifecycle_store
                                    .read_cook_index(received_cook_id)
                                    .expect("left index before cancellation reconciliation")
                                    .latest_run_id,
                                run_id
                            );
                            left_reconciled.store(true, Ordering::SeqCst);
                            Ok(())
                        },
                    )
                    .expect("materialize left");
            });
            let right_barrier = Arc::clone(&barrier);
            let right_recipe_store = right_recipe_store.clone();
            let right_lifecycle_store = right_lifecycle_store.clone();
            let right_reconciled = Arc::clone(&right_reconciled);
            scope.spawn(move || {
                CookExecutionPreparation::new(&right_recipe_store, &right_lifecycle_store)
                    .materialize_with_admission(
                        cook_id,
                        run_id,
                        &right_plan,
                        |_| {
                            right_barrier.wait();
                            Ok(serde_json::json!({ "store": "right" }))
                        },
                        |received_cook_id| {
                            assert_eq!(received_cook_id, cook_id);
                            assert_eq!(
                                right_lifecycle_store
                                    .read_cook_index(received_cook_id)
                                    .expect("right index before cancellation reconciliation")
                                    .latest_run_id,
                                run_id
                            );
                            right_reconciled.store(true, Ordering::SeqCst);
                            Ok(())
                        },
                    )
                    .expect("materialize right");
            });
        });

        assert!(left_reconciled.load(Ordering::SeqCst));
        assert!(right_reconciled.load(Ordering::SeqCst));

        assert_eq!(
            left_lifecycle_store
                .read_controller_plan(run_id)
                .unwrap()
                .plan_id,
            "left-plan"
        );
        assert_eq!(
            right_lifecycle_store
                .read_controller_plan(run_id)
                .unwrap()
                .plan_id,
            "right-plan"
        );
        assert_eq!(
            left_lifecycle_store.read_record(run_id).unwrap().metadata["controller_runtime"]
                ["store"],
            "left"
        );
        assert_eq!(
            right_lifecycle_store.read_record(run_id).unwrap().metadata["controller_runtime"]
                ["store"],
            "right"
        );
        assert_eq!(
            left_lifecycle_store
                .read_cook_index(cook_id)
                .unwrap()
                .latest_run_id,
            run_id
        );
        assert_eq!(
            right_lifecycle_store
                .read_cook_index(cook_id)
                .unwrap()
                .latest_run_id,
            run_id
        );
        assert!(left_lifecycle_store
            .cook_index_path(cook_id)
            .starts_with(left_context.data_dir()));
        assert!(right_lifecycle_store
            .cook_index_path(cook_id)
            .starts_with(right_context.data_dir()));
        assert_ne!(
            left_lifecycle_store.cook_index_path(cook_id),
            right_lifecycle_store.cook_index_path(cook_id)
        );
        assert_eq!(
            left_recipe_store.load_recipe(cook_id).unwrap().attempts[0]
                .plan
                .plan_id,
            "left-plan"
        );
        assert_eq!(
            right_recipe_store.load_recipe(cook_id).unwrap().attempts[0]
                .plan
                .plan_id,
            "right-plan"
        );
    }

    #[test]
    fn explicit_stores_recover_adoption_failures_in_parallel() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_recipe_store = CookRecipeStore::new(left_context.path_roots());
        let right_recipe_store = CookRecipeStore::new(right_context.path_roots());
        let left_lifecycle_store = AgentTaskLifecycleStore::new(left_context.path_roots());
        let right_lifecycle_store = AgentTaskLifecycleStore::new(right_context.path_roots());
        let cook_id = "same-cook";
        let run_id = "same-run";
        let mut left_plan = plan("left-plan", "left");
        left_plan.metadata = serde_json::json!({ "store": "left" });
        let mut right_plan = plan("right-plan", "right");
        right_plan.metadata = serde_json::json!({ "store": "right" });
        left_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, left_plan.clone()))
            .expect("persist left recipe");
        right_recipe_store
            .persist_recipe(&recipe(cook_id, run_id, right_plan.clone()))
            .expect("persist right recipe");
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let left_barrier = Arc::clone(&barrier);
            let recipe_store = left_recipe_store.clone();
            let lifecycle_store = left_lifecycle_store.clone();
            scope.spawn(move || {
                CookExecutionPreparation::new(&recipe_store, &lifecycle_store)
                    .recover_for_adoption_with_runtime(
                        cook_id,
                        run_id,
                        Some(&|_| None),
                        Some("left-runner".to_string()),
                        |_| {
                            left_barrier.wait();
                            Ok(serde_json::json!({ "store": "left" }))
                        },
                        |_| Ok(()),
                    )
                    .expect("recover left adoption");
            });
            let right_barrier = Arc::clone(&barrier);
            let recipe_store = right_recipe_store.clone();
            let lifecycle_store = right_lifecycle_store.clone();
            scope.spawn(move || {
                CookExecutionPreparation::new(&recipe_store, &lifecycle_store)
                    .recover_for_adoption_with_runtime(
                        cook_id,
                        run_id,
                        Some(&|_| None),
                        Some("right-runner".to_string()),
                        |_| {
                            right_barrier.wait();
                            Ok(serde_json::json!({ "store": "right" }))
                        },
                        |_| Ok(()),
                    )
                    .expect("recover right adoption");
            });
        });

        for (lifecycle_store, expected_plan, expected_runner) in [
            (&left_lifecycle_store, "left-plan", "left-runner"),
            (&right_lifecycle_store, "right-plan", "right-runner"),
        ] {
            let record = lifecycle_store.read_record(run_id).expect("read record");
            let aggregate = lifecycle_store
                .read_aggregate(run_id)
                .expect("read aggregate");
            assert_eq!(
                record.metadata["pre_execution_failure"]["phase"],
                "transport_dispatcher_prepare"
            );
            assert!(record.metadata["pre_execution_failure"]["message"]
                .as_str()
                .expect("recovery message")
                .contains("recovered orphaned durable cook recipe"));
            assert_eq!(record.metadata["runner_id"], expected_runner);
            assert!(record
                .metadata
                .get("execution_placement_decision")
                .is_none());
            assert_eq!(aggregate.plan_id, expected_plan);
            assert_eq!(aggregate.totals.failed, 1);
            assert_eq!(
                lifecycle_store
                    .read_controller_plan(run_id)
                    .expect("read controller plan")
                    .plan_id,
                expected_plan
            );
        }
    }

    #[test]
    fn paired_stores_isolate_identical_ids_across_split_recipe_and_lifecycle_roots() {
        let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
        let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
        assert_ne!(recipe_context.data_dir(), lifecycle_context.data_dir());

        let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
        let lifecycle_store = AgentTaskLifecycleStore::new(lifecycle_context.path_roots());

        let cook_id = "split-root-cook";
        let run_id = "split-root-run";
        let attempt_plan = plan(cook_id, "split-root-materialization");
        recipe_store
            .persist_recipe(&recipe(cook_id, run_id, attempt_plan.clone()))
            .expect("persist recipe in recipe root");
        lifecycle_store
            .submit_plan_with_runtime_admission(&attempt_plan, run_id, |_| {
                Ok(serde_json::json!({}))
            })
            .expect("submit plan in lifecycle root");

        materialize_initial_cook_attempt_with_stores(
            &recipe_store,
            &lifecycle_store,
            &options_for(cook_id, run_id, attempt_plan),
        )
        .expect("materialize initial attempt through paired stores");

        assert_eq!(
            lifecycle_store
                .read_cook_index(cook_id)
                .expect("read cook index in lifecycle root")
                .latest_run_id,
            run_id
        );
        assert_eq!(
            lifecycle_store
                .read_record(run_id)
                .expect("read run record in lifecycle root")
                .run_id,
            run_id
        );
        assert!(lifecycle_store
            .run_dir(run_id)
            .starts_with(lifecycle_context.data_dir()));
        assert!(lifecycle_store
            .cook_index_path(cook_id)
            .starts_with(lifecycle_context.data_dir()));
        assert!(recipe_store.recipe_exists(cook_id));

        let recipe_root_lifecycle_store = AgentTaskLifecycleStore::new(recipe_context.path_roots());
        assert!(!recipe_root_lifecycle_store
            .record_exists(run_id)
            .expect("no run record in recipe root"));
        assert!(!recipe_root_lifecycle_store.cook_index_exists(cook_id));
        let lifecycle_root_recipe_store = CookRecipeStore::new(lifecycle_context.path_roots());
        assert!(!lifecycle_root_recipe_store.recipe_exists(cook_id));

        let retry_cook_id = "split-root-retry-cook";
        let retry_run_id = "split-root-retry-run";
        let retry_plan = plan(retry_cook_id, "split-root-retry-materialization");
        recipe_store
            .persist_recipe(&recipe(retry_cook_id, retry_run_id, retry_plan.clone()))
            .expect("persist retry recipe in recipe root");
        lifecycle_store
            .submit_plan_with_runtime_admission(&retry_plan, retry_run_id, |_| {
                Ok(serde_json::json!({}))
            })
            .expect("submit retry plan in lifecycle root");

        materialize_cook_attempt_with_stores(
            &recipe_store,
            &lifecycle_store,
            retry_cook_id,
            retry_run_id,
            &retry_plan,
        )
        .expect("materialize retry attempt through paired stores");

        assert_eq!(
            lifecycle_store
                .read_cook_index(retry_cook_id)
                .expect("read retry cook index in lifecycle root")
                .latest_run_id,
            retry_run_id
        );
        assert!(!recipe_root_lifecycle_store
            .record_exists(retry_run_id)
            .expect("no retry run record in recipe root"));
        assert!(!recipe_root_lifecycle_store.cook_index_exists(retry_cook_id));
    }
}
