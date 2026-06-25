//! Agent-task plan execution lifecycle: submit/run/resume/retry, workspace
//! preparation and component-worktree normalization, secret-env preflight,
//! and the shared `AgentTaskRunResult` envelope. Pure move out of the former
//! `agent_task_service.rs` god-file.

use serde_json::Value;

use crate::core::agent_task::{AgentTaskRequest, AgentTaskWorkspaceMode};
use crate::core::agent_task_lifecycle::{
    self, AgentTaskRunArtifacts, AgentTaskRunLog, AgentTaskRunRecord,
};
use crate::core::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, provider_secret_sources_for_plan,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use crate::core::agent_task_secrets::validate_secret_env_with_fallbacks;
use crate::core::secret_env_plan::SecretEnvPlan;
use crate::core::{config, worktree, Error, Result};

use super::discovery::source_uri;

#[derive(Debug, Clone)]
pub struct AgentTaskRunResult<T> {
    pub value: T,
    pub exit_code: i32,
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
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    prepare_plan_for_execution(&mut plan, record_run_id)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
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
    let mut plan = agent_task_lifecycle::load_plan(&run_id)?;
    prepare_plan_for_execution(&mut plan, Some(&run_id))?;
    agent_task_lifecycle::mark_running(&run_id)?;
    run_prepared_claimed(run_id, plan, executor)
}

pub fn run_next<E>(executor: E) -> Result<AgentTaskRunResult<Option<AgentTaskAggregate>>>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok(AgentTaskRunResult {
            value: None,
            exit_code: 0,
        });
    };

    let result = run_claimed(record.run_id, executor)?;
    Ok(AgentTaskRunResult {
        value: Some(result.value),
        exit_code: result.exit_code,
    })
}

pub fn resume<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
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
    let mut plan = agent_task_lifecycle::load_plan(&run_id)?;
    if let Err(error) = prepare_plan_for_execution(&mut plan, Some(&run_id)) {
        agent_task_lifecycle::record_pre_execution_failure(
            &run_id,
            &plan,
            "prepare_plan_for_execution",
            &error,
        )?;
        return Err(error);
    }
    run_prepared_claimed(run_id, plan, executor)
}

fn run_prepared_claimed<E>(
    run_id: String,
    plan: AgentTaskPlan,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
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

fn run_plan_with_scheduler<E>(plan: AgentTaskPlan, executor: E) -> AgentTaskAggregate
where
    E: AgentTaskExecutorAdapter,
{
    AgentTaskScheduler::new(executor).run(plan)
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
