//! Agent-task plan execution lifecycle: submit/run/resume/retry, workspace
//! preparation and component-worktree normalization, secret-env preflight,
//! and the shared `AgentTaskRunResult` envelope. Pure move out of the former
//! `agent_task_service.rs` god-file.

use std::time::Duration;

use serde_json::{json, Value};

use crate::agent_task::{AgentTaskRequest, AgentTaskWorkspaceMode};
use crate::agent_task_lifecycle::{
    self, AgentTaskRunArtifacts, AgentTaskRunLog, AgentTaskRunRecord, AgentTaskRunStatus,
};
use crate::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, provider_secret_sources_for_plan,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use crate::agent_task_secrets::validate_secret_env_with_fallbacks;
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::{config, worktree, Error, Result};

use super::cook_baseline::DerivedCookBaselineCapability;
use super::discovery::source_uri;

#[derive(Debug, Clone)]
pub struct AgentTaskRunResult<T> {
    pub value: T,
    pub exit_code: i32,
}

fn transport_proxy_recovery_error(recovery: agent_task_lifecycle::TransportProxyRecovery) -> Error {
    let recovery_message = match &recovery {
        agent_task_lifecycle::TransportProxyRecovery::Resumed { .. } => {
            "was resumed on its recorded runner; await its durable result"
        }
        _ => "is owned by runner transport recovery; provider execution was not attempted",
    };
    Error::validation_invalid_argument(
        "run_id",
        format!(
            "agent-task run '{}' {recovery_message}",
            recovery.record().run_id,
        ),
        Some(recovery.record().run_id.clone()),
        None,
    )
    .with_hint(format!("Next: {}", recovery.next_action()))
    .with_retryable(true)
}

pub fn read_plan(spec: &str) -> Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

pub fn run_loaded_plan<E>(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    run_loaded_plan_with_derived_cook_baseline(plan, record_run_id, executor, None, None)
}

pub(crate) fn run_loaded_plan_with_derived_cook_baseline<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    supplied_harvest_context: Option<crate::agent_task_scheduler::HarvestExecutionContext>,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    if let Some(run_id) = record_run_id {
        // Prepare before persistence so the lifecycle record and scheduler use
        // the same materialized workspace contract. In particular, Cook's
        // derived baseline capability must bind the persisted task workspace.
        if let Err(error) = prepare_plan_for_execution(&mut plan, Some(run_id)) {
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            agent_task_lifecycle::record_pre_execution_failure(
                run_id,
                &plan,
                "prepare_plan_for_execution",
                &error,
            )?;
            return Err(error);
        }
        let harvest_context = match supplied_harvest_context.clone().map(Ok).unwrap_or_else(
            crate::agent_task_scheduler::HarvestExecutionContext::from_current_process,
        ) {
            Ok(context) => context,
            Err(error) => {
                agent_task_lifecycle::record_pre_execution_failure(
                    run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
        if harvest_context.snapshot_signaled() {
            bind_runner_snapshot_workspace_attestations(&mut plan)?;
        }
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
        let aggregate = run_plan_with_scheduler(
            plan.clone(),
            record_run_id,
            executor,
            derived_cook_baseline,
            harvest_context,
        )?;
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
        return Ok(AgentTaskRunResult {
            exit_code: aggregate_exit_code(&aggregate),
            value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
        });
    } else {
        prepare_plan_for_execution(&mut plan, None)?;
    }

    let harvest_context = supplied_harvest_context
        .unwrap_or(crate::agent_task_scheduler::HarvestExecutionContext::from_current_process()?);
    if harvest_context.snapshot_signaled() {
        bind_runner_snapshot_workspace_attestations(&mut plan)?;
    }
    let aggregate = run_plan_with_scheduler(
        plan.clone(),
        record_run_id,
        executor,
        derived_cook_baseline,
        harvest_context,
    )?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
    })
}

/// A Lab plan carries its controller admission identity until its paths are
/// materialized on the runner. Bind that concrete runner snapshot before the
/// plan is persisted or executed; the predecessor remains audit provenance.
pub fn bind_runner_snapshot_workspace_attestations(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let Some(current_identity) = task.metadata.get("cook_workspace_identity").cloned() else {
            continue;
        };
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook runner snapshot identity requires a workspace root",
                Some(task.task_id.clone()),
                None,
            )
        })?;
        let runner_identity =
            crate::agent_task_workspace_identity::attest_workspace(std::path::Path::new(root))?;
        task.metadata["cook_workspace_identity"] = runner_identity;
        if task
            .metadata
            .get("cook_workspace_identity_predecessor")
            .is_none()
        {
            task.metadata["cook_workspace_identity_predecessor"] = current_identity;
        }
    }
    Ok(())
}

