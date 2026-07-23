//! Public batch-cook fanout command handlers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use homeboy::agents::agent_task_provider::AgentTaskProviderProfileDeclaration;
use homeboy::agents::agent_tasks::batch;
use homeboy::agents::agent_tasks::dispatch_service::{
    AgentTaskDispatchCommand, DispatchCoreInputs,
};
use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentPolicy, AgentTaskGateRevealPolicy, VerifyGateOptions,
};
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider::{self, AgentTaskProviderCatalog};
use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;
use homeboy::agents::agent_tasks::service::{
    self as agent_task_service, AgentTaskCookServiceOptions,
};
use homeboy::agents::agent_tasks::{
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
    AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
};
use homeboy::core::{config, worktree, Error, Result};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs, AgentTaskFanoutRunPlanArgs,
    AgentTaskFanoutSubmitArgs, AgentTaskFanoutSubmitBatchArgs,
};
use super::command_json_value;

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::CookBatch(cook_batch_args) => cook_batch(cook_batch_args),
        AgentTaskFanoutCommand::Plan(plan_args) => {
            let plan = load_batch_cook_fanout_plan(&plan_args.input)?;
            Ok((command_json_value(plan)?, 0))
        }
        AgentTaskFanoutCommand::Submit(submit_args) => submit_batch_cook_fanout(submit_args),
        AgentTaskFanoutCommand::SubmitBatch(submit_args) => submit_fanout_batch(submit_args),
        AgentTaskFanoutCommand::Status(status_args) => batch_status(status_args),
        AgentTaskFanoutCommand::Resume(resume_args) => batch_resume(resume_args),
        AgentTaskFanoutCommand::Artifacts(status_args) => batch_artifacts(status_args),
        AgentTaskFanoutCommand::RunPlan(run_args) => run_batch_cook_fanout(run_args),
    }
}

type CookAttemptDispatcherFactory = dyn Fn(
        &AgentTaskCookServiceOptions,
    ) -> std::sync::Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>
    + Send
    + Sync;

/// Runs controller-owned cook-batch coordination while dispatching its typed
/// provider attempts through the caller-selected transport.
pub(crate) fn cook_batch_with_attempt_dispatcher(
    args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    cook_batch_inner(args, Some(attempt_dispatcher))
}

fn submit_batch_cook_fanout(args: AgentTaskFanoutSubmitArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(run_id) = args.run_id {
        plan.rekey(run_id);
    }

    let cooks = plan
        .cooks
        .iter()
        .map(|cook| {
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
                "run_id_semantics": "stable cook id; executed attempts use unique durable run ids",
                "worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
                "title": cook.title,
                "command": cook_command(&plan, cook),
            })
        })
        .collect::<Vec<_>>();

    Ok((
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
            "fanout_id": plan.fanout_id,
            "state": "ready",
            "cooks": cooks,
            "next_actions": [
                "run each cook command on its target worktree/branch, or use `agent-task fanout run-plan` to execute the batch cook from this machine"
            ]
        }),
        0,
    ))
}

fn submit_fanout_batch(args: AgentTaskFanoutSubmitBatchArgs) -> CmdResult<Value> {
    let plan = load_fanout_agent_task_plan(&args.input)?;
    let record = batch::submit_plan_batch(&plan, args.batch_id.as_deref())?;
    let batch_id = record.batch_id.clone();
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-batch-submit-result/v1",
            "batch": record,
            "commands": batch_commands(&batch_id),
        }),
        0,
    ))
}

fn batch_status(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    Ok((command_json_value(batch::status(&args.batch_id)?)?, 0))
}

/// Resume a durable fanout batch after its synchronous coordinator exited.
/// Idempotently harvests every terminal-but-unfinalized child through its
/// original promotion, deterministic gates, commit, push, and PR finalization
/// contract, reconciling per-child state back into the durable batch record so
/// repeated resume calls converge without duplicate PRs (#9525).
fn batch_resume(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    let result = agent_task_service::resume_cook_batch(
        &args.batch_id,
        provider::ExtensionProviderAgentTaskExecutor::discover(),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )?;
    let exit_code = result.exit_code;
    let report = result.value;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-batch-resume/v1",
            "batch_id": report.batch_id,
            "status": report.status,
            "summary": {
                "total": report.total,
                "succeeded": report.succeeded,
                "failed": report.failed,
            },
            "cooks": report.cooks,
            "commands": {
                "status": format!("homeboy agent-task fanout status {}", args.batch_id),
                "artifacts": format!("homeboy agent-task fanout artifacts {}", args.batch_id),
                "resume": format!("homeboy agent-task fanout resume {}", args.batch_id),
            },
        }),
        exit_code,
    ))
}

fn batch_artifacts(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    Ok((command_json_value(batch::artifacts(&args.batch_id)?)?, 0))
}

fn run_batch_cook_fanout(args: AgentTaskFanoutRunPlanArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    run_batch_cook_fanout_plan(plan)
}

/// Keeps durable batch coordination on the controller while routing each
/// independent provider attempt through the selected transport.
pub(crate) fn run_batch_cook_fanout_with_attempt_dispatcher(
    args: AgentTaskFanoutRunPlanArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    run_batch_cook_fanout_plan_with_attempt_dispatcher(plan, attempt_dispatcher)
}

/// Persist the durable batch record before dispatching children so
/// `fanout status <fanout_id>` can resolve every child run (#9397). Without
/// this, run-plan admitted children but never wrote
/// `agent-task-batches/<fanout_id>.json`, so status failed with
/// `No such file or directory`.
fn persist_fanout_run_batch_record(plan: &BatchCookFanoutPlan) -> Result<()> {
    let children = plan
        .cooks
        .iter()
        .map(|cook| batch::FanoutRunBatchChild {
            task_id: cook.cook_id.clone(),
            run_id: cook.run_id(),
        })
        .collect::<Vec<_>>();
    batch::persist_fanout_run_batch(
        &plan.fanout_id,
        &plan.fanout_id,
        &children,
        serde_json::json!({
            "source": "fanout-run-plan",
            "durable_child_runs": true,
        }),
    )?;
    Ok(())
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    persist_fanout_run_batch_record(&plan)?;
    let result = agent_task_service::run_cook_batch(
        agent_task_service::AgentTaskCookBatchOptions {
            batch_id: plan.fanout_id.clone(),
            cooks: compile_batch_cooks(&plan, |options| {
                options.attempt_dispatcher = Some(attempt_dispatcher(options));
            })?,
            max_concurrency: batch_worker_limit(&plan),
        },
        provider::ExtensionProviderAgentTaskExecutor::discover(),
    )?;
    Ok(batch_cook_result(&plan, result))
}

