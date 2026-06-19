//! Core service for durable agent-task dispatch.
//!
//! The CLI adapter owns clap parsing and JSON rendering. This service owns the
//! typed dispatch request, plan construction, provider preflight, durable
//! lifecycle transitions, and scheduler orchestration.

use serde::Serialize;
use serde_json::Value;

use crate::core::agent_task_aggregate::AgentTaskAggregateReport;
use crate::core::agent_task_dispatch_plan::{
    build_dispatch_plan_with_provider_requirements, preflight_dispatch_provider_secrets,
};
use crate::core::agent_task_lifecycle as lifecycle;
use crate::core::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
use crate::core::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, default_backend_for_component,
    provider_requires_cwd_git_checkout,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskScheduler,
};
use crate::core::agent_task_service::{aggregate_exit_code, AgentTaskRunResult};
use crate::core::{Error, Result};

pub const DISPATCH_RESULT_SCHEMA: &str = "homeboy/agent-task-dispatch/v1";

/// Dispatch inputs shared verbatim across the dispatch arg, command, and request
/// carriers (and their test override fixtures). Factored into one struct so the
/// `[attempts, client_context, provider_config, queue_only, tasks_json]` group
/// is declared once instead of duplicated across every dispatch carrier (#5187).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchCoreInputs {
    /// JSON array/object of task prompts for waves.
    pub tasks_json: Option<String>,
    /// Provider config JSON object, `@file`, or `-` for stdin.
    pub provider_config: Option<String>,
    /// Opaque client context JSON object, `@file`, or `-` for stdin.
    pub client_context: Option<String>,
    /// Attempts per task, including the first attempt.
    pub attempts: u32,
    /// Persist the run for a daemon/runner but do not execute immediately.
    pub queue_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskDispatchRequest {
    pub prompt: Option<String>,
    pub tasks: Vec<String>,
    pub cwd: Option<String>,
    pub workspace: Option<String>,
    pub repo: Option<String>,
    pub task_url: Option<String>,
    pub backend: String,
    pub selector: Option<String>,
    pub model: Option<String>,
    pub required_capabilities: Vec<String>,
    pub secret_env: Vec<String>,
    pub concurrency: usize,
    pub run_id: Option<String>,
    pub core: DispatchCoreInputs,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskDispatchReport {
    pub schema: &'static str,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub plan_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    pub task_count: usize,
    pub queued: bool,
    pub record: AgentTaskRunRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AgentTaskAggregate>,
}

pub fn dispatch<E>(
    request: AgentTaskDispatchRequest,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>>
where
    E: AgentTaskExecutorAdapter,
{
    dispatch_with_provider_requirements(request, executor, provider_requires_cwd_git_checkout)
}

pub fn dispatch_with_provider_requirements<E>(
    request: AgentTaskDispatchRequest,
    executor: E,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>>
where
    E: AgentTaskExecutorAdapter,
{
    let mut plan = build_dispatch_plan_with_provider_requirements(
        &request,
        provider_requires_cwd_git_checkout,
    )?;
    apply_provider_runner_secret_env_contracts(&mut plan);
    preflight_dispatch_provider_secrets(&plan)?;
    let submitted = lifecycle::submit_plan(&plan, request.run_id.as_deref())?;
    let run_id = submitted.run_id.clone();

    if request.core.queue_only {
        return Ok(AgentTaskRunResult {
            value: dispatch_report(submitted, None, true),
            exit_code: 0,
        });
    }

    lifecycle::mark_running(&run_id)?;
    let aggregate = AgentTaskScheduler::new(executor).run(plan.clone());
    let record = lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = aggregate_exit_code(&aggregate);

    Ok(AgentTaskRunResult {
        value: dispatch_report(record, Some(aggregate), false),
        exit_code,
    })
}

fn dispatch_report(
    record: AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
    queued: bool,
) -> AgentTaskDispatchReport {
    AgentTaskDispatchReport {
        schema: DISPATCH_RESULT_SCHEMA,
        run_id: record.run_id.clone(),
        plan_id: record.plan_id.clone(),
        state: record.state,
        plan_path: record.plan_path.clone(),
        aggregate_path: record.aggregate_path.clone(),
        task_count: record.tasks.len(),
        queued,
        record,
        aggregate,
    }
}

/// Command-surface inputs for a dispatch/cook invocation. The CLI adapter maps
/// its clap args into this plain data carrier; this service owns the orchestration
/// (backend resolution, request construction, scheduler dispatch, and cook
/// handoff rendering) so the command module stays a thin adapter (#5078).
#[derive(Debug, Clone, Default)]
pub struct AgentTaskDispatchCommand {
    pub prompt: Option<String>,
    pub tasks: Vec<String>,
    pub cwd: Option<String>,
    pub workspace: Option<String>,
    pub repo: Option<String>,
    pub task_url: Option<String>,
    pub backend: Option<String>,
    pub selector: Option<String>,
    pub model: Option<String>,
    pub required_capabilities: Vec<String>,
    pub secret_env: Vec<String>,
    pub concurrency: usize,
    pub run_id: Option<String>,
    pub core: DispatchCoreInputs,
}

/// Resolve a typed dispatch request from command-surface inputs, applying the
/// declared default-backend policy when `--backend` is absent.
pub fn resolve_dispatch_request(
    command: AgentTaskDispatchCommand,
) -> Result<AgentTaskDispatchRequest> {
    resolve_dispatch_request_with_default(command, default_backend_for_component)
}

/// Resolve a typed dispatch request, using the supplied default-backend resolver
/// so tests can inject a deterministic policy.
pub fn resolve_dispatch_request_with_default(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
) -> Result<AgentTaskDispatchRequest> {
    let backend = match command.backend {
        Some(backend) => backend,
        None => default_backend(command.repo.as_deref())?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "backend",
                "agent-task dispatch requires --backend because no default backend policy is configured",
                None,
                Some(vec![
                    "Set agent_task.default_backend in component, extension, or Homeboy config policy, or pass --backend explicitly.".to_string(),
                ]),
            )
        })?,
    };

    Ok(AgentTaskDispatchRequest {
        prompt: command.prompt,
        tasks: command.tasks,
        cwd: command.cwd,
        workspace: command.workspace,
        repo: command.repo,
        task_url: command.task_url,
        backend,
        selector: command.selector,
        model: command.model,
        required_capabilities: command.required_capabilities,
        secret_env: command.secret_env,
        concurrency: command.concurrency,
        run_id: command.run_id,
        core: command.core,
    })
}

