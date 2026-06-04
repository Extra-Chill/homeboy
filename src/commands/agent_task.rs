use clap::{Args, Subcommand};
use serde_json::Value;

use homeboy::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_promotion::{promote, AgentTaskPromotionOptions};
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskRetryPolicy, AgentTaskScheduler,
};
use homeboy::core::config;

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Build and dispatch common repo-cooking agent tasks without hand-authored provider JSON.
    Dispatch(DispatchArgs),
    /// Run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable agent-task run.
    Run(StatusArgs),
    /// Claim and execute the oldest queued durable agent-task run.
    RunNext,
    /// Persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Read durable agent-task run status.
    Status(StatusArgs),
    /// Read durable agent-task run scheduler events.
    Logs(StatusArgs),
    /// List artifacts and evidence refs recorded for a completed run.
    Artifacts(StatusArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(PromoteArgs),
    /// List extension-declared agent-task executor providers.
    Providers,
}

#[derive(Args, Debug)]
pub struct DispatchArgs {
    /// Inline prompt, @file, or - for stdin. Repeat --task for waves.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Additional task prompt, @file, or - for stdin. Each --task creates one minion cell.
    #[arg(long = "task", value_name = "PROMPT")]
    pub tasks: Vec<String>,

    /// JSON array/object of task prompts for waves. Supports @file and - for stdin.
    #[arg(long = "tasks", value_name = "JSON")]
    pub tasks_json: Option<String>,

    /// Existing local repo checkout or DMC worktree path to cook in.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<String>,

    /// DMC worktree handle, e.g. data-machine@fix-runtime-inline-agent-bundle-import.
    #[arg(long = "worktree", value_name = "HANDLE")]
    pub dmc_worktree: Option<String>,

    /// Repo/component slug for metadata and task grouping, e.g. data-machine.
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,

    /// Issue, PR, or tracker URL the task is cooking.
    #[arg(long, value_name = "URL")]
    pub task_url: Option<String>,

    /// Discord channel id for provider/UI routing. Does not require --user.
    #[arg(long, value_name = "CHANNEL_ID")]
    pub channel: Option<String>,

    /// Optional Discord thread id for provider/UI routing.
    #[arg(long, value_name = "THREAD_ID")]
    pub thread: Option<String>,

    /// Executor backend to request. Defaults to the Codebox coding backend.
    #[arg(long, default_value = "codebox", value_name = "BACKEND")]
    pub backend: String,

    /// Optional provider selector/id when more than one backend provider exists.
    #[arg(long, value_name = "SELECTOR")]
    pub selector: Option<String>,

    /// Optional model override passed through to the provider.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Secret environment variable name to hydrate for the provider. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,

    /// Provider config JSON object, @file, or - for stdin. Merged with routing/workspace metadata.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,

    /// Maximum number of task cells to run at once.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub concurrency: usize,

    /// Attempts per task, including the first attempt.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempts: u32,

    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,

    /// Persist the run for a daemon/runner but do not execute immediately.
    #[arg(long)]
    pub queue_only: bool,
}

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Also persist the completed run lifecycle record under this id.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct PromoteArgs {
    /// AgentTaskOutcome or AgentTaskAggregate JSON file, @file, or - for stdin.
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Managed DMC worktree handle to apply into, e.g. repo@branch-slug.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,

    /// Outcome task id to select when SOURCE is an aggregate.
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,

    /// Patch artifact id to select when the outcome contains multiple patches.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,

    /// Validate and report the selected promotion without creating/applying.
    #[arg(long)]
    pub dry_run: bool,

    /// Verification command to run in the promoted worktree after apply.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,
}