fn run_batch_cook_fanout_plan(plan: BatchCookFanoutPlan) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_executor(
        plan,
        provider::ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_batch_cook_fanout_plan_with_executor<E>(
    plan: BatchCookFanoutPlan,
    executor: E,
) -> CmdResult<Value>
where
    E: homeboy::agents::agent_tasks::scheduler::AgentTaskExecutorAdapter + Clone + Send,
{
    persist_fanout_run_batch_record(&plan)?;
    let result = agent_task_service::run_cook_batch(
        agent_task_service::AgentTaskCookBatchOptions {
            batch_id: plan.fanout_id.clone(),
            cooks: compile_batch_cooks(&plan, |_| {})?,
            max_concurrency: batch_worker_limit(&plan),
        },
        executor,
    )?;
    Ok(batch_cook_result(&plan, result))
}

fn batch_harvest_context() -> Result<homeboy::agents::agent_task_scheduler::HarvestExecutionContext>
{
    if std::env::var_os("HOMEBOY_RUNNER_HOSTED_EXEC").is_some() {
        homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
    } else {
        Ok(homeboy::agents::agent_task_scheduler::HarvestExecutionContext::default())
    }
}

fn compile_batch_cooks(
    plan: &BatchCookFanoutPlan,
    configure: impl Fn(&mut AgentTaskCookServiceOptions),
) -> Result<Vec<AgentTaskCookServiceOptions>> {
    let harvest_context = batch_harvest_context()?;
    plan.cooks
        .iter()
        .map(|cook| {
            let invocation = cook.to_cook_invocation(plan)?;
            let mut options =
                agent_task_service::compile_cook_attempt(invocation.options, invocation.dispatch)?;
            options.harvest_context = harvest_context.clone();
            configure(&mut options);
            Ok(options)
        })
        .collect()
}

fn batch_worker_limit(plan: &BatchCookFanoutPlan) -> usize {
    plan.cooks
        .len()
        .min(std::thread::available_parallelism().map_or(1, usize::from))
}

fn batch_cook_result(
    plan: &BatchCookFanoutPlan,
    result: agent_task_service::AgentTaskRunResult<agent_task_service::AgentTaskCookBatchReport>,
) -> (Value, i32) {
    let report = result.value;
    let cooks = report
        .cooks
        .iter()
        .zip(&plan.cooks)
        .map(|(cell, cook)| {
            let cell_result = cell
                .result
                .as_ref()
                .map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                .unwrap_or_else(|| serde_json::json!({ "error": cell.error }));
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
                "worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
                "exit_code": cell.exit_code,
                "result": cell_result,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
            "fanout_id": plan.fanout_id,
            "status": report.status,
            "summary": { "total": report.total, "succeeded": report.succeeded, "failed": report.failed },
            "cooks": cooks,
            "commands": {
                "status": format!("homeboy agent-task fanout status {}", plan.fanout_id),
                "artifacts": format!("homeboy agent-task fanout artifacts {}", plan.fanout_id),
            },
        }),
        result.exit_code,
    )
}

fn cook_batch(args: AgentTaskFanoutCookBatchArgs) -> CmdResult<Value> {
    cook_batch_inner(args, None)
}

fn cook_batch_inner(
    mut args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
) -> CmdResult<Value> {
    if !args.gates.has_deterministic_gate() {
        return Err(invalid_fanout(
            "agent-task fanout cook-batch requires --verify or --private-verify",
        ));
    }
    apply_provider_profile(&mut args);
    // Resolve the effective backend (explicit --backend or the configured
    // default) and validate it up front (#7717). Otherwise an omitted
    // --backend silently rode a config default like `codebox` all the way to
    // provider execution, where each child cook failed late with a
    // provider-shaped `no extension agent-task provider found for backend`
    // error instead of an early, actionable configuration failure. Making the
    // effective backend explicit here also surfaces it in the preflight and
    // pins every child cook to the same resolved backend.
    resolve_and_validate_effective_backend(&mut args)?;
    let plan = build_cook_batch_plan(&args)?;
    preflight_batch_cook_recipes(&plan, attempt_dispatcher)?;
    let branches = plan
        .cooks
        .iter()
        .map(|cook| {
            let branch = cook.head.clone().expect("generated cooks have heads");
            (branch, cook.to_worktree.clone())
        })
        .collect::<Vec<_>>();
    let worktrees = queue_or_reuse_worktrees(&args, &branches)?;
    let blocked = worktrees
        .rows
        .iter()
        .filter(|row| {
            !matches!(
                row.status,
                worktree::WorktreeQueueCreateStatus::Created
                    | worktree::WorktreeQueueCreateStatus::Queued
            )
        })
        .count();
    let can_run = !args.dry_run
        && blocked == 0
        && worktrees
            .rows
            .iter()
            .all(|row| matches!(row.status, worktree::WorktreeQueueCreateStatus::Created));
    let run_result = if args.run_plan && can_run {
        let (value, exit_code) = match attempt_dispatcher {
            Some(dispatcher) => {
                run_batch_cook_fanout_plan_with_attempt_dispatcher(plan.clone(), dispatcher)?
            }
            None => run_batch_cook_fanout_plan(plan.clone())?,
        };
        Some(serde_json::json!({ "exit_code": exit_code, "result": value }))
    } else {
        None
    };
    let status = if args.run_plan && run_result.is_some() {
        run_result
            .as_ref()
            .and_then(|value| value["result"]["status"].as_str())
            .unwrap_or("completed")
    } else if blocked > 0 {
        "blocked"
    } else if args.dry_run {
        "planned"
    } else {
        "ready"
    };
    let exit_code = if blocked == 0 { 0 } else { 1 };

    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-batch/v1",
            "fanout_id": plan.fanout_id,
            "status": status,
            "dry_run": args.dry_run,
            "summary": {
                "issues": plan.cooks.len(),
                "worktrees_total": worktrees.rows.len(),
                "worktrees_blocked": blocked,
            },
            "preflight": {
                "provider_readiness_command": provider_readiness_command(&args),
                "provider_selection": provider_selection_preflight(&args),
                "deterministic_gates": {
                    "verify": args.gates.verify,
                    "private_verify": args.gates.private_verify,
                }
            },
            "worktrees": worktrees,
            "plan": plan,
            "run_result": run_result,
            "commands": cook_batch_commands(&args),
            "next_actions": cook_batch_next_actions(status, args.run_plan, blocked),
        }),
        exit_code,
    ))
}

fn queue_or_reuse_worktrees(
    args: &AgentTaskFanoutCookBatchArgs,
    branches: &[(String, String)],
) -> Result<worktree::WorktreeQueueCreateOutput> {
    let queue_create = |create_branches: Vec<String>, dry_run: bool| {
        worktree::queue_create(worktree::WorktreeQueueCreateOptions {
            repo: args.repo.clone(),
            branches: create_branches,
            from: args.from.clone(),
            task_url: None,
            task_ref: None,
            dry_run,
            retry_after_seconds: 30,
        })
    };

    if args.dry_run {
        return queue_create(
            branches.iter().map(|(branch, _)| branch.clone()).collect(),
            true,
        );
    }

    let mut reused = Vec::new();
    let mut to_create = Vec::new();
    for (branch, handle) in branches {
        match worktree::status(handle) {
            Ok(status)
                if status.record.state == worktree::TaskWorktreeState::Active
                    && !status.safety.worktree_missing =>
            {
                reused.push(worktree::WorktreeQueueCreateRow {
                    branch: branch.clone(),
                    handle: handle.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command: worktree_create_command(args, branch),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(status.record.worktree_path),
                    error: None,
                });
            }
            _ => to_create.push(branch.clone()),
        }
    }

    let created = queue_create(to_create, false)?;
    let mut rows = Vec::new();
    for (branch, handle) in branches {
        if let Some(row) = reused.iter().find(|row| row.handle == *handle) {
            rows.push(row.clone());
        } else if let Some(row) = created.rows.iter().find(|row| row.branch == *branch) {
            rows.push(row.clone());
        }
    }

    Ok(worktree::WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: args.repo.clone(),
        base_ref: args.from.clone(),
        dry_run: false,
        rows,
    })
}