pub fn submit_plan_spec(spec: &str, run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let plan = read_plan(spec)?;
    agent_task_lifecycle::submit_plan(&plan, run_id)
}

pub fn run_submitted<E>(
    run_id: String,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    run_submitted_with_timeout(run_id, None, executor)
}

pub fn run_submitted_with_timeout<E>(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    if let Some(result) = terminal_run_result(&run_id)? {
        return Ok(result);
    }
    if let Some(recovery) = agent_task_lifecycle::recover_transport_proxy(&run_id)? {
        if let Ok(aggregate) = agent_task_lifecycle::read_aggregate(&recovery.record().run_id) {
            return Ok(AgentTaskRunResult {
                exit_code: aggregate_exit_code(&aggregate),
                value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
            });
        }
        return Err(transport_proxy_recovery_error(recovery));
    }
    let mut plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
    if let Some(timeout_ms) = timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    prepare_plan_for_execution(&mut plan, Some(&run_id))?;
    let harvest_context =
        match crate::agent_task_scheduler::HarvestExecutionContext::from_current_process() {
            Ok(context) => context,
            Err(error) => {
                agent_task_lifecycle::record_pre_execution_failure(
                    &run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
    agent_task_lifecycle::mark_running(&run_id)?;
    run_prepared_claimed(run_id, plan, executor, harvest_context)
}

pub fn run_next<E>(executor: E) -> Result<AgentTaskRunResult<Option<AgentTaskAggregate>>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_next_with_cook_dispatcher(executor, |_| Ok(None))
}

pub fn run_next_with_cook_dispatcher<E>(
    executor: E,
    dispatcher: impl FnOnce(
        &Value,
    ) -> Result<
        Option<std::sync::Arc<dyn super::cook::AgentTaskCookAttemptDispatcher>>,
    >,
) -> Result<AgentTaskRunResult<Option<AgentTaskAggregate>>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if let Some(claim) = super::claim_continuation()? {
        let cook_id = claim.continuation().cook_id.clone();
        let run_id = claim.continuation().run_id.clone();
        let exit_code = super::consume_claimed_with_dispatcher(claim, dispatcher, |options| {
            super::run_cook(options, executor.clone()).map(|result| result.exit_code)
        })?;
        let latest_run_id = agent_task_lifecycle::cook_index(&cook_id)
            .map(|index| index.latest_run_id)
            .unwrap_or(run_id);
        let aggregate = agent_task_lifecycle::read_aggregate(&latest_run_id).ok();
        return Ok(AgentTaskRunResult {
            value: aggregate.map(|aggregate| {
                crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate)
            }),
            exit_code,
        });
    }
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok(AgentTaskRunResult {
            value: None,
            exit_code: 0,
        });
    };

    let result = run_claimed(record.run_id, executor)?;
    Ok(AgentTaskRunResult {
        value: Some(crate::agent_task_artifacts::reviewer_facing_aggregate(
            &result.value,
        )),
        exit_code: result.exit_code,
    })
}

pub fn resume<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    if let Some(result) = terminal_run_result(&run_id)? {
        return Ok(result);
    }
    if let Some(recovery) = agent_task_lifecycle::recover_transport_proxy(&run_id)? {
        if let Ok(aggregate) = agent_task_lifecycle::read_aggregate(&recovery.record().run_id) {
            return Ok(AgentTaskRunResult {
                exit_code: aggregate_exit_code(&aggregate),
                value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
            });
        }
        return Err(transport_proxy_recovery_error(recovery));
    }
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
}

/// Reconcile a completed run's controller-owned artifact projection without
/// resuming or redispatching provider execution.
pub fn reconcile_terminal_artifact_projection(run_id: &str) -> Result<bool> {
    agent_task_lifecycle::reconcile_terminal_artifact_projection(run_id)
}

/// Replay an authenticated terminal runner snapshot without resuming provider work.
pub fn recover_terminal_transport_proxy_evidence(run_id: &str) -> Result<bool> {
    agent_task_lifecycle::recover_terminal_transport_proxy_evidence(run_id)
}