pub fn run(args: AgentTaskArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Dispatch(dispatch_args) => dispatch(dispatch_args),
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run_submitted(status_args),
        AgentTaskCommand::RunNext => run_next(),
        AgentTaskCommand::Submit(submit_args) => submit(submit_args),
        AgentTaskCommand::Status(status_args) => status(status_args),
        AgentTaskCommand::Logs(status_args) => logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => artifacts(status_args),
        AgentTaskCommand::Promote(promote_args) => promote_artifact(promote_args),
        AgentTaskCommand::Providers => providers(),
    }
}

fn dispatch(args: DispatchArgs) -> CmdResult<Value> {
    dispatch_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

fn dispatch_with_executor<E>(args: DispatchArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = build_dispatch_plan(&args)?;
    let submitted = agent_task_lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
    let run_id = submitted.run_id.clone();

    if args.queue_only {
        return Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-dispatch/v1",
                "run_id": submitted.run_id,
                "plan_id": submitted.plan_id,
                "state": submitted.state,
                "plan_path": submitted.plan_path,
                "task_count": submitted.tasks.len(),
                "queued": true,
                "record": submitted,
            }),
            0,
        ));
    }

    agent_task_lifecycle::mark_running(&run_id)?;
    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    let record = agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };

    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-dispatch/v1",
            "run_id": record.run_id,
            "plan_id": record.plan_id,
            "state": record.state,
            "plan_path": record.plan_path,
            "aggregate_path": record.aggregate_path,
            "task_count": record.tasks.len(),
            "queued": false,
            "record": record,
            "aggregate": aggregate,
        }),
        exit_code,
    ))
}

fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_loaded_plan<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    normalize_plan_workspaces(&mut plan)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        exit_code,
    ))
}

fn run_submitted(args: StatusArgs) -> CmdResult<Value> {
    run_submitted_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_submitted_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_running(&run_id)?;
    run_claimed(run_id, executor)
}

fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };

    run_claimed(record.run_id, executor)
}

fn run_claimed<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        exit_code,
    ))
}

fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    let record = agent_task_lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn status(args: StatusArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::status(&args.run_id)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_lifecycle::logs(&args.run_id)?;
    Ok((serde_json::to_value(log).unwrap_or(Value::Null), 0))
}

fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_lifecycle::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

