//! Agent-task plan execution lifecycle: submit/run/resume/retry, workspace
//! preparation and component-worktree normalization, secret-env preflight,
//! and the shared `AgentTaskRunResult` envelope. Pure move out of the former
//! `agent_task_service.rs` god-file.

use serde_json::Value;

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
use crate::secret_env_plan::SecretEnvPlan;
use crate::{config, worktree, Error, Result};

use super::cook::DerivedCookBaselineCapability;
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
        // A runner has its own lifecycle store. Materialize the handed-off plan
        // before any preparation can invoke runner-scoped lifecycle commands.
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        if let Err(error) = prepare_plan_for_execution(&mut plan, Some(run_id)) {
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
    let record = agent_task_lifecycle::retry(run_id, new_run_id)?;
    Ok(AgentTaskRetryServiceResult { record, run })
}

#[derive(Debug, Clone)]
pub struct AgentTaskRetryServiceResult {
    pub record: AgentTaskRunRecord,
    pub run: bool,
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::status(run_id)
}

pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    agent_task_lifecycle::run_status(run_id, since_cursor)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    agent_task_lifecycle::logs(run_id)
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