pub fn terminal_transport_recovery_required(run_id: &str) -> bool {
    agent_task_lifecycle::read_aggregate(run_id).map_or(true, |aggregate| {
        aggregate.outcomes.is_empty()
            && (aggregate.totals.queued
                + aggregate.totals.running
                + aggregate.totals.blocked
                + aggregate.totals.skipped
                + aggregate.totals.succeeded
                + aggregate.totals.failed
                + aggregate.totals.cancelled
                + aggregate.totals.timed_out
                + aggregate.totals.candidate_recoverable
                + aggregate.totals.recoverable_candidates)
                > 0
    })
}

/// Return durable terminal evidence instead of attempting to transition a
/// completed child run back into execution during controller reconciliation.
pub fn terminal_run_result(run_id: &str) -> Result<Option<AgentTaskRunResult<AgentTaskAggregate>>> {
    let record = agent_task_lifecycle::status(run_id)?;
    if !matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialFailure
            | agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::Cancelled
    ) {
        return Ok(None);
    }

    let aggregate = match agent_task_lifecycle::read_aggregate(&record.run_id) {
        Ok(aggregate) => aggregate,
        Err(_) => {
            // A terminal Lab result may have been persisted before its typed
            // aggregate projection. Reconcile only that recorded terminal
            // evidence; never resume or rerun the provider for this path.
            agent_task_lifecycle::recover_terminal_transport_proxy_evidence(&record.run_id)?;
            agent_task_lifecycle::read_aggregate(&record.run_id).map_err(|_| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is terminal with state {:?} but has no durable aggregate evidence",
                record.run_id, record.state
            ),
            Some(record.run_id.clone()),
            Some(vec![format!(
                "retry the child with: homeboy agent-task retry {} --run",
                record.run_id
            )]),
        )
            })?
        }
    };
    Ok(Some(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
    }))
}

pub fn retry(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
) -> Result<AgentTaskRetryServiceResult> {
    let source = agent_task_lifecycle::status(run_id)?;
    let cook_retry = retryable_cook_attempt(&source)?;
    let record = match cook_retry {
        Some(cook_retry) => {
            let discovered_run_id = agent_task_lifecycle::find_unbound_cook_retry_successor(
                &source.run_id,
                &cook_retry.cook_id,
                cook_retry.attempt,
                &cook_retry.plan,
            )?
            .map(|record| record.run_id);
            let mut retry_run_id = cook_retry
                .pending_run_id
                .as_deref()
                .or(new_run_id)
                .map(str::to_string)
                .or(discovered_run_id)
                .unwrap_or_else(|| {
                    agent_task_lifecycle::cook_attempt_run_id(
                        &cook_retry.cook_id,
                        cook_retry.attempt,
                    )
                });
            let retry_exists = agent_task_lifecycle::run_record_exists(&retry_run_id)?;
            if retry_exists
                && !is_exact_retry_reservation(&source, &cook_retry.plan, &retry_run_id)?
            {
                return Err(Error::validation_invalid_argument(
                    "new_run_id",
                    "retry run id is not the durable retry reservation for this Cook attempt",
                    Some(retry_run_id),
                    None,
                ));
            }
            // The lifecycle record is the durable reservation. It writes retry_of
            // before recipe/index binding, so recovery can prove ownership without
            // adopting an unrelated same-plan run.
            if !retry_exists {
                retry_run_id = reserve_cook_retry_lifecycle(&source, &cook_retry, &retry_run_id)?;
            }
            // Recipe and index are one Cook-owned binding boundary. Serialize
            // concurrent claim observers so neither can overwrite the other's
            // append-only recipe revision between its read and write.
            config::with_config_lock(|| {
                super::record_recipe_attempt(
                    &cook_retry.cook_id,
                    cook_retry.attempt,
                    &retry_run_id,
                    &cook_retry.plan,
                )?;
                agent_task_lifecycle::record_cook_attempt(
                    &cook_retry.cook_id,
                    cook_retry.attempt,
                    &retry_run_id,
                )?;
                Ok(())
            })?;
            let record = agent_task_lifecycle::status(&retry_run_id)?;
            if record.state.is_terminal() {
                return Ok(AgentTaskRetryServiceResult { record, run: false });
            }
            record
        }
        None => agent_task_lifecycle::retry(&source.run_id, new_run_id)?,
    };
    Ok(AgentTaskRetryServiceResult { record, run })
}