fn preflight_batch_cook_recipes(
    plan: &BatchCookFanoutPlan,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
) -> Result<()> {
    for cook in &plan.cooks {
        let mut invocation = cook.to_cook_invocation(plan)?;
        invocation.options.harvest_context = batch_harvest_context()?;
        if let Some(dispatcher) = attempt_dispatcher {
            invocation.options.attempt_dispatcher = Some(dispatcher(&invocation.options));
        }
        agent_task_service::validate_initial_recipe_compatibility(&invocation.options)?;
    }
    Ok(())
}

fn load_fanout_agent_task_plan(
    args: &AgentTaskFanoutInputArgs,
) -> Result<homeboy::agents::agent_tasks::scheduler::AgentTaskPlan> {
    agent_task_service::read_plan(&args.input)
}

fn load_batch_cook_fanout_plan(args: &AgentTaskFanoutInputArgs) -> Result<BatchCookFanoutPlan> {
    let raw = config::read_json_spec_to_string(&args.input)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task fanout batch-cook input".to_string()),
            Some(raw.clone()),
        )
    })?;
    BatchCookFanoutPlan::from_value(value, args)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookFanoutPlan {
    #[serde(default = "batch_cook_fanout_plan_schema")]
    schema: String,
    fanout_id: String,
    cooks: Vec<BatchCookSpec>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    metadata: Value,
}

impl BatchCookFanoutPlan {
    fn from_value(value: Value, args: &AgentTaskFanoutInputArgs) -> Result<Self> {
        reject_generic_fanout_inputs(&value)?;
        let mut plan: BatchCookFanoutPlan = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "input",
                error.to_string(),
                None,
                Some(vec![
                    "Expected homeboy/agent-task-batch-cook-fanout-plan/v1 with a non-empty cooks array.".to_string(),
                ]),
            )
        })?;
        if let Some(fanout_id) = &args.fanout_id {
            plan.rekey(fanout_id.clone());
        }
        if plan.schema != AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA {
            return Err(invalid_fanout(
                "agent-task fanout requires homeboy/agent-task-batch-cook-fanout-plan/v1",
            ));
        }
        if plan.fanout_id.trim().is_empty() {
            return Err(invalid_fanout(
                "fanout_id is required for batch cook fanout",
            ));
        }
        if plan.cooks.is_empty() {
            return Err(invalid_fanout(
                "batch cook fanout requires at least one cook",
            ));
        }
        for cook in &mut plan.cooks {
            cook.apply_defaults(args)?;
        }
        Ok(plan)
    }

    fn rekey(&mut self, fanout_id: String) {
        let previous_prefix = format!("{}-", self.fanout_id);
        for cook in &mut self.cooks {
            let cell_id = cook
                .cook_id
                .strip_prefix(&previous_prefix)
                .unwrap_or(&cook.cook_id);
            cook.cook_id = format!("{fanout_id}-{cell_id}");
        }
        self.fanout_id = fanout_id;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookSpec {
    cook_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default)]
    tasks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    workspace_materialization: Vec<BatchCookWorkspaceMaterialization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    secret_env: Vec<String>,
    #[serde(default = "one")]
    attempts: u32,
    #[serde(default)]
    same_provider_retries: u32,
    #[serde(default)]
    provider_rotations: u32,
    #[serde(default = "one_usize")]
    concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_context: Option<String>,
    to_worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_command: Option<String>,
    #[serde(default)]
    verify: Vec<String>,
    #[serde(default)]
    private_verify: Vec<String>,
    #[serde(default = "default_private_gate_reveal")]
    private_gate_reveal: AgentTaskGateRevealPolicy,
    #[serde(default = "default_gate_timeout_seconds")]
    gate_timeout_seconds: u64,
    #[serde(default = "default_gate_heartbeat_interval_seconds")]
    gate_heartbeat_interval_seconds: u64,
    #[serde(default)]
    rerun_completed_gates: bool,
    #[serde(default)]
    gate_environment: AgentTaskGateEnvironmentPolicy,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default)]
    no_finalize: bool,
    #[serde(default = "default_base")]
    base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_message: Option<String>,
    #[serde(default)]
    protected_branches: Vec<String>,
    #[serde(default = "default_ai_tool")]
    ai_tool: String,
    #[serde(default = "default_ai_used_for")]
    ai_used_for: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookWorkspaceMaterialization {
    field: String,
    controller_path: String,
    runner_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    ref_name: Option<String>,
    sync_status: String,
}

#[derive(Debug, Clone)]
struct BatchCookInvocation {
    dispatch: AgentTaskDispatchCommand,
    options: AgentTaskCookServiceOptions,
}

impl BatchCookSpec {
    fn apply_defaults(&mut self, args: &AgentTaskFanoutInputArgs) -> Result<()> {
        if self.cook_id.trim().is_empty() {
            return Err(invalid_fanout("each cook requires a non-empty cook_id"));
        }
        if self.to_worktree.trim().is_empty() {
            return Err(invalid_fanout("each cook requires to_worktree"));
        }
        if self.prompt.is_none() && self.tasks.is_empty() {
            return Err(invalid_fanout("each cook requires prompt or tasks"));
        }
        if self.backend.is_none() {
            self.backend = args.backend.clone();
        }
        if self.selector.is_none() {
            self.selector = args.selector.clone();
        }
        if self.model.is_none() {
            self.model = args.model.clone();
        }
        if self.protected_branches.is_empty() {
            self.protected_branches = super::review::default_protected_branches();
        }
        Ok(())
    }

    fn run_id(&self) -> String {
        format!("cook-{}", self.cook_id)
    }

    fn to_cook_invocation(&self, plan: &BatchCookFanoutPlan) -> Result<BatchCookInvocation> {
        if self.verify.is_empty() && self.private_verify.is_empty() {
            return Err(invalid_fanout(
                "each fanout cook requires verify or private_verify so PR finalization has deterministic gates",
            ));
        }
        let dispatch = AgentTaskDispatchCommand {
            prompt: self.prompt.clone(),
            tasks: self.tasks.clone(),
            cwd: self.cwd.clone(),
            workspace: self
                .workspace
                .clone()
                .or_else(|| self.cwd.is_none().then(|| self.to_worktree.clone())),
            repo: self.repo.clone(),
            task_url: self.task_url.clone(),
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            required_capabilities: Vec::new(),
            secret_env: self.secret_env.clone(),
            concurrency: self.concurrency,
            run_id: Some(agent_task_lifecycle::cook_attempt_run_id(&self.run_id(), 1)),
            task_id: None,
            core: DispatchCoreInputs {
                tasks_json: None,
                provider_config: self.provider_config.clone(),
                client_context: Some(merged_client_context(plan, self)),
                attempts: self.attempts,
                same_provider_retries: self.same_provider_retries,
                provider_rotations: self.provider_rotations,
                queue_only: false,
                timeout_ms: None,
                resolved_provider_policy: None,
            },
        };
        let title = self
            .title
            .clone()
            .unwrap_or_else(|| default_cook_title(self));
        let commit_message = self
            .commit_message
            .clone()
            .unwrap_or_else(|| default_cook_commit_message(self));
        let source_worktree_path =
            agent_task_service::source_worktree_path(self.cwd.clone(), self.workspace.clone());
        let task_base_sha = source_worktree_path
            .as_deref()
            .and_then(|path| {
                std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(path)
                    .output()
                    .ok()
            })
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
        Ok(BatchCookInvocation {
            dispatch,
            options: AgentTaskCookServiceOptions {
                cook_id: self.run_id(),
                initial_run_id: self.run_id(),
                initial_plan: AgentTaskPlan::new(self.run_id(), Vec::new()),
                to_worktree: self.to_worktree.clone(),
                source_worktree_path,
                provider_command: self.provider_command.clone(),
                provider_invocation: None,
                gates: VerifyGateOptions {
                    verify: self.verify.clone(),
                    private_verify: self.private_verify.clone(),
                    private_gate_reveal: self.private_gate_reveal,
                    gate_timeout_seconds: self.gate_timeout_seconds,
                    gate_heartbeat_interval_seconds: self.gate_heartbeat_interval_seconds,
                    rerun_completed_gates: self.rerun_completed_gates,
                    gate_environment: self.gate_environment.clone(),
                },
                max_attempts: self.max_attempts,
                no_finalize: self.no_finalize,
                base: self.base.clone(),
                task_base_sha,
                head: self.head.clone(),
                title,
                commit_message,
                source_refs: self
                    .task_url
                    .clone()
                    .into_iter()
                    .chain(std::iter::once(cook_recipe_source_identity(plan, self)?))
                    .collect(),
                protected_branches: self.protected_branches.clone(),
                ai_tool: resolve_ai_tool_disclosure(
                    &self.ai_tool,
                    self.backend.as_deref(),
                    self.selector.as_deref(),
                    self.model.as_deref(),
                ),
                // Explicit/config/rotation model selection only. Disclosure text
                // is presentation, not provenance, so it is never reverse-parsed
                // into a model — omitted stays omitted (#9789).
                ai_model: self.model.clone(),
                ai_used_for: self.ai_used_for.clone(),
                attempt_dispatcher: None,
                harvest_context:
                    crate::agents::agent_task_scheduler::HarvestExecutionContext::default(),
            },
        })
    }
}