/// Run a dispatch invocation end to end: resolve the request, dispatch it through
/// the scheduler, and adapt the report into a JSON value plus process exit code.
pub fn run_dispatch_command<E>(
    command: AgentTaskDispatchCommand,
    executor: E,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter,
{
    let request = resolve_dispatch_request(command)?;
    let result = dispatch(request, executor)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

/// Run a cook invocation: dispatch the request and attach the cook handoff
/// envelope describing the patch-artifact boundary and promotion next actions.
pub fn run_cook_command<E>(command: AgentTaskDispatchCommand, executor: E) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter,
{
    let target_worktree = command.workspace.clone();
    let (mut value, exit_code) = run_dispatch_command(command, executor)?;
    value["handoff"] = cook_handoff(&value, target_worktree.as_deref());
    Ok((value, exit_code))
}

fn command_json_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Error::internal_json(error.to_string(), None))
}

fn cook_handoff(value: &Value, to_worktree: Option<&str>) -> Value {
    let run_id = value["run_id"].as_str().unwrap_or("<run-id>");
    let aggregate_path = value["aggregate_path"].as_str().unwrap_or(run_id);
    let aggregate_review = value
        .get("aggregate")
        .and_then(|aggregate| serde_json::from_value::<AgentTaskAggregate>(aggregate.clone()).ok())
        .map(|aggregate| AgentTaskAggregateReport::from(aggregate.outcomes))
        .unwrap_or_else(|| AgentTaskAggregateReport::from(Vec::new()));
    let promotion_commands =
        cook_promotion_commands(aggregate_path, to_worktree, &aggregate_review);
    let patch_artifact_produced = !promotion_commands.is_empty();
    let mut next_actions = Vec::new();

    if patch_artifact_produced {
        next_actions.push("patch artifact produced; promote one candidate before claiming worktree changes or PR completion".to_string());
        if to_worktree.is_some() {
            next_actions.push(
                "run one `promote_commands` entry to apply the patch into the target worktree"
                    .to_string(),
            );
        } else {
            next_actions.push(format!(
                "run `homeboy agent-task review {run_id} --to-worktree <handle>` to generate exact promote commands"
            ));
        }
        next_actions.push(format!(
            "after promotion and verification, run `homeboy agent-task finalize-pr --run-id {run_id} --path <promoted-worktree-path> --title <title> --commit-message <message>`"
        ));
    } else if value["queued"].as_bool().unwrap_or(false) {
        next_actions.push(format!(
            "queued only; run `homeboy agent-task run {run_id}` or let a daemon claim it with `homeboy agent-task run-next`"
        ));
    } else {
        next_actions.push("no patch artifact was produced; inspect `aggregate` candidates before retrying or reporting".to_string());
    }

    serde_json::json!({
        "schema": "homeboy/agent-task-cook-handoff/v1",
        "states": {
            "patch_artifact_produced": patch_artifact_produced,
            "patch_promoted": false,
            "pr_opened": false
        },
        "boundary": if value["queued"].as_bool().unwrap_or(false) {
            "queued"
        } else if patch_artifact_produced {
            "patch_artifact_only"
        } else {
            "no_patch_artifact"
        },
        "promote_commands": promotion_commands,
        "finalize_command": format!(
            "homeboy agent-task finalize-pr --run-id {run_id} --path <promoted-worktree-path> --title <title> --commit-message <message>"
        ),
        "next_actions": next_actions
    })
}

fn cook_promotion_commands(
    source: &str,
    to_worktree: Option<&str>,
    review: &AgentTaskAggregateReport,
) -> Vec<Vec<String>> {
    review
        .apply_candidates
        .iter()
        .flat_map(|candidate| {
            candidate.artifact_ids.iter().map(move |artifact_id| {
                let mut command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "promote".to_string(),
                    source.to_string(),
                    "--task-id".to_string(),
                    candidate.task_id.clone(),
                    "--artifact-id".to_string(),
                    artifact_id.clone(),
                ];
                if let Some(to_worktree) = to_worktree {
                    command.push("--to-worktree".to_string());
                    command.push(to_worktree.to_string());
                }
                command
            })
        })
        .collect()
}