fn reserve_cook_retry_lifecycle(
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    retry: &CookRetryAttempt,
    retry_run_id: &str,
) -> Result<String> {
    let operation_key = format!("retry:{}:{}", retry.cook_id, retry.attempt);
    match agent_task_lifecycle::claim_cook_operation(
        &source.run_id,
        &operation_key,
        Duration::from_secs(30),
    )? {
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            agent_task_lifecycle::retry(&source.run_id, Some(retry_run_id))?;
            agent_task_lifecycle::complete_cook_operation(
                &source.run_id,
                &operation_key,
                json!({ "run_id": retry_run_id }),
            )?;
            Ok(retry_run_id.to_string())
        }
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            let recorded_run_id = result["run_id"].as_str().ok_or_else(|| {
                Error::internal_unexpected("completed Cook retry claim has no run id")
            })?;
            Ok(recorded_run_id.to_string())
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            // The winner writes its lifecycle reservation before completing the
            // claim. Re-read through the indexed successor path on the next
            // retry rather than allocating a competing run id.
            // Controller admission can take up to the normal local lease
            // handoff window. Bound the observer wait above that window so a
            // crashed winner remains recoverable rather than waiting forever.
            for _ in 0..2_000 {
                if let Some(record) = agent_task_lifecycle::find_unbound_cook_retry_successor(
                    &source.run_id,
                    &retry.cook_id,
                    retry.attempt,
                    &retry.plan,
                )? {
                    return Ok(record.run_id);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(Error::validation_invalid_argument(
                "run_id",
                "Cook retry reservation is still being finalized",
                Some(source.run_id.clone()),
                None,
            ))
        }
    }
}

/// A retryable failure before provider execution has no candidate or execution
/// evidence to supersede, so its retry remains an append-only Cook attempt.
fn retryable_cook_attempt(
    source: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<CookRetryAttempt>> {
    if source.metadata["pre_execution_failure"]["retryable"] != serde_json::Value::Bool(true) {
        return Ok(None);
    }
    let Some(cook_id) = source.metadata["cook_id"].as_str() else {
        return Ok(None);
    };
    let Some(source_attempt) = source.metadata["cook_attempt"].as_u64() else {
        return Ok(None);
    };
    let attempt = u32::try_from(source_attempt).map_err(|_| {
        Error::validation_invalid_argument(
            "cook_attempt",
            "durable Cook attempt number exceeds the supported range",
            Some(source.run_id.clone()),
            None,
        )
    })?;
    if !super::recipe_exists(cook_id)? {
        // Legacy runs predate durable recipes. Retain their established generic
        // lifecycle retry behavior rather than inventing Cook ownership.
        return Ok(None);
    }
    let recipe = super::load_recipe(cook_id)?;
    let source_recipe_attempt = recipe.attempts.iter().find(|recipe_attempt| {
        recipe_attempt.attempt == attempt && recipe_attempt.run_id == source.run_id
    });
    let Some(source_recipe_attempt) = source_recipe_attempt else {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "retryable pre-provider failure is not owned by its durable Cook recipe",
            Some(source.run_id.clone()),
            Some(vec![format!(
                "Continue the owning Cook with: homeboy agent-task cook-continue {}",
                cook_id
            )]),
        ));
    };
    let mut pending_attempt = None;
    for recipe_attempt in recipe
        .attempts
        .iter()
        .filter(|recipe_attempt| recipe_attempt.attempt == attempt.saturating_add(1))
    {
        if !agent_task_lifecycle::run_record_exists(&recipe_attempt.run_id)? {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry recipe entry has no durable lifecycle reservation",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        let record = agent_task_lifecycle::exact_record(&recipe_attempt.run_id)?;
        if record.metadata["retry_of"] != source.run_id {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run is not the durable retry of its source attempt",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        if agent_task_lifecycle::load_plan(&recipe_attempt.run_id)? != recipe_attempt.plan {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run does not match its durable plan",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        if record.state.is_terminal() {
            if record.state == agent_task_lifecycle::AgentTaskRunState::Succeeded {
                return Ok(Some(CookRetryAttempt {
                    cook_id: cook_id.to_string(),
                    attempt: recipe_attempt.attempt,
                    pending_run_id: Some(recipe_attempt.run_id.clone()),
                    plan: recipe_attempt.plan.clone(),
                }));
            }
            // A terminal failed successor is authoritative evidence. It cannot
            // be resumed; a later attempt may be allocated below if the durable
            // retry budget permits it.
            continue;
        }
        if record.state != agent_task_lifecycle::AgentTaskRunState::Queued {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run is already active and must be resumed through its lifecycle",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        pending_attempt = Some(recipe_attempt);
        break;
    }
    if let Some(pending_attempt) = pending_attempt {
        return Ok(Some(CookRetryAttempt {
            cook_id: cook_id.to_string(),
            attempt: pending_attempt.attempt,
            pending_run_id: Some(pending_attempt.run_id.clone()),
            plan: pending_attempt.plan.clone(),
        }));
    }
    let max_attempts = recipe
        .retry_budget
        .get("max_attempts")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.retry_budget.max_attempts",
                "durable Cook recipe is missing a valid max_attempts budget",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    let next_attempt = recipe
        .attempts
        .iter()
        .map(|recipe_attempt| recipe_attempt.attempt)
        .max()
        .unwrap_or(attempt)
        .checked_add(1)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable Cook attempt sequence is exhausted",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    if next_attempt > max_attempts {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.retry_budget.max_attempts",
            "manual retry would exceed the durable Cook attempt budget",
            Some(cook_id.to_string()),
            None,
        ));
    }
    Ok(Some(CookRetryAttempt {
        cook_id: cook_id.to_string(),
        attempt: next_attempt,
        pending_run_id: None,
        plan: source_recipe_attempt.plan.clone(),
    }))
}