fn cook_recipe_source_identity(plan: &BatchCookFanoutPlan, cook: &BatchCookSpec) -> Result<String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "schema": "homeboy/agent-task-fanout-cook-source/v1",
        "fanout_id": plan.fanout_id,
        "cook": cook,
    }))
    .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(format!(
        "homeboy://agent-task/fanout-source/{:x}",
        Sha256::digest(encoded)
    ))
}

fn default_cook_title(cook: &BatchCookSpec) -> String {
    let target = cook
        .repo
        .as_deref()
        .or(cook.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_cook_commit_message(cook: &BatchCookSpec) -> String {
    let target = cook.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

fn merged_client_context(plan: &BatchCookFanoutPlan, cook: &BatchCookSpec) -> String {
    let mut context = serde_json::from_str::<Value>(cook.client_context.as_deref().unwrap_or("{}"))
        .unwrap_or(Value::Null);
    if !context.is_object() {
        context = serde_json::json!({ "base": context });
    }
    if let Some(object) = context.as_object_mut() {
        object.insert(
            "fanout".to_string(),
            serde_json::json!({
                "id": plan.fanout_id,
                "semantics": "batch_cook",
                "cook_id": cook.cook_id,
                "to_worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
            }),
        );
    }
    context.to_string()
}

fn cook_command(plan: &BatchCookFanoutPlan, _cook: &BatchCookSpec) -> Vec<String> {
    vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        "<batch-cook-plan.json>".to_string(),
        "--record-run-id".to_string(),
        plan.fanout_id.clone(),
    ]
}

fn build_cook_batch_plan(args: &AgentTaskFanoutCookBatchArgs) -> Result<BatchCookFanoutPlan> {
    let mut seen = HashSet::new();
    let mut cooks = Vec::with_capacity(args.issues.len());
    for issue_url in &args.issues {
        let issue = IssueRef::parse(issue_url)?;
        if !seen.insert(issue.key.clone()) {
            return Err(invalid_fanout(
                "duplicate issue URLs are not allowed in one cook-batch",
            ));
        }
        let branch = format!(
            "{}/issue-{}-{}",
            trim_slashes(&args.branch_prefix),
            issue.number,
            slugify(&issue.repo)
        );
        let worktree = format!("{}@{}", args.repo, slugify(&branch));
        let prompt = render_prompt(
            args.prompt_template.as_deref(),
            &issue,
            &args.repo,
            &branch,
            &worktree,
        );
        cooks.push(BatchCookSpec {
            cook_id: format!("issue-{}", issue.number),
            prompt: Some(prompt),
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            workspace_materialization: Vec::new(),
            repo: Some(args.repo.clone()),
            task_url: Some(issue_url.clone()),
            backend: args.backend.clone(),
            selector: args.selector.clone(),
            model: args.model.clone(),
            secret_env: args.secret_env.clone(),
            attempts: 1,
            same_provider_retries: 0,
            provider_rotations: 0,
            concurrency: 1,
            provider_config: args.provider_config.clone(),
            client_context: Some(
                serde_json::json!({
                    "issue_url": issue_url,
                    "issue_ref": issue.key,
                    "operator_workflow": "agent-task fanout cook-batch"
                })
                .to_string(),
            ),
            to_worktree: worktree,
            provider_command: None,
            verify: args.gates.verify.clone(),
            private_verify: args.gates.private_verify.clone(),
            private_gate_reveal: args.gates.private_gate_reveal,
            gate_timeout_seconds: args.gates.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gates.gate_heartbeat_interval_seconds,
            rerun_completed_gates: args.gates.rerun_completed_gates,
            gate_environment: VerifyGateOptions::from(args.gates.clone()).gate_environment,
            max_attempts: default_max_attempts(),
            no_finalize: false,
            base: args.base.clone(),
            head: Some(branch),
            title: Some(format!("Fix {}", issue.key)),
            commit_message: Some(format!("fix: address {}", issue.key)),
            protected_branches: super::review::default_protected_branches(),
            ai_tool: default_ai_tool(),
            ai_used_for: default_ai_used_for(),
        });
    }
    let first = cooks
        .first()
        .map(|cook| cook.cook_id.clone())
        .unwrap_or_else(|| "empty".to_string());
    let fanout_id = args.fanout_id.clone().unwrap_or_else(|| {
        let encoded = serde_json::to_vec(&cooks).expect("Cook specs serialize");
        let digest = format!("{:x}", Sha256::digest(encoded));
        format!(
            "cook-batch-{}-{}-{}-{}",
            args.repo,
            first,
            cooks.len(),
            &digest[..12]
        )
    });
    // Durable cook recipes are keyed by cook_id. Scope each generated child to
    // its fanout generation so the same issue can run in a later batch.
    for cook in &mut cooks {
        cook.cook_id = format!("{fanout_id}-{}", cook.cook_id);
    }
    Ok(BatchCookFanoutPlan {
        schema: batch_cook_fanout_plan_schema(),
        fanout_id,
        cooks,
        metadata: serde_json::json!({
            "source": "agent-task fanout cook-batch",
            "issue_count": args.issues.len(),
            "repo": args.repo,
            "base": args.base,
            "from": args.from,
        }),
    })
}

fn apply_provider_profile(args: &mut AgentTaskFanoutCookBatchArgs) {
    let Some(profile) = selected_provider_profile(args.provider_profile.as_deref()) else {
        return;
    };
    if args.backend.is_none() {
        args.backend = profile.backend;
    }
    if args.selector.is_none() {
        args.selector = profile.selector;
    }
    if args.model.is_none() {
        args.model = profile.model;
    }
    if args.provider_config.is_none() {
        args.provider_config = profile.provider_config.map(|value| value.to_string());
    }
}

/// Resolve the effective execution backend for the batch and fail early when it
/// has no installed provider (#7717).
///
/// When `--backend` is omitted the effective backend comes from the configured
/// `agent_task.default_backend`. We pin it onto `args.backend` so it is visible
/// in the preflight and carried identically to every child cook, then confirm a
/// provider can serve it — turning a late, provider-shaped child failure into an
/// early configuration error listing the backends that are actually installed.
fn resolve_and_validate_effective_backend(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let effective = match args.backend.as_deref() {
        Some(backend) if !backend.trim().is_empty() => backend.trim().to_string(),
        _ => match provider::default_backend().map_err(|error| {
            invalid_fanout(&format!("could not resolve default backend: {error}"))
        })? {
            Some(backend) => backend,
            // No explicit backend and no configured default: leave resolution to
            // the existing per-cook/component defaulting and its own diagnostics.
            None => return Ok(()),
        },
    };

    // Validate against installed providers only when the batch will actually
    // execute. Dry-run/planning legitimately runs where no provider is installed
    // (e.g. CI compiling the plan), so a hard provider check there would reject
    // valid planning. Execution is where an unresolved backend fails late, so
    // that is exactly where we fail early instead.
    let will_execute = args.run_plan && !args.dry_run;
    if will_execute {
        let catalog = AgentTaskProviderCatalog::discover();
        let selector = args.selector.as_deref();
        if !matches!(
            provider::resolve_provider_for_backend(catalog.providers(), &effective, selector),
            provider::ProviderResolution::Resolved(_)
        ) {
            let mut available: Vec<String> = catalog
                .providers()
                .iter()
                .map(|p| p.backend.clone())
                .collect();
            available.sort();
            available.dedup();
            let available_hint = if available.is_empty() {
                "no agent-task provider backends are installed; install a provider extension (e.g. opencode)".to_string()
            } else {
                format!("installed backends: {}", available.join(", "))
            };
            let source = if args.backend.is_some() {
                "requested via --backend"
            } else {
                "resolved from agent_task.default_backend"
            };
            return Err(invalid_fanout(&format!(
                "agent-task fanout backend '{effective}' ({source}) has no installed provider. \
                 Pass --backend <installed> explicitly, or set agent_task.default_backend to an installed backend. {available_hint}"
            )));
        }
    }

    // Pin the resolved backend so the preflight and every child cook use it,
    // making an otherwise-implicit default visible and consistent.
    args.backend = Some(effective);
    Ok(())
}

fn selected_provider_profile(name: Option<&str>) -> Option<AgentTaskProviderProfileDeclaration> {
    let name = name?.trim();
    AgentTaskProviderCatalog::discover()
        .providers()
        .iter()
        .flat_map(|provider| provider.cli.profiles.iter())
        .find(|profile| profile.name == name)
        .cloned()
}

fn provider_selection_preflight(args: &AgentTaskFanoutCookBatchArgs) -> Value {
    let warnings = provider_selection_warnings(args);
    serde_json::json!({
        "profile": args.provider_profile,
        "executor": {
            "backend": args.backend,
            "selector": args.selector,
        },
        "model": args.model,
        "provider_config": args.provider_config.as_ref().map(|_| "provided"),
        "warnings": warnings,
    })
}

fn provider_selection_warnings(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    match args.provider_profile.as_deref() {
        Some(name) if selected_provider_profile(Some(name)).is_none() => vec![format!(
            "provider profile '{name}' is not declared by installed executor providers; run `homeboy agent-task providers` to inspect available profiles"
        )],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IssueRef {
    url: String,
    owner: String,
    repo: String,
    number: String,
    key: String,
}

impl IssueRef {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim();
        let marker = "/issues/";
        let Some((prefix, number_part)) = trimmed.split_once(marker) else {
            return Err(invalid_fanout(
                "cook-batch issue inputs must be GitHub issue URLs",
            ));
        };
        let number = number_part
            .split(|c| matches!(c, '/' | '?' | '#'))
            .next()
            .unwrap_or_default();
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid_fanout(
                "GitHub issue URL is missing a numeric issue number",
            ));
        }
        let mut segments = prefix.trim_end_matches('/').rsplit('/');
        let repo = segments.next().unwrap_or_default();
        let owner = segments.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() {
            return Err(invalid_fanout(
                "GitHub issue URL must include owner and repo",
            ));
        }
        let key = format!("{owner}/{repo}#{number}");
        Ok(Self {
            url: trimmed.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: number.to_string(),
            key,
        })
    }
}

fn render_prompt(
    template: Option<&str>,
    issue: &IssueRef,
    repo: &str,
    branch: &str,
    worktree: &str,
) -> String {
    let template = template.unwrap_or(
        "Fix {issue_url}. Inspect the issue, implement the smallest correct change in {repo}, run the requested verification gates, and report the changed files plus verification results. Homeboy deterministic finalization is enabled: Homeboy will commit, push {branch}, open/update the PR, add AI disclosure, and finalize reviewer-ready evidence after gates pass. Do not inspect credentials, configure git identity, commit, push, or open/update the PR yourself.",
    );
    template
        .replace("{issue_url}", &issue.url)
        .replace("{issue_ref}", &issue.key)
        .replace("{repo}", repo)
        .replace("{branch}", branch)
        .replace("{worktree}", worktree)
}

fn provider_readiness_command(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "providers".to_string(),
    ];
    if let Some(backend) = &args.backend {
        command.push(format!("--backend={backend}"));
    }
    if let Some(selector) = &args.selector {
        command.push(format!("--selector={selector}"));
    }
    for secret in &args.secret_env {
        command.push(format!("--secret-env={secret}"));
    }
    command.push("--validate-readiness".to_string());
    command
}

