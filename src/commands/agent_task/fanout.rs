//! Public batch-cook fanout command handlers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy::core::agent_tasks::dispatch_service::{self, AgentTaskDispatchCommand, DispatchCoreInputs};
use homeboy::core::agent_tasks::gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};
use homeboy::core::agent_tasks::provider;
use homeboy::core::agent_tasks::service::{self as agent_task_service, AgentTaskCookServiceOptions};
use homeboy::core::agent_tasks::{
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
    AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
};
use homeboy::core::{config, Error, Result};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutCommand, AgentTaskFanoutInputArgs,
    AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs,
};
use super::command_json_value;

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::Plan(plan_args) => {
            let plan = load_batch_cook_fanout_plan(&plan_args.input)?;
            Ok((command_json_value(plan)?, 0))
        }
        AgentTaskFanoutCommand::Submit(submit_args) => submit_batch_cook_fanout(submit_args),
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

fn run_batch_cook_fanout(args: AgentTaskFanoutRunPlanArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
    if let Some(record_run_id) = args.record_run_id {
        plan.fanout_id = record_run_id;
    }
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
        let title = self.title.clone().unwrap_or_else(|| default_cook_title(self));
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
                ai_model: self.model.clone().or_else(|| ai_model_from_tool(&self.ai_tool)),
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

fn client_context(plan: &BatchCookFanoutPlan, cook: &BatchCookSpec) -> String {
    serde_json::json!({
        "fanout": {
            "id": plan.fanout_id,
            "semantics": "batch_cook",
            "cook_id": cook.cook_id,
            "to_worktree": cook.to_worktree,
            "head": cook.head,
        }
    })
    .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(invocation.dispatch.run_id.as_deref(), Some("cook-5929-docs"));
        assert_eq!(invocation.options.gates.verify, vec!["homeboy test homeboy"]);
        assert!(invocation
            .dispatch
            .core
            .client_context
            .as_deref()
            .expect("client context")
            .contains("batch_cook"));
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
}