fn is_exact_retry_reservation(
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    run_id: &str,
) -> Result<bool> {
    let record = agent_task_lifecycle::exact_record(run_id)?;
    Ok(record.metadata["retry_of"] == source.run_id
        && agent_task_lifecycle::load_plan(run_id)? == *plan)
}

struct CookRetryAttempt {
    cook_id: String,
    attempt: u32,
    pending_run_id: Option<String>,
    plan: AgentTaskPlan,
}

#[derive(Debug, Clone)]
pub struct AgentTaskRetryServiceResult {
    pub record: AgentTaskRunRecord,
    pub run: bool,
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::status(run_id)
}

/// [`status`] with explicit control over whether the read may reach the runner.
///
/// Read-only inspection must stay answerable while the Lab is wedged (#10418).
pub fn status_with_options(
    run_id: &str,
    options: agent_task_lifecycle::AgentTaskStatusOptions,
) -> Result<agent_task_lifecycle::AgentTaskStatusOutcome> {
    agent_task_lifecycle::status_with_options(run_id, options)
}

/// Return the controller's durable record without runner liveness enrichment.
pub fn persisted_status(run_id: &str) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::persisted_status(run_id)
}

pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    agent_task_lifecycle::run_status(run_id, since_cursor)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    agent_task_lifecycle::logs(run_id)
}

pub fn logs_with_raw(run_id: &str) -> Result<AgentTaskRunLog> {
    agent_task_lifecycle::logs_with_raw(run_id, true)
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    agent_task_lifecycle::artifacts(run_id)
}

pub fn cancel(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::cancel_run(run_id, reason)
}

pub fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn run_claimed<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let mut plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
    if let Err(error) = prepare_plan_for_execution(&mut plan, Some(&run_id)) {
        agent_task_lifecycle::record_pre_execution_failure(
            &run_id,
            &plan,
            "prepare_plan_for_execution",
            &error,
        )?;
        return Err(error);
    }
    let harvest_context =
        match crate::agent_task_scheduler::HarvestExecutionContext::from_current_process() {
            Ok(context) => context,
            Err(error) => {
                agent_task_lifecycle::record_pre_execution_failure(
                    &run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
    run_prepared_claimed(run_id, plan, executor, harvest_context)
}

fn run_prepared_claimed<E>(
    run_id: String,
    plan: AgentTaskPlan,
    executor: E,
    harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let aggregate =
        run_plan_with_scheduler(plan.clone(), Some(&run_id), executor, None, harvest_context)?;
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

fn prepare_plan_for_execution(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    prepare_plan_workspaces(plan, run_id)?;
    apply_provider_runner_secret_env_contracts(plan);
    preflight_plan_secret_env(plan)
}

fn prepare_plan_workspaces(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    for request in &mut plan.tasks {
        prepare_component_worktree_workspace(request, run_id)?;
    }

    Ok(())
}

fn preflight_plan_secret_env(plan: &AgentTaskPlan) -> Result<()> {
    let secret_env_plan = SecretEnvPlan::from_secret_env_names(
        plan.tasks
            .iter()
            .flat_map(|task| task.executor.secret_env.iter().cloned()),
    );

    validate_secret_env_with_fallbacks(
        &secret_env_plan.secret_env_names(),
        &provider_secret_sources_for_plan(plan),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "secret_env",
            error.message,
            None,
            Some(vec![
                "Agent-task executor provider manifests can declare runner-required secret env contracts; Homeboy validates those contracts before task execution.".to_string(),
                "For local execution, configure provider credentials with `homeboy agent-task auth map-env`, `set-keychain`, or `set-keychain-bundle`.".to_string(),
                "For delegated runner execution, configure the selected runner's secret_env references so the runner receives these names without printing values.".to_string(),
            ]),
        )
    })
}

fn run_plan_with_scheduler<E>(
    plan: AgentTaskPlan,
    run_id: Option<&str>,
    executor: E,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
) -> Result<AgentTaskAggregate>
where
    E: AgentTaskExecutorAdapter,
{
    let scheduler =
        AgentTaskScheduler::new_controller(executor).with_harvest_context(harvest_context);
    match run_id {
        Some(run_id) => Ok(scheduler
            .with_run_id(run_id.to_string())
            .run_with_derived_cook_baseline(plan, derived_cook_baseline)),
        None => Ok(scheduler.run_with_derived_cook_baseline(plan, derived_cook_baseline)),
    }
}

pub fn aggregate_exit_code(aggregate: &AgentTaskAggregate) -> i32 {
    if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    }
}