fn worktree_create_command(args: &AgentTaskFanoutCookBatchArgs, branch: &str) -> Vec<String> {
    vec![
        "worktree".to_string(),
        "create".to_string(),
        args.repo.clone(),
        "--branch".to_string(),
        branch.to_string(),
        "--from".to_string(),
        args.from.clone(),
    ]
}

fn cook_batch_commands(args: &AgentTaskFanoutCookBatchArgs) -> Value {
    let issues = args.issues.join(" ");
    serde_json::json!({
        "plan": format!("homeboy agent-task fanout cook-batch --repo {} --dry-run {}", args.repo, issues),
        "run": format!("homeboy agent-task fanout cook-batch --repo {} --run-plan {}", args.repo, issues),
        "status": "inspect each cook result under plan.cooks and use agent-task status <run-id>",
        "retry": "rerun this cook-batch after fixing provider/worktree blockers, or rerun the blocked issue URL only",
        "resume_from_plan": "save .plan to JSON and run homeboy agent-task fanout run-plan --input @batch-cook-plan.json",
    })
}

fn cook_batch_next_actions(status: &str, run_plan: bool, blocked: usize) -> Vec<String> {
    if blocked > 0 {
        return vec![
            "repair worktree queue blockers reported under worktrees.rows".to_string(),
            "rerun the same cook-batch command; created worktrees are recorded and blocked rows carry retry commands".to_string(),
        ];
    }
    if run_plan {
        return vec![format!(
            "batch execution {status}; inspect run_result.result.cooks for PR/finalization outcomes"
        )];
    }
    vec![
        "review plan.cooks and provider_readiness_command before execution".to_string(),
        "rerun with --run-plan or save plan to JSON and run homeboy agent-task fanout run-plan --input @batch-cook-plan.json".to_string(),
    ]
}