fn promote_artifact(args: PromoteArgs) -> CmdResult<Value> {
    let (raw, source_path) = read_promotion_source(&args.source)?;
    let report = promote(AgentTaskPromotionOptions {
        source: raw,
        source_path,
        to_worktree: args.to_worktree,
        task_id: args.task_id,
        artifact_id: args.artifact_id,
        dry_run: args.dry_run,
        verify: args.verify,
    })?;

    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

fn read_promotion_source(
    spec: &str,
) -> homeboy::core::Result<(String, Option<std::path::PathBuf>)> {
    if spec != "-" {
        let path = std::path::PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

fn source_spec_path(spec: &str) -> Option<std::path::PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(std::path::PathBuf::from(
        spec.strip_prefix('@').unwrap_or(spec),
    ))
}

fn providers() -> CmdResult<Value> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    Ok((
        serde_json::to_value(executor.providers()).unwrap_or(Value::Null),
        0,
    ))
}

fn build_dispatch_plan(args: &DispatchArgs) -> homeboy::core::Result<AgentTaskPlan> {
    if args.prompt.is_none() && args.tasks.is_empty() && args.tasks_json.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "prompt",
            "agent-task dispatch requires --prompt, --prompt @file, --prompt -, repeated --task inputs, or --tasks @tasks.json",
            None,
            Some(vec![
                "Example: homeboy agent-task dispatch --repo data-machine --cwd /path/to/worktree --channel 1493345787894038649 --prompt @task.txt".to_string(),
                "Wave input: homeboy agent-task dispatch --tasks @tasks.json --concurrency 8 --channel 1493345787894038649".to_string(),
            ]),
        ));
    }

    let workspace_root = resolve_dispatch_workspace(args)?;
    let repo = args
        .repo
        .clone()
        .or_else(|| {
            args.dmc_worktree
                .as_ref()
                .and_then(|handle| handle.split('@').next().map(str::to_string))
        })
        .or_else(|| {
            workspace_root
                .as_ref()
                .and_then(|root| root.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
        });
    let mut prompt_specs = Vec::new();
    if let Some(prompt) = &args.prompt {
        prompt_specs.push(prompt.clone());
    }
    prompt_specs.extend(args.tasks.clone());
    prompt_specs.extend(read_dispatch_tasks_json(args.tasks_json.as_deref())?);

    let routing = dispatch_routing(args);
    let provider_config = dispatch_provider_config(args, &repo, workspace_root.as_ref(), &routing)?;
    let mut tasks = Vec::new();
    for (index, prompt_spec) in prompt_specs.iter().enumerate() {
        let instructions = read_text_spec(prompt_spec, "prompt")?;
        let task_id = dispatch_task_id(repo.as_deref(), index);
        let mut source_refs = Vec::new();
        if let Some(task_url) = &args.task_url {
            source_refs.push(AgentTaskSourceRef {
                kind: "task".to_string(),
                uri: task_url.clone(),
                revision: None,
            });
        }

        tasks.push(AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id,
            group_key: repo.clone(),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: args.backend.clone(),
                selector: args.selector.clone(),
                required_capabilities: Vec::new(),
                secret_env: args.secret_env.clone(),
                model: args.model.clone(),
                config: provider_config.clone(),
            },
            instructions,
            inputs: Value::Null,
            source_refs,
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Existing,
                root: workspace_root
                    .as_ref()
                    .map(|path| path.display().to_string()),
                slug: repo.clone(),
                kind: args
                    .dmc_worktree
                    .as_ref()
                    .map(|_| "dmc-worktree".to_string()),
                component_id: None,
                branch: None,
                base_ref: None,
                task_url: args.task_url.clone(),
                cleanup: Some("preserve".to_string()),
                materialization: serde_json::json!({
                    "dmc_worktree": args.dmc_worktree,
                    "repo": repo,
                }),
            },
            policy: AgentTaskPolicy {
                read: "workspace".to_string(),
                write: "patch".to_string(),
                apply: "manual".to_string(),
            },
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec![
                "patch".to_string(),
                "agent_result".to_string(),
                "transcript".to_string(),
            ],
            metadata: serde_json::json!({
                "repo": repo,
                "dmc_worktree": args.dmc_worktree,
                "routing": routing,
                "task_url": args.task_url,
                "prompt_source": prompt_spec,
                "dispatch": "agent-task dispatch",
            }),
        });
    }

    let mut plan = AgentTaskPlan::new(
        format!("agent-task-dispatch-{}", uuid::Uuid::new_v4()),
        tasks,
    );
    plan.group_key = repo.clone();
    plan.options.max_concurrency = args.concurrency.max(1);
    plan.options.retry = AgentTaskRetryPolicy {
        max_attempts: args.attempts.max(1),
        ..AgentTaskRetryPolicy::default()
    };
    plan.metadata = serde_json::json!({
        "kind": "repo-cooking-dispatch",
        "repo": repo,
        "dmc_worktree": args.dmc_worktree,
        "workspace_root": workspace_root.map(|path| path.display().to_string()),
        "routing": routing,
        "task_url": args.task_url,
    });

    Ok(plan)
}

fn resolve_dispatch_workspace(
    args: &DispatchArgs,
) -> homeboy::core::Result<Option<std::path::PathBuf>> {
    if let Some(cwd) = &args.cwd {
        let path = std::path::PathBuf::from(cwd);
        if !path.is_dir() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "cwd",
                format!(
                    "agent-task dispatch workspace '{}' is not a directory",
                    path.display()
                ),
                Some(cwd.clone()),
                None,
            ));
        }
        return Ok(Some(path));
    }

    let Some(handle) = &args.dmc_worktree else {
        return Ok(None);
    };

    let root = std::env::var("HOMEBOY_DMC_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|home| std::path::PathBuf::from(home).join("Developer"))
                .unwrap_or_else(|_| std::path::PathBuf::from("/Users/chubes/Developer"))
        });
    let path = root.join(handle);
    if !path.is_dir() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "worktree",
            format!(
                "DMC worktree handle '{}' did not resolve to an existing directory at {}; pass --cwd explicitly if it lives elsewhere",
                handle,
                path.display()
            ),
            Some(handle.clone()),
            None,
        ));
    }

    Ok(Some(path))
}