fn normalize_component_worktree_workspace(request: &mut AgentTaskRequest) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        ));
    };

    let resolved_root = request
        .workspace
        .root
        .clone()
        .or_else(|| materialization_string(&request.workspace.materialization, "root"))
        .or_else(|| materialization_string(&request.workspace.materialization, "resolved_root"));

    let Some(root) = resolved_root else {
        return Ok(());
    };

    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(root);
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = None;
    request.workspace.materialization = Value::Null;

    Ok(())
}

fn prepare_component_worktree_workspace(
    request: &mut AgentTaskRequest,
    run_id: Option<&str>,
) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }
    if request.workspace.root.is_some()
        || materialization_string(&request.workspace.materialization, "root").is_some()
        || materialization_string(&request.workspace.materialization, "resolved_root").is_some()
    {
        return normalize_component_worktree_workspace(request);
    }

    let component_id = request.workspace.component_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        )
    })?;
    let branch = request.workspace.branch.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.branch",
            format!(
                "agent-task task '{}' component-worktree workspace for component '{}' requires branch",
                request.task_id, component_id
            ),
            None,
            None,
        )
    })?;
    let cleanup_policy = cleanup_policy_for_workspace(request.workspace.cleanup.as_deref());
    let created = worktree::create(worktree::WorktreeCreateOptions {
        component_id: component_id.clone(),
        branch,
        from: request.workspace.base_ref.clone(),
        task_url: request.workspace.task_url.clone().or_else(|| {
            request
                .source_refs
                .iter()
                .find(|source| source.kind == "task")
                .or_else(|| request.source_refs.first())
                .map(source_uri)
        }),
        run_id: run_id.map(str::to_string),
        cleanup_policy,
    })?;
    let record = created.record;
    let cleanup = cleanup_lifecycle_policy(&record.cleanup_policy);
    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(record.worktree_path.clone());
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = Some(cleanup.to_string());
    request.workspace.materialization = serde_json::json!({
        "kind": "homeboy-worktree",
        "id": record.id,
        "component_id": record.component_id,
        "branch": record.branch,
        "base_ref": record.base_ref,
        "root": record.worktree_path,
        "source_checkout": record.source_checkout,
        "task_url": record.task_url,
        "run_id": record.run_id,
        "cleanup_policy": cleanup,
    });

    Ok(())
}

fn cleanup_policy_for_workspace(value: Option<&str>) -> Option<worktree::CleanupPolicy> {
    match value {
        Some("remove_when_safe") | Some("remove-when-safe") | Some("cleanup") => {
            Some(worktree::CleanupPolicy::RemoveWhenSafe)
        }
        Some("preserve") | Some("preserve_on_failure") | Some("preserve-on-failure") => {
            Some(worktree::CleanupPolicy::PreserveOnFailure)
        }
        _ => None,
    }
}

fn cleanup_lifecycle_policy(policy: &worktree::CleanupPolicy) -> &'static str {
    match policy {
        worktree::CleanupPolicy::RemoveWhenSafe => "remove_when_safe",
        worktree::CleanupPolicy::PreserveOnFailure => "preserve",
    }
}

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}
