use clap::Args;
use serde_json::Value;

use homeboy::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskRetryPolicy, AgentTaskScheduler,
};
use homeboy::core::config;

use super::CmdResult;

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

pub fn dispatch(args: DispatchArgs) -> CmdResult<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::{
        AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
    };
    use homeboy::core::agent_task_scheduler::AgentTaskExecutionContext;

    #[test]
    fn builds_repo_cooking_plan_from_prompt_file_and_channel() {
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
    fn accepts_tasks_json_wave_input() {
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
    fn queue_only_returns_durable_run_without_executing() {
        with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");

            let (value, exit_code) = dispatch_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Queue this minion.".to_string()),
                    cwd: Some(workspace.path().display().to_string()),
                    queue_only: true,
                    run_id: Some("dispatch-queued".to_string()),
                    ..DispatchArgOverrides::default()
                }),
                NoopExecutor,
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
    fn resolves_dmc_worktree_handle_under_configured_root() {
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

    struct NoopExecutor;

    impl AgentTaskExecutorAdapter for NoopExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
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