fn dispatch_routing(args: &DispatchArgs) -> Value {
    serde_json::json!({
        "client": if args.channel.is_some() { "discord" } else { "cli" },
        "channel": args.channel,
        "thread": args.thread,
        "user_required": false,
        "ui": if args.channel.is_some() { "kimaki" } else { "cli" },
    })
}

fn dispatch_provider_config(
    args: &DispatchArgs,
    repo: &Option<String>,
    workspace_root: Option<&std::path::PathBuf>,
    routing: &Value,
) -> homeboy::core::Result<Value> {
    let mut config = if let Some(spec) = &args.provider_config {
        let raw = read_text_spec(spec, "provider-config")?;
        serde_json::from_str::<Value>(&raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task dispatch provider config".to_string()),
                Some(raw),
            )
        })?
    } else {
        serde_json::json!({})
    };

    if !config.is_object() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-config",
            "agent-task dispatch --provider-config must resolve to a JSON object",
            None,
            None,
        ));
    }

    let map = config.as_object_mut().expect("provider config object");
    map.entry("task_kind".to_string())
        .or_insert_with(|| serde_json::json!("repo-cooking"));
    map.entry("repo".to_string())
        .or_insert_with(|| serde_json::json!(repo));
    map.entry("dmc_worktree".to_string())
        .or_insert_with(|| serde_json::json!(args.dmc_worktree));
    map.entry("workspace_root".to_string()).or_insert_with(|| {
        serde_json::json!(workspace_root.map(|path| path.display().to_string()))
    });
    map.entry("routing".to_string())
        .or_insert_with(|| routing.clone());
    map.entry("task_url".to_string())
        .or_insert_with(|| serde_json::json!(args.task_url));

    Ok(config)
}

fn read_text_spec(spec: &str, label: &str) -> homeboy::core::Result<String> {
    config::read_json_spec_to_string(spec).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to read agent-task dispatch {label} input: {error}"
        ))
    })
}

fn read_dispatch_tasks_json(spec: Option<&str>) -> homeboy::core::Result<Vec<String>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };

    let raw = read_text_spec(spec, "tasks")?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task dispatch tasks".to_string()),
            Some(raw.clone()),
        )
    })?;

    match value {
        Value::Array(items) => task_prompts_from_json_items(items),
        Value::Object(mut object) => match object.remove("tasks") {
            Some(Value::Array(items)) => task_prompts_from_json_items(items),
            _ => Err(invalid_tasks_json_error()),
        },
        _ => Err(invalid_tasks_json_error()),
    }
}

fn task_prompts_from_json_items(items: Vec<Value>) -> homeboy::core::Result<Vec<String>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| task_prompt_from_json_item(item, index))
        .collect()
}

fn invalid_tasks_json_error() -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "tasks",
        "agent-task dispatch --tasks expects a JSON array or object with a tasks array",
        None,
        None,
    )
}

fn task_prompt_from_json_item(item: Value, index: usize) -> homeboy::core::Result<String> {
    match item {
        Value::String(prompt) => Ok(prompt),
        Value::Object(mut object) => object
            .remove("prompt")
            .or_else(|| object.remove("instructions"))
            .and_then(|value| value.as_str().map(str::to_string))
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "tasks",
                    format!(
                        "agent-task dispatch task item {} must include a string prompt or instructions field",
                        index + 1
                    ),
                    None,
                    None,
                )
            }),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "tasks",
            format!(
                "agent-task dispatch task item {} must be a string or object",
                index + 1
            ),
            None,
            None,
        )),
    }
}

