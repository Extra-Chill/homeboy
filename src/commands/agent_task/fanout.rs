//! Public batch-cook fanout command handlers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use homeboy::core::agent_tasks::batch;
use homeboy::core::agent_tasks::dispatch_service::{
    self, AgentTaskDispatchCommand, DispatchCoreInputs,
};
use homeboy::core::agent_tasks::gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};
use homeboy::core::agent_tasks::provider;
use homeboy::core::agent_tasks::service::{
    self as agent_task_service, AgentTaskCookServiceOptions,
};
use homeboy::core::agent_tasks::{
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
        AgentTaskFanoutCommand::Artifacts(status_args) => batch_artifacts(status_args),
        AgentTaskFanoutCommand::RunPlan(run_args) => run_batch_cook_fanout(run_args),
    }
}

fn submit_batch_cook_fanout(args: AgentTaskFanoutSubmitArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(run_id) = args.run_id {
        plan.fanout_id = run_id;
    }

    let cooks = plan
        .cooks
        .iter()
        .map(|cook| {
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
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

fn batch_artifacts(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    Ok((command_json_value(batch::artifacts(&args.batch_id)?)?, 0))
}

fn run_batch_cook_fanout(args: AgentTaskFanoutRunPlanArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(record_run_id) = args.record_run_id {
        plan.fanout_id = record_run_id;
    }
    run_batch_cook_fanout_plan(plan)
}

fn run_batch_cook_fanout_plan(plan: BatchCookFanoutPlan) -> CmdResult<Value> {
    let executor = provider::ExtensionProviderAgentTaskExecutor::discover();
    let mut results = Vec::new();
    let mut failed = 0usize;

    for cook in &plan.cooks {
        let invocation = cook.to_cook_invocation(&plan)?;
        let (dispatch_value, _dispatch_exit) =
            dispatch_service::run_dispatch_command(invocation.dispatch, executor.clone())?;
        let run_id = dispatch_value["run_id"]
            .as_str()
            .ok_or_else(|| {
                Error::internal_unexpected(
                    "agent-task dispatch did not return a run_id".to_string(),
                )
            })?
            .to_string();
        let mut options = invocation.options;
        options.initial_run_id = run_id;
        let result = agent_task_service::run_cook(options, executor.clone())?;
        let value = serde_json::to_value(result.value).unwrap_or(Value::Null);
        let exit_code = result.exit_code;
        if exit_code != 0 {
            failed += 1;
        }
        results.push(serde_json::json!({
            "cook_id": cook.cook_id,
            "run_id": cook.run_id(),
            "worktree": cook.to_worktree,
            "head": cook.head,
            "workspace_materialization": cook.workspace_materialization,
            "exit_code": exit_code,
            "result": value,
        }));
    }

    Ok((
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
            "fanout_id": plan.fanout_id,
            "status": if failed == 0 { "succeeded" } else { "failed" },
            "summary": {
                "total": results.len(),
                "succeeded": results.len() - failed,
                "failed": failed,
            },
            "cooks": results,
        }),
        if failed == 0 { 0 } else { 1 },
    ))
}

fn cook_batch(args: AgentTaskFanoutCookBatchArgs) -> CmdResult<Value> {
    if !args.gates.has_deterministic_gate() {
        return Err(invalid_fanout(
            "agent-task fanout cook-batch requires --verify or --private-verify",
        ));
    }
    let plan = build_cook_batch_plan(&args)?;
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
        let (value, exit_code) = run_batch_cook_fanout_plan(plan.clone())?;
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
    if args.dry_run {
        return worktree::queue_create(worktree::WorktreeQueueCreateOptions {
            repo: args.repo.clone(),
            branches: branches.iter().map(|(branch, _)| branch.clone()).collect(),
            from: args.from.clone(),
            task_url: None,
            task_ref: None,
            dry_run: true,
            retry_after_seconds: 30,
            dmc_bin: args.dmc_bin.clone(),
        });
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
                    command: dmc_add_command(args, branch),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(status.record.worktree_path),
                    error: None,
                });
            }
            _ => to_create.push(branch.clone()),
        }
    }

    let created = worktree::queue_create(worktree::WorktreeQueueCreateOptions {
        repo: args.repo.clone(),
        branches: to_create,
        from: args.from.clone(),
        task_url: None,
        task_ref: None,
        dry_run: false,
        retry_after_seconds: 30,
        dmc_bin: args.dmc_bin.clone(),
    })?;
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