fn trim_slashes(value: &str) -> String {
    value.trim_matches('/').to_string()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn reject_generic_fanout_inputs(value: &Value) -> Result<()> {
    let schema = value.get("schema").and_then(Value::as_str);
    if matches!(
        schema,
        Some("homeboy/agent-task-plan/v1" | "homeboy/agent-task-fanout-plan/v1")
    ) || value.is_array()
        || value.get("tasks").is_some()
        || value.get("packets").is_some()
    {
        return Err(invalid_fanout(
            "agent-task fanout now accepts only batch cook plans with independent cooks; generic task fanout belongs behind internal scheduler code",
        ));
    }
    Ok(())
}

fn invalid_fanout(message: &str) -> Error {
    Error::validation_invalid_argument("input", message.to_string(), None, None)
}

fn batch_cook_fanout_plan_schema() -> String {
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA.to_string()
}

fn one() -> u32 {
    1
}

fn one_usize() -> usize {
    1
}

fn default_max_attempts() -> u32 {
    3
}

fn default_base() -> String {
    "main".to_string()
}

fn default_private_gate_reveal() -> AgentTaskGateRevealPolicy {
    AgentTaskGateRevealPolicy::SummaryOnly
}

fn default_gate_timeout_seconds() -> u64 {
    30 * 60
}

fn default_gate_heartbeat_interval_seconds() -> u64 {
    5
}

fn default_ai_tool() -> String {
    AgentTaskProviderCatalog::discover()
        .providers()
        .iter()
        .find_map(|provider| provider.cli.default_ai_disclosure.clone())
        .or_else(|| {
            AgentTaskProviderCatalog::discover()
                .providers()
                .iter()
                .flat_map(|provider| provider.cli.profiles.iter())
                .find_map(|profile| profile.ai_disclosure.clone())
        })
        .unwrap_or_else(|| GENERIC_AI_DISCLOSURE.to_string())
}

/// Generic fallback disclosure used when no provider supplies one. Also the
/// sentinel that marks `ai_tool` as "not explicitly overridden by the operator",
/// so a `--model` selection can derive a concrete disclosure instead.
const GENERIC_AI_DISCLOSURE: &str = "AI-assisted";

/// Resolve the effective `ai_tool` disclosure for a cook.
///
/// When the operator explicitly supplied `--ai-tool`, that value is preserved.
/// Otherwise (`ai_tool` is empty or the generic default) the disclosure is
/// derived from the effective backend/selector/model via the provider catalog —
/// the single typed disclosure source — so a `--model` override produces a
/// correct AI-assistance statement rather than a stale hard-coded default.
/// (#8404)
pub(super) fn resolve_ai_tool_disclosure(
    ai_tool: &str,
    backend: Option<&str>,
    selector: Option<&str>,
    model: Option<&str>,
) -> String {
    let is_operator_override =
        !ai_tool.trim().is_empty() && ai_tool.trim() != GENERIC_AI_DISCLOSURE;
    if is_operator_override {
        return ai_tool.to_string();
    }

    let backend = match backend {
        Some(backend) if !backend.trim().is_empty() => backend.to_string(),
        _ => match provider::default_backend().ok().flatten() {
            Some(backend) => backend,
            None => return ai_tool.to_string(),
        },
    };

    AgentTaskProviderCatalog::discover()
        .ai_disclosure_for(&backend, selector, model)
        .unwrap_or_else(|| ai_tool.to_string())
}

/// Legacy AI-usage disclosure default. The reviewer-facing "Used for" text is
/// now authored by the agent's `review_form.used_for` and enforced by the cook
/// loop's review-form gate, so this no longer feeds the PR body. Defaults empty
/// (no canned platitude); retained only for recipe back-compatibility.
fn default_ai_used_for() -> String {
    String::new()
}

fn batch_commands(batch_id: &str) -> Value {
    serde_json::json!({
        "status": format!("homeboy agent-task fanout status {batch_id}"),
        "artifacts": format!("homeboy agent-task fanout artifacts {batch_id}"),
        "run_next": "homeboy agent-task run-next"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::{AgentTaskCommand, AgentTaskFanoutCommand};
    use crate::test_support::{env_lock, with_isolated_home};
    use clap::Parser;
    use serde_json::json;

    fn test_batch_plan() -> BatchCookFanoutPlan {
        BatchCookFanoutPlan::from_value(
            json!({
                "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                "fanout_id": "test-batch",
                "cooks": [
                    {"cook_id": "first", "prompt": "first", "cwd": env!("CARGO_MANIFEST_DIR"), "to_worktree": "homeboy@first", "verify": ["true"]},
                    {"cook_id": "second", "prompt": "second", "cwd": env!("CARGO_MANIFEST_DIR"), "to_worktree": "homeboy@second", "verify": ["true"]}
                ]
            }),
            &args(),
        )
        .expect("test batch plan")
    }

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn set(values: &[(&'static str, Option<&str>)]) -> Self {
            let prior = values
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var_os(name);
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                    (*name, previous)
                })
                .collect();
            Self(prior)
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn resolve_ai_tool_disclosure_preserves_an_explicit_operator_value() {
        // An operator-supplied disclosure is preserved verbatim regardless of
        // the selected model. (#8404)
        let resolved = resolve_ai_tool_disclosure(
            "Custom Tool (v2)",
            Some("opencode"),
            None,
            Some("openai/gpt-5.6-terra"),
        );
        assert_eq!(resolved, "Custom Tool (v2)");
    }

    #[test]
    fn resolve_ai_tool_disclosure_keeps_generic_default_for_an_unknown_backend() {
        // With no explicit override and a backend the catalog cannot resolve,
        // the generic default is preserved (nothing to derive from). Using an
        // explicit unknown backend keeps this deterministic regardless of the
        // providers installed in the test environment.
        let resolved = resolve_ai_tool_disclosure(
            GENERIC_AI_DISCLOSURE,
            Some("no-such-backend-xyz"),
            None,
            Some("openai/gpt-5.6-terra"),
        );
        assert_eq!(resolved, GENERIC_AI_DISCLOSURE);
    }

    #[test]
    fn compile_batch_cooks_delivers_controller_context_to_every_cell() {
        let _env_lock = env_lock();
        let _env = EnvRestore::set(&[
            ("HOMEBOY_RUNNER_HOSTED_EXEC", None),
            ("HOMEBOY_SOURCE_SNAPSHOT_JSON", None),
            ("HOMEBOY_LAB_OFFLOAD_JSON", None),
        ]);
        let plan = test_batch_plan();
        let cooks = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");

        assert_eq!(cooks.len(), 2);
        assert!(cooks.iter().all(|cook| cook.initial_plan.tasks.len() == 1));
        assert!(cooks
            .iter()
            .all(|cook| format!("{:?}", cook.harvest_context)
                == "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"));
    }

    fn args() -> AgentTaskFanoutInputArgs {
        AgentTaskFanoutInputArgs {
            input: "inline".to_string(),
            fanout_id: Some("fanout/refactor".to_string()),
            backend: Some("test".to_string()),
            selector: Some("fixture".to_string()),
            model: None,
        }
    }

    #[test]
    fn batch_cook_plan_requires_independent_cooks_with_worktrees() {
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "fanout/original",
                    "cooks": [
                        {
                            "cook_id": "5929-docs",
                            "prompt": "fix docs",
                            "repo": "homeboy",
                            "cwd": "/runner/workspaces/homeboy@5929-docs",
                            "workspace_materialization": [{
                                "field": "cwd",
                                "controller_path": "/Users/user/Developer/homeboy@5929-docs",
                                "runner_path": "/runner/workspaces/homeboy@5929-docs",
                                "branch": "fix/5929-docs",
                                "ref": "fix/5929-docs",
                                "sync_status": "materialized"
                            }],
                            "to_worktree": "homeboy@fix-5929-docs",
                            "head": "fix/5929-docs",
                            "verify": ["homeboy review test homeboy"]
                        },
                        {
                            "cook_id": "5929-cli",
                            "prompt": "fix cli",
                            "repo": "homeboy",
                            "to_worktree": "homeboy@fix-5929-cli",
                            "head": "fix/5929-cli",
                            "verify": ["homeboy review test homeboy"]
                        }
                    ]
                }),
                &args(),
            )
            .expect("batch cook fanout plan");

            assert_eq!(plan.fanout_id, "fanout/refactor");
            assert_eq!(plan.cooks.len(), 2);
            assert_eq!(plan.cooks[0].backend.as_deref(), Some("test"));
            assert_eq!(plan.cooks[0].selector.as_deref(), Some("fixture"));
            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(invocation.options.to_worktree, "homeboy@fix-5929-docs");
            assert_eq!(invocation.options.head.as_deref(), Some("fix/5929-docs"));
            assert_eq!(
                invocation.dispatch.cwd.as_deref(),
                Some("/runner/workspaces/homeboy@5929-docs")
            );
            assert_eq!(invocation.dispatch.workspace, None);
            let run_id = invocation
                .dispatch
                .run_id
                .as_deref()
                .expect("attempt run id");
            assert!(run_id.starts_with("cook-fanout_refactor-5929-docs-attempt-1-"));
            assert_eq!(
                run_id.len(),
                "cook-fanout_refactor-5929-docs-attempt-1-".len() + 8
            );
            assert_eq!(
                invocation.options.gates.verify,
                vec!["homeboy review test homeboy"]
            );
            assert!(invocation
                .dispatch
                .core
                .client_context
                .as_deref()
                .expect("client context")
                .contains("batch_cook"));
            assert!(invocation
                .dispatch
                .core
                .client_context
                .as_deref()
                .expect("client context")
                .contains("/Users/user/Developer/homeboy@5929-docs"));
        });
    }

    #[test]
    fn generic_fanout_inputs_are_rejected_from_public_contract() {
        let error = BatchCookFanoutPlan::from_value(
            json!({
                "schema": "homeboy/agent-task-fanout-plan/v1",
                "fanout_id": "generic",
                "plane": "workflow",
                "tasks": []
            }),
            &args(),
        )
        .expect_err("generic fanout rejected");

        assert!(error
            .to_string()
            .contains("accepts only batch cook plans with independent cooks"));
    }

    fn cook_batch_args() -> AgentTaskFanoutCookBatchArgs {
        AgentTaskFanoutCookBatchArgs {
            issues: vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453".to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/6454".to_string(),
            ],
            repo: "homeboy".to_string(),
            from: "origin/main".to_string(),
            base: "main".to_string(),
            branch_prefix: "fix".to_string(),
            fanout_id: Some("issue-wave".to_string()),
            prompt_template: None,
            backend: Some("sandbox".to_string()),
            selector: Some("sample.executor-provider".to_string()),
            model: Some("gpt-5.5".to_string()),
            provider_profile: None,
            secret_env: vec!["AI_PROVIDER_OPENAI_CODEX_TOKEN".to_string()],
            provider_config: Some(r#"{"runtime":"opencode"}"#.to_string()),
            gates: super::super::args::VerifyGateArgs {
                verify: vec!["cargo test --lib".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                gate_timeout_seconds: 30 * 60,
                gate_heartbeat_interval_seconds: 5,
                rerun_completed_gates: false,
                gate_environment_mode: "inherit".to_string(),
                gate_environment: Vec::new(),
                isolate_gate_home: true,
                isolate_gate_xdg: true,
            },
            dry_run: true,
            run_plan: false,
        }
    }

    #[test]
    fn cook_batch_builds_batch_cook_plan_from_issue_urls() {
        with_isolated_home(|_| {
            let args = cook_batch_args();
            let plan = build_cook_batch_plan(&args).expect("cook batch plan");

            assert_eq!(plan.fanout_id, "issue-wave");
            assert_eq!(plan.cooks.len(), 2);
            assert_eq!(plan.cooks[0].cook_id, "issue-wave-issue-6453");
            assert_eq!(plan.cooks[0].to_worktree, "homeboy@fix-issue-6453-homeboy");
            assert_eq!(
                plan.cooks[0].head.as_deref(),
                Some("fix/issue-6453-homeboy")
            );
            assert_eq!(
                plan.cooks[0].title.as_deref(),
                Some("Fix Extra-Chill/homeboy#6453")
            );
            assert!(plan.cooks[0]
                .prompt
                .as_deref()
                .expect("prompt")
                .contains("https://github.com/Extra-Chill/homeboy/issues/6453"));
            let prompt = plan.cooks[0].prompt.as_deref().expect("prompt");
            assert!(prompt.contains("Homeboy will commit, push fix/issue-6453-homeboy"));
            assert!(prompt.contains("open/update the PR"));
            assert!(prompt.contains("add AI disclosure"));
            assert!(
                prompt.contains("Do not inspect credentials, configure git identity, commit, push")
            );
            assert_eq!(plan.cooks[0].verify, vec!["cargo test --lib"]);
            assert_eq!(plan.cooks[0].backend.as_deref(), Some("sandbox"));

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.dispatch.workspace.as_deref(),
                Some("homeboy@fix-issue-6453-homeboy")
            );
        });
    }

    #[test]
    fn cook_invocation_omits_model_when_only_disclosure_names_one() {
        // #9789: an ai_tool disclosure like `OpenCode (gpt-5.5)` must not be
        // reverse-parsed into a model. With no explicit/config/rotation model,
        // the execution request's ai_model stays None.
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "disclosure-only",
                    "cooks": [{
                        "cook_id": "no-model",
                        "prompt": "do the thing",
                        "cwd": env!("CARGO_MANIFEST_DIR"),
                        "to_worktree": "homeboy@no-model",
                        "verify": ["true"],
                        "ai_tool": "OpenCode (gpt-5.5)"
                    }]
                }),
                &args(),
            )
            .expect("plan");
            // Guard the premise: no model selection anywhere, only a disclosure.
            assert_eq!(plan.cooks[0].model, None);
            assert!(plan.cooks[0].ai_tool.contains("gpt-5.5"));

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.options.ai_model, None,
                "disclosure text must not populate ai_model"
            );
        });
    }

    #[test]
    fn cook_invocation_preserves_explicit_model_selection() {
        // Explicit/config/rotation model selection must still populate the
        // execution request even when a disclosure is present.
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "explicit-model",
                    "cooks": [{
                        "cook_id": "with-model",
                        "prompt": "do the thing",
                        "cwd": env!("CARGO_MANIFEST_DIR"),
                        "to_worktree": "homeboy@with-model",
                        "verify": ["true"],
                        "model": "openai/gpt-5.6-terra",
                        "ai_tool": "OpenCode (stale-disclosure-model)"
                    }]
                }),
                &args(),
            )
            .expect("plan");

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.options.ai_model.as_deref(),
                Some("openai/gpt-5.6-terra"),
                "explicit model selection must reach the execution request"
            );
        });
    }

    #[test]
    fn cook_batch_scopes_durable_children_to_the_fanout_generation() {
        let first = build_cook_batch_plan(&cook_batch_args()).expect("first cook batch plan");
        let mut second_args = cook_batch_args();
        second_args.fanout_id = Some("issue-wave-v2".to_string());
        second_args.branch_prefix = "fix-v2".to_string();
        let second = build_cook_batch_plan(&second_args).expect("second cook batch plan");

        assert_eq!(first.cooks[0].task_url, second.cooks[0].task_url);
        assert_eq!(
            first.cooks[0].client_context, second.cooks[0].client_context,
            "issue identity remains task metadata"
        );
        assert_ne!(first.cooks[0].cook_id, second.cooks[0].cook_id);
        assert_ne!(first.cooks[0].run_id(), second.cooks[0].run_id());
        assert_ne!(first.cooks[0].to_worktree, second.cooks[0].to_worktree);
        assert_eq!(second.cooks[0].cook_id, "issue-wave-v2-issue-6453");
    }

    #[test]
    fn implicit_fanout_identity_tracks_effective_cook_inputs() {
        let mut args = cook_batch_args();
        args.fanout_id = None;
        let first = build_cook_batch_plan(&args).expect("first implicit plan");
        let replay = build_cook_batch_plan(&args).expect("exact replay");
        assert_eq!(first.fanout_id, replay.fanout_id);
        assert_eq!(first.cooks[0].cook_id, replay.cooks[0].cook_id);

        args.branch_prefix = "fix-v2".to_string();
        let changed = build_cook_batch_plan(&args).expect("changed plan");
        assert_ne!(first.fanout_id, changed.fanout_id);
        assert_ne!(first.cooks[0].cook_id, changed.cooks[0].cook_id);
    }

    #[test]
    fn fanout_overrides_rekey_scoped_and_legacy_children_once() {
        let mut plan = test_batch_plan();
        assert_eq!(plan.cooks[0].cook_id, "fanout/refactor-first");

        plan.rekey("replacement".to_string());
        assert_eq!(plan.fanout_id, "replacement");
        assert_eq!(plan.cooks[0].cook_id, "replacement-first");
        assert_eq!(plan.cooks[1].cook_id, "replacement-second");

        let mut legacy = BatchCookFanoutPlan {
            schema: batch_cook_fanout_plan_schema(),
            fanout_id: "legacy".to_string(),
            cooks: vec![BatchCookSpec {
                cook_id: "issue-6453".to_string(),
                ..plan.cooks[0].clone()
            }],
            metadata: Value::Null,
        };
        legacy.rekey("fresh".to_string());
        assert_eq!(legacy.cooks[0].cook_id, "fresh-issue-6453");
    }

    #[test]
    fn recipe_preflight_accepts_exact_replay_and_rejects_changed_inputs() {
        with_isolated_home(|_| {
            let plan = test_batch_plan();
            let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");
            let mut invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            invocation.options.harvest_context = batch_harvest_context().expect("harvest context");
            invocation.options.initial_plan = compiled[0].initial_plan.clone();
            agent_task_service::persist_initial_recipe(&invocation.options)
                .expect("persist initial recipe");

            if let Err(error) = preflight_batch_cook_recipes(&plan, None) {
                panic!("exact replay is incompatible: {error:?}");
            }

            let mut changed = plan;
            changed.cooks[0].title = Some("changed title".to_string());
            let error = preflight_batch_cook_recipes(&changed, None)
                .expect_err("changed execution inputs conflict");
            assert!(error.message.contains("different execution inputs"));
        });
    }

    #[test]
    fn executing_batch_fails_early_for_a_backend_without_a_provider() {
        // #7717: an executing batch whose backend cannot be served by any
        // installed provider must fail early with an actionable configuration
        // error, not ride the backend all the way to a late provider-shaped
        // child failure. The test environment installs no providers, so any
        // backend is unresolved — which is exactly the "no installed provider"
        // path.
        let mut args = cook_batch_args();
        args.backend = Some("codebox-nonexistent".to_string());
        args.selector = None;
        args.dry_run = false;
        args.run_plan = true;

        let error = resolve_and_validate_effective_backend(&mut args)
            .expect_err("an executing batch with an unresolved backend must fail early");
        assert_eq!(error.details["field"], "input");
        assert!(
            error.message.contains("codebox-nonexistent")
                && error.message.contains("no installed provider"),
            "error must name the backend and the missing provider: {}",
            error.message
        );
        assert!(
            error.message.contains("--backend"),
            "error must be actionable: {}",
            error.message
        );
    }

    #[test]
    fn dry_run_batch_pins_the_backend_without_requiring_a_provider() {
        // Dry-run/planning must not require an installed provider — it only
        // builds the plan. The effective backend is still pinned so it is
        // visible and carried consistently.
        let mut args = cook_batch_args();
        args.backend = Some("sandbox".to_string());
        args.dry_run = true;
        args.run_plan = false;

        resolve_and_validate_effective_backend(&mut args)
            .expect("dry-run planning must not require an installed provider");
        assert_eq!(args.backend.as_deref(), Some("sandbox"));
    }

    #[test]
    fn cook_batch_preserves_explicit_prompt_template_for_manual_pr_modes() {
        let mut args = cook_batch_args();
        args.prompt_template = Some(
            "Fix {issue_ref} on {branch}; push manually and open the pull request.".to_string(),
        );

        let plan = build_cook_batch_plan(&args).expect("cook batch plan");

        assert_eq!(
            plan.cooks[0].prompt.as_deref(),
            Some(
                "Fix Extra-Chill/homeboy#6453 on fix/issue-6453-homeboy; push manually and open the pull request."
            )
        );
    }

    #[test]
    fn cook_batch_dry_run_returns_status_and_resume_commands() {
        let (value, exit_code) = cook_batch(cook_batch_args()).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-cook-batch/v1");
        assert_eq!(value["status"], "planned");
        assert_eq!(value["summary"]["issues"], 2);
        assert_eq!(
            value["preflight"]["provider_selection"]["executor"]["backend"],
            "sandbox"
        );
        assert_eq!(value["preflight"]["provider_selection"]["model"], "gpt-5.5");
        assert_eq!(
            value["preflight"]["provider_selection"]["provider_config"],
            "provided"
        );
        assert_eq!(value["worktrees"]["dry_run"], true);
        assert_eq!(value["worktrees"]["rows"][0]["status"], "queued");
        assert!(value["commands"]["resume_from_plan"]
            .as_str()
            .expect("resume command")
            .contains("fanout run-plan"));
    }

    #[test]
    fn cook_batch_unknown_provider_profile_warns_without_core_defaults() {
        let mut args = cook_batch_args();
        args.backend = None;
        args.model = None;
        args.provider_profile = Some("example-profile".to_string());

        let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["preflight"]["provider_selection"]["profile"],
            "example-profile"
        );
        assert!(value["preflight"]["provider_selection"]["warnings"][0]
            .as_str()
            .expect("warning")
            .contains("not declared"));
    }

    #[test]
    fn cook_batch_does_not_warn_for_specific_backend_names_in_core() {
        let mut args = cook_batch_args();
        args.backend = Some("example".to_string());
        args.provider_config = None;

        let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert!(value["preflight"]["provider_selection"]["warnings"]
            .as_array()
            .expect("warnings")
            .is_empty());
    }

    #[test]
    fn cook_batch_cli_parses_multiple_issues_and_gates() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--provider-profile",
            "opencode-codex-gpt55",
            "--verify",
            "cargo test --lib",
            "https://github.com/Extra-Chill/homeboy/issues/6453",
            "https://github.com/Extra-Chill/homeboy/issues/6454",
        ])
        .expect("cook-batch parses");

        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::CookBatch(args) = fanout.command else {
            panic!("cook-batch command");
        };
        assert_eq!(args.issues.len(), 2);
        assert_eq!(args.gates.verify, vec!["cargo test --lib"]);
        assert_eq!(args.from, "origin/main");
        assert_eq!(
            args.provider_profile,
            Some("opencode-codex-gpt55".to_string())
        );
    }
}