fn dispatch_task_id(repo: Option<&str>, index: usize) -> String {
    let slug = repo
        .map(sanitize_slug)
        .unwrap_or_else(|| "task".to_string());
    if index == 0 {
        format!("cook-{slug}")
    } else {
        format!("cook-{slug}-{}", index + 1)
    }
}

fn sanitize_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

fn read_plan(spec: &str) -> homeboy::core::Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> homeboy::core::Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn normalize_component_worktree_workspace(
    request: &mut AgentTaskRequest,
) -> homeboy::core::Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(homeboy::core::Error::validation_invalid_argument(
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
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workspace.root",
            format!(
                "agent-task task '{}' requested component-worktree workspace for component '{}' but no resolved root was provided; creating component worktrees depends on the generic Homeboy worktree primitive tracked by Extra-Chill/homeboy#3362",
                request.task_id, component_id
            ),
            None,
            None,
        ));
    };

    request.workspace.kind = None;
    request.workspace.mode = homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing;
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

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy::core::agent_task_lifecycle::{
        status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
    };
    use homeboy::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[test]
    fn submit_run_status_reports_terminal_state() {
        with_temp_home(|| {
            let plan = AgentTaskPlan::new(
                "plan-cli-terminal",
                vec![AgentTaskRequest {
                    schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "task-cli-terminal".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "missing-provider-test".to_string(),
                        selector: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "exercise durable terminal status".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            let plan_file = tempfile::NamedTempFile::new().expect("plan file");
            std::fs::write(
                plan_file.path(),
                serde_json::to_string(&plan).expect("plan json"),
            )
            .expect("write plan");
            let plan_path = format!("@{}", plan_file.path().display());

            submit(SubmitArgs {
                plan: plan_path,
                run_id: Some("run-cli-terminal".to_string()),
            })
            .expect("submitted");
            let (_, run_exit_code) = run_submitted(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("run completed");
            let (status_json, status_exit_code) = status(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("status loaded");
            let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

            assert_eq!(run_exit_code, 1);
            assert_eq!(status_exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Failed);
            assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
            assert_eq!(record.totals.expect("totals").failed, 1);
        });
    }

    #[test]
    fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
        with_temp_home(|| {
            let run_id = "run-plan-durable";
            let observed_status = Arc::new(Mutex::new(None));
            let executor = InspectingExecutor {
                run_id: run_id.to_string(),
                observed_status: Arc::clone(&observed_status),
            };

            let (_value, exit_code) =
                run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed durable status");
            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(observed.tasks[0].state, AgentTaskState::Running);
            assert_eq!(observed.metadata["runner_pid"], std::process::id());
            assert!(observed.aggregate_path.is_none());

            let completed = lifecycle_status(run_id).expect("completed status loaded");
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
            assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
            assert!(completed.aggregate_path.is_some());
        });
    }

    #[test]
    fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
                .expect("first submitted");
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
                .expect("second submitted");
            let observed_status = Arc::new(Mutex::new(None));

            let (_value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "run-next-a".to_string(),
                observed_status: Arc::clone(&observed_status),
            })
            .expect("claimed run completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed claimed status");
            let first = lifecycle_status("run-next-a").expect("first status");
            let second = lifecycle_status("run-next-b").expect("second status");

            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(first.state, AgentTaskRunState::Succeeded);
            assert_eq!(second.state, AgentTaskRunState::Queued);
        });
    }

    #[test]
    fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
        with_temp_home(|| {
            let (value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "unused".to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            })
            .expect("run-next checked queue");

            assert_eq!(exit_code, 0);
            assert_eq!(value["claimed"], false);
        });
    }

    #[test]
    fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
        let observed_request = Arc::new(Mutex::new(None));
        let executor = CapturingExecutor {
            observed_request: Arc::clone(&observed_request),
        };
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());
        plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
        plan.tasks[0].workspace.task_url =
            Some("https://github.com/Extra-Chill/wp-coding-agents/issues/179".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].workspace.materialization = json!({
            "root": "/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance"
        });

        let (_value, exit_code) =
            run_loaded_plan(plan, None, executor).expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 0);
        assert_eq!(
            observed.workspace.mode,
            homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing
        );
        assert_eq!(
            observed.workspace.root.as_deref(),
            Some("/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance")
        );
        assert_eq!(observed.workspace.slug.as_deref(), Some("wp-coding-agents"));
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert!(observed.workspace.branch.is_none());
        assert!(observed.workspace.base_ref.is_none());
        assert!(observed.workspace.task_url.is_none());
        assert!(observed.workspace.cleanup.is_none());
        assert!(observed.workspace.materialization.is_null());
    }

    #[test]
    fn run_plan_rejects_unresolved_component_worktree_until_core_primitive_exists() {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());

        let error = run_loaded_plan(plan, None, CapturingExecutor::default())
            .expect_err("unresolved component worktree rejected");
        let message = error.to_string();

        assert!(message.contains("component-worktree workspace"));
        assert!(message.contains("Extra-Chill/homeboy#3362"));
    }

    #[test]
    fn promotion_source_resolves_completed_run_id() {
        with_temp_home(|| {
            let run_id = "run-promotion-source";

            run_loaded_plan(test_plan(), Some(run_id), InspectingExecutor::noop(run_id))
                .expect("run completed");

            let (raw, path) = read_promotion_source(run_id).expect("promotion source resolved");

            assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
            assert_eq!(
                path.as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str()),
                Some("aggregate.json")
            );
        });
    }

    #[test]
    fn promotion_source_reads_bare_json_file_path() {
        let file = tempfile::NamedTempFile::new().expect("source file");
        std::fs::write(
            file.path(),
            r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
        )
        .expect("write source");

        let (raw, path) = read_promotion_source(&file.path().display().to_string())
            .expect("promotion source file resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(path.as_deref(), Some(file.path()));
    }

    #[test]
    fn dispatch_builds_repo_cooking_plan_from_prompt_file_and_channel() {
        let workspace = tempfile::tempdir().expect("workspace");
        let prompt = tempfile::NamedTempFile::new().expect("prompt file");
        std::fs::write(prompt.path(), "Cook the issue cleanly.").expect("write prompt");

        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some(format!("@{}", prompt.path().display())),
            cwd: Some(workspace.path().display().to_string()),
            repo: Some("data-machine".to_string()),
            channel: Some("1493345787894038649".to_string()),
            ..DispatchArgOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.group_key.as_deref(), Some("data-machine"));
        assert_eq!(plan.tasks[0].task_id, "cook-data-machine");
        assert_eq!(plan.tasks[0].instructions, "Cook the issue cleanly.");
        assert_eq!(
            plan.tasks[0].workspace.mode,
            AgentTaskWorkspaceMode::Existing
        );
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.path().to_str().expect("workspace utf8"))
        );
        assert_eq!(
            plan.tasks[0].executor.config["routing"]["channel"],
            "1493345787894038649"
        );
        assert_eq!(
            plan.tasks[0].executor.config["routing"]["user_required"],
            false
        );
    }

    #[test]
    fn dispatch_accepts_tasks_json_wave_input() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(
            tasks.path(),
            r#"{"tasks":["Cook issue A",{"prompt":"Cook issue B"}]}"#,
        )
        .expect("write tasks");

        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            tasks_json: Some(format!("@{}", tasks.path().display())),
            repo: Some("homeboy".to_string()),
            concurrency: 8,
            attempts: 3,
            ..DispatchArgOverrides::default()
        }))
        .expect("dispatch wave plan");

        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.options.max_concurrency, 8);
        assert_eq!(plan.options.retry.max_attempts, 3);
        assert_eq!(plan.tasks[0].task_id, "cook-homeboy");
        assert_eq!(plan.tasks[1].task_id, "cook-homeboy-2");
        assert_eq!(plan.tasks[0].instructions, "Cook issue A");
        assert_eq!(plan.tasks[1].instructions, "Cook issue B");
    }

    #[test]
    fn dispatch_queue_only_returns_durable_run_without_executing() {
        with_temp_home(|| {
            let workspace = tempfile::tempdir().expect("workspace");

            let (value, exit_code) = dispatch_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Queue this minion.".to_string()),
                    cwd: Some(workspace.path().display().to_string()),
                    queue_only: true,
                    run_id: Some("dispatch-queued".to_string()),
                    ..DispatchArgOverrides::default()
                }),
                CapturingExecutor::default(),
            )
            .expect("dispatch queued");

            assert_eq!(exit_code, 0);
            assert_eq!(value["run_id"], "dispatch-queued");
            assert_eq!(value["queued"], true);
            assert_eq!(value["state"], "queued");
            assert!(value["plan_path"]
                .as_str()
                .expect("plan path")
                .ends_with("plan.json"));
        });
    }

    #[test]
    fn dispatch_resolves_dmc_worktree_handle_under_configured_root() {
        let root = tempfile::tempdir().expect("dmc root");
        let worktree = root.path().join("data-machine@fix-runtime");
        std::fs::create_dir(&worktree).expect("worktree dir");
        std::env::set_var("HOMEBOY_DMC_WORKSPACE_ROOT", root.path());

        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some("Cook DMC worktree.".to_string()),
            dmc_worktree: Some("data-machine@fix-runtime".to_string()),
            ..DispatchArgOverrides::default()
        }))
        .expect("dispatch dmc plan");

        assert_eq!(plan.group_key.as_deref(), Some("data-machine"));
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(worktree.to_str().expect("worktree utf8"))
        );
        assert_eq!(
            plan.tasks[0].workspace.kind.as_deref(),
            Some("dmc-worktree")
        );

        std::env::remove_var("HOMEBOY_DMC_WORKSPACE_ROOT");
    }

    struct InspectingExecutor {
        run_id: String,
        observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
    }

    impl InspectingExecutor {
        fn noop(run_id: &str) -> Self {
            Self {
                run_id: run_id.to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl AgentTaskExecutorAdapter for InspectingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let record =
                lifecycle_status(&self.run_id).expect("status exists before executor runs");
            *self
                .observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record);

            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    #[derive(Default)]
    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());

            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn with_temp_home(run: impl FnOnce()) {
        with_isolated_home(|_| run());
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "plan-a",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("fixture".to_string()),
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    #[derive(Default)]
    struct DispatchArgOverrides {
        prompt: Option<String>,
        tasks_json: Option<String>,
        cwd: Option<String>,
        dmc_worktree: Option<String>,
        repo: Option<String>,
        channel: Option<String>,
        concurrency: usize,
        attempts: u32,
        queue_only: bool,
        run_id: Option<String>,
    }

    fn dispatch_args(overrides: DispatchArgOverrides) -> DispatchArgs {
        DispatchArgs {
            prompt: overrides.prompt,
            tasks: Vec::new(),
            tasks_json: overrides.tasks_json,
            cwd: overrides.cwd,
            dmc_worktree: overrides.dmc_worktree,
            repo: overrides.repo,
            task_url: None,
            channel: overrides.channel,
            thread: None,
            backend: "fixture".to_string(),
            selector: None,
            model: None,
            secret_env: Vec::new(),
            provider_config: None,
            concurrency: if overrides.concurrency == 0 {
                1
            } else {
                overrides.concurrency
            },
            attempts: if overrides.attempts == 0 {
                1
            } else {
                overrides.attempts
            },
            run_id: overrides.run_id,
            queue_only: overrides.queue_only,
        }
    }
}