fn load_fanout_agent_task_plan(
    args: &AgentTaskFanoutInputArgs,
) -> Result<homeboy::core::agent_tasks::scheduler::AgentTaskPlan> {
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
            plan.fanout_id = fanout_id.clone();
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
            workspace: self.workspace.clone(),
            repo: self.repo.clone(),
            task_url: self.task_url.clone(),
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            required_capabilities: Vec::new(),
            secret_env: self.secret_env.clone(),
            concurrency: self.concurrency,
            run_id: Some(self.run_id()),
            core: DispatchCoreInputs {
                tasks_json: None,
                provider_config: self.provider_config.clone(),
                client_context: Some(merged_client_context(plan, self)),
                attempts: self.attempts,
                queue_only: false,
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
        Ok(BatchCookInvocation {
            dispatch,
            options: AgentTaskCookServiceOptions {
                cook_id: self.cook_id.clone(),
                initial_run_id: self.run_id(),
                to_worktree: self.to_worktree.clone(),
                provider_command: self.provider_command.clone(),
                gates: VerifyGateOptions {
                    verify: self.verify.clone(),
                    private_verify: self.private_verify.clone(),
                    private_gate_reveal: self.private_gate_reveal,
                },
                max_attempts: self.max_attempts,
                no_finalize: self.no_finalize,
                base: self.base.clone(),
                head: self.head.clone(),
                title,
                commit_message,
                source_refs: self.task_url.clone().into_iter().collect(),
                protected_branches: self.protected_branches.clone(),
                ai_tool: self.ai_tool.clone(),
                ai_model: self
                    .model
                    .clone()
                    .or_else(|| ai_model_from_tool(&self.ai_tool)),
                ai_used_for: self.ai_used_for.clone(),
            },
        })
    }
}

fn ai_model_from_tool(ai_tool: &str) -> Option<String> {
    let start = ai_tool.find('(')?;
    let end = ai_tool[start + 1..].find(')')? + start + 1;
    let model = ai_tool[start + 1..end].trim();
    (!model.is_empty()).then(|| model.to_string())
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
            repo: Some(args.repo.clone()),
            task_url: Some(issue_url.clone()),
            backend: args.backend.clone(),
            selector: args.selector.clone(),
            model: args.model.clone(),
            secret_env: args.secret_env.clone(),
            attempts: 1,
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
    Ok(BatchCookFanoutPlan {
        schema: batch_cook_fanout_plan_schema(),
        fanout_id: args
            .fanout_id
            .clone()
            .unwrap_or_else(|| format!("cook-batch-{}-{}-{}", args.repo, first, cooks.len())),
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
        "Fix {issue_url}. Inspect the issue, implement the smallest correct change in {repo}, run the requested verification gates, push {branch}, and open/update the PR with reviewer-ready evidence.",
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

fn dmc_add_command(args: &AgentTaskFanoutCookBatchArgs, branch: &str) -> Vec<String> {
    vec![
        args.dmc_bin.clone(),
        "wp".to_string(),
        "datamachine-code".to_string(),
        "workspace".to_string(),
        "worktree".to_string(),
        "add".to_string(),
        args.repo.clone(),
        branch.to_string(),
        format!("--from={}", args.from),
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

fn default_ai_tool() -> String {
    "OpenCode (GPT-5.5)".to_string()
}

fn default_ai_used_for() -> String {
    "Drafted implementation and tests; Chris reviews and owns the change.".to_string()
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
    use clap::Parser;
    use serde_json::json;

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
                        "verify": ["homeboy test homeboy"]
                    },
                    {
                        "cook_id": "5929-cli",
                        "prompt": "fix cli",
                        "repo": "homeboy",
                        "to_worktree": "homeboy@fix-5929-cli",
                        "head": "fix/5929-cli",
                        "verify": ["homeboy test homeboy"]
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
        assert_eq!(
            invocation.dispatch.run_id.as_deref(),
            Some("cook-5929-docs")
        );
        assert_eq!(
            invocation.options.gates.verify,
            vec!["homeboy test homeboy"]
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
            backend: Some("codebox".to_string()),
            selector: Some("wordpress.codebox-agent-task-executor".to_string()),
            model: Some("gpt-5.5".to_string()),
            secret_env: vec!["AI_PROVIDER_OPENAI_CODEX_TOKEN".to_string()],
            provider_config: Some(r#"{"runtime":"opencode"}"#.to_string()),
            gates: super::super::args::VerifyGateArgs {
                verify: vec!["cargo test --lib".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
            },
            dry_run: true,
            run_plan: false,
            dmc_bin: "studio".to_string(),
        }
    }

    #[test]
    fn cook_batch_builds_batch_cook_plan_from_issue_urls() {
        let args = cook_batch_args();
        let plan = build_cook_batch_plan(&args).expect("cook batch plan");

        assert_eq!(plan.fanout_id, "issue-wave");
        assert_eq!(plan.cooks.len(), 2);
        assert_eq!(plan.cooks[0].cook_id, "issue-6453");
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
        assert_eq!(plan.cooks[0].verify, vec!["cargo test --lib"]);
        assert_eq!(plan.cooks[0].backend.as_deref(), Some("codebox"));
    }

    #[test]
    fn cook_batch_dry_run_returns_status_and_resume_commands() {
        let (value, exit_code) = cook_batch(cook_batch_args()).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-cook-batch/v1");
        assert_eq!(value["status"], "planned");
        assert_eq!(value["summary"]["issues"], 2);
        assert_eq!(value["worktrees"]["dry_run"], true);
        assert_eq!(value["worktrees"]["rows"][0]["status"], "queued");
        assert!(value["commands"]["resume_from_plan"]
            .as_str()
            .expect("resume command")
            .contains("fanout run-plan"));
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
    }
}
