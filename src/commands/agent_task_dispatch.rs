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
use homeboy::core::agent_task_secrets::validate_secret_env;
use homeboy::core::config;
use homeboy::core::worktree;

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug, Clone)]
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

    /// Existing local repo checkout or worktree path to cook in.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<String>,

    /// Homeboy workspace ID or existing local workspace path to cook in.
    #[arg(long, value_name = "ID_OR_PATH")]
    pub workspace: Option<String>,

    /// Repo/component slug for metadata and task grouping, e.g. data-machine.
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,

    /// Issue, PR, or tracker URL the task is cooking.
    #[arg(long, value_name = "URL")]
    pub task_url: Option<String>,

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

    /// Provider config JSON object, @file, or - for stdin. Merged with workspace metadata.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,

    /// Opaque client context JSON object, @file, or - for stdin.
    #[arg(long = "client-context", value_name = "JSON")]
    pub client_context: Option<String>,

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

pub fn run(args: DispatchArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    dispatch_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

pub(crate) fn dispatch_with_executor<E>(args: DispatchArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = build_dispatch_plan(&args)?;
    preflight_dispatch_provider_secrets(&plan)?;
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
                "Example: homeboy agent-task dispatch --repo data-machine --cwd /path/to/worktree --prompt @task.txt".to_string(),
                "Wave input: homeboy agent-task dispatch --tasks @tasks.json --concurrency 8".to_string(),
            ]),
        ));
    }

    let workspace_target = resolve_dispatch_workspace(args)?;
    let workspace_root = workspace_target.as_ref().map(|target| target.root.clone());
    let repo = args
        .repo
        .clone()
        .or_else(|| {
            workspace_target
                .as_ref()
                .and_then(|target| target.component_id.clone())
        })
        .or_else(|| {
            workspace_target
                .as_ref()
                .and_then(|target| target.slug.clone())
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

    let client_context = dispatch_client_context(args)?;
    let provider_config =
        dispatch_provider_config(args, &repo, workspace_target.as_ref(), &client_context)?;
    let mut tasks = Vec::new();
    for (index, prompt_spec) in prompt_specs.iter().enumerate() {
        let instructions = dispatch_instructions(
            read_text_spec(prompt_spec, "prompt")?,
            args.task_url.as_deref(),
        );
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
                kind: workspace_target
                    .as_ref()
                    .and_then(|target| target.kind.clone()),
                component_id: workspace_target
                    .as_ref()
                    .and_then(|target| target.component_id.clone()),
                branch: workspace_target
                    .as_ref()
                    .and_then(|target| target.branch.clone()),
                base_ref: workspace_target
                    .as_ref()
                    .and_then(|target| target.base_ref.clone()),
                task_url: args.task_url.clone(),
                cleanup: Some("preserve".to_string()),
                materialization: dispatch_workspace_materialization(
                    workspace_target.as_ref(),
                    &repo,
                ),
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
                "client_context": client_context,
                "workspace": workspace_target.as_ref().map(|target| target.metadata.clone()),
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
        "workspace": workspace_target.as_ref().map(|target| target.metadata.clone()),
        "workspace_root": workspace_root.map(|path| path.display().to_string()),
        "client_context": client_context,
        "task_url": args.task_url,
    });

    Ok(plan)
}

fn preflight_dispatch_provider_secrets(plan: &AgentTaskPlan) -> homeboy::core::Result<()> {
    let mut names = Vec::new();
    for task in &plan.tasks {
        for name in &task.executor.secret_env {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }

    validate_secret_env(&names).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "secret_env",
            error.message,
            None,
            Some(vec![
                "Configure provider credentials with Homeboy's agent-task secret config or run `homeboy agent-task providers --secret-env <ENV>` to inspect redacted readiness.".to_string(),
            ]),
        )
    })
}

fn dispatch_instructions(instructions: String, task_url: Option<&str>) -> String {
    if task_url.is_none() || instructions.contains("Iterator runtime-fix guardrails") {
        return instructions;
    }

    format!(
        "{instructions}\n\nIterator runtime-fix guardrails:\n- Before adding a new runtime predicate or transform, search for nearby existing predicates and transform families that already cover the finding shape.\n- Keep generated runtime changes bounded to the source finding evidence; preserve evidence-specific discriminators such as class, href, role, text, or DOM shape unless the task explicitly asks for broader behavior.\n- If existing behavior plus evidence/test coverage already resolves the finding, prefer evidence-only or test-only output over a broader runtime change.\n- In the PR evidence, report source relationship, change kind, verification capability, and why any runtime change is not broader than the packet evidence."
    )
}

fn resolve_dispatch_workspace(
    args: &DispatchArgs,
) -> homeboy::core::Result<Option<DispatchWorkspaceTarget>> {
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
        return Ok(Some(DispatchWorkspaceTarget::path(path, "cwd")));
    }

    let Some(workspace) = &args.workspace else {
        return Ok(None);
    };

    let path = std::path::PathBuf::from(workspace);
    if path.is_dir() {
        return Ok(Some(DispatchWorkspaceTarget::path(path, "workspace-path")));
    }

    let record = worktree::resolve(workspace).map_err(|_| {
        homeboy::core::Error::validation_invalid_argument(
            "workspace",
            format!(
                "agent-task dispatch workspace '{}' is neither an existing directory nor a Homeboy worktree record",
                workspace
            ),
            Some(workspace.clone()),
            Some(vec![
                "Pass --cwd <path> for an explicit checkout".to_string(),
                "Pass --workspace <path> for an existing workspace path".to_string(),
                "Create or list Homeboy task worktrees with `homeboy worktree create` and `homeboy worktree list`".to_string(),
            ]),
        )
    })?;
    let root = std::path::PathBuf::from(&record.worktree_path);
    if !root.is_dir() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workspace",
            format!(
                "Homeboy worktree '{}' points at a missing directory {}; recreate or remove the stale record",
                record.id,
                root.display()
            ),
            Some(workspace.clone()),
            None,
        ));
    }

    Ok(Some(DispatchWorkspaceTarget::record(record)))
}

#[derive(Debug, Clone)]
struct DispatchWorkspaceTarget {
    root: std::path::PathBuf,
    slug: Option<String>,
    kind: Option<String>,
    component_id: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    metadata: Value,
}

impl DispatchWorkspaceTarget {
    fn path(root: std::path::PathBuf, kind: &str) -> Self {
        let slug = root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        Self {
            root: root.clone(),
            slug,
            kind: Some(kind.to_string()),
            component_id: None,
            branch: None,
            base_ref: None,
            metadata: serde_json::json!({
                "kind": kind,
                "root": root.display().to_string(),
            }),
        }
    }

    fn record(record: worktree::TaskWorktreeRecord) -> Self {
        let root = std::path::PathBuf::from(&record.worktree_path);
        Self {
            root,
            slug: Some(record.component_id.clone()),
            kind: Some("homeboy-worktree".to_string()),
            component_id: Some(record.component_id.clone()),
            branch: Some(record.branch.clone()),
            base_ref: Some(record.base_ref.clone()),
            metadata: serde_json::json!({
                "kind": "homeboy-worktree",
                "id": record.id,
                "component_id": record.component_id,
                "branch": record.branch,
                "base_ref": record.base_ref,
                "root": record.worktree_path,
                "source_checkout": record.source_checkout,
                "task_url": record.task_url,
            }),
        }
    }
}

fn dispatch_workspace_materialization(
    workspace: Option<&DispatchWorkspaceTarget>,
    repo: &Option<String>,
) -> Value {
    let Some(workspace) = workspace else {
        return serde_json::json!({ "repo": repo });
    };

    serde_json::json!({
        "repo": repo,
        "workspace": workspace.metadata,
    })
}

fn dispatch_client_context(args: &DispatchArgs) -> homeboy::core::Result<Value> {
    let Some(spec) = &args.client_context else {
        return Ok(serde_json::json!({}));
    };

    let raw = read_text_spec(spec, "client-context")?;
    let context = serde_json::from_str::<Value>(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task dispatch client context".to_string()),
            Some(raw),
        )
    })?;

    if !context.is_object() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "client-context",
            "agent-task dispatch --client-context must resolve to a JSON object",
            None,
            None,
        ));
    }

    Ok(context)
}

fn dispatch_provider_config(
    args: &DispatchArgs,
    repo: &Option<String>,
    workspace: Option<&DispatchWorkspaceTarget>,
    client_context: &Value,
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
    map.entry("workspace".to_string())
        .or_insert_with(|| serde_json::json!(workspace.map(|target| target.metadata.clone())));
    map.entry("workspace_root".to_string()).or_insert_with(|| {
        serde_json::json!(workspace.map(|target| target.root.display().to_string()))
    });
    map.entry("client_context".to_string())
        .or_insert_with(|| client_context.clone());
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
    fn builds_repo_cooking_plan_from_prompt_file_and_client_context() {
        let workspace = tempfile::tempdir().expect("workspace");
        let prompt = tempfile::NamedTempFile::new().expect("prompt file");
        std::fs::write(prompt.path(), "Cook the issue cleanly.").expect("write prompt");

        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some(format!("@{}", prompt.path().display())),
            cwd: Some(workspace.path().display().to_string()),
            repo: Some("data-machine".to_string()),
            client_context: Some(
                r#"{"surface":"chat","conversation_id":"opaque-123"}"#.to_string(),
            ),
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
            plan.tasks[0].executor.config["client_context"]["surface"],
            "chat"
        );
        assert_eq!(
            plan.tasks[0].metadata["client_context"]["conversation_id"],
            "opaque-123"
        );
        let serialized = serde_json::to_string(&plan).expect("serialize plan");
        assert!(!serialized.contains("discord"));
        assert!(!serialized.contains("kimaki"));
        assert!(!serialized.contains("channel"));
        assert!(!serialized.contains("thread"));
    }

    #[test]
    fn rejects_non_object_client_context() {
        let error = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some("Cook with invalid context.".to_string()),
            client_context: Some("[]".to_string()),
            ..DispatchArgOverrides::default()
        }))
        .expect_err("client context should reject non-object JSON");

        assert!(error.to_string().contains("--client-context"));
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
    fn tracker_backed_dispatch_adds_runtime_fix_guardrails() {
        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some("Fix the finding.".to_string()),
            repo: Some("homeboy".to_string()),
            task_url: Some("https://github.com/Extra-Chill/homeboy/issues/3810".to_string()),
            ..DispatchArgOverrides::default()
        }))
        .expect("dispatch plan");

        assert!(plan.tasks[0]
            .instructions
            .contains("Iterator runtime-fix guardrails"));
        assert!(plan.tasks[0]
            .instructions
            .contains("bounded to the source finding evidence"));
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
    fn dispatch_preflights_missing_secret_env_before_submitting() {
        with_isolated_home(|_| {
            let missing = format!(
                "HOMEBOY_TEST_DISPATCH_MISSING_SECRET_{}",
                std::process::id()
            );
            std::env::remove_var(&missing);

            let err = dispatch_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Cook with missing provider auth.".to_string()),
                    secret_env: vec![missing.clone()],
                    run_id: Some("dispatch-missing-secret".to_string()),
                    ..DispatchArgOverrides::default()
                }),
                NoopExecutor,
            )
            .expect_err("missing secret should fail before submit");

            assert!(err.to_string().contains(&missing));
            assert!(agent_task_lifecycle::status("dispatch-missing-secret").is_err());
        });
    }

    #[test]
    fn resolves_workspace_path_without_specialized_coupling() {
        let worktree = tempfile::tempdir().expect("workspace");

        let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
            prompt: Some("Cook generic workspace.".to_string()),
            workspace: Some(worktree.path().display().to_string()),
            ..DispatchArgOverrides::default()
        }))
        .expect("dispatch workspace plan");

        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(worktree.path().to_str().expect("worktree utf8"))
        );
        assert_eq!(
            plan.tasks[0].workspace.kind.as_deref(),
            Some("workspace-path")
        );
        assert_eq!(
            plan.tasks[0].metadata["workspace"]["kind"],
            "workspace-path"
        );
    }

    #[test]
    fn resolves_homeboy_worktree_record_as_generic_workspace() {
        with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("workspace root");
            let source = root.path().join("homeboy");
            let worktree_path = root.path().join("homeboy@fix-runtime");
            std::fs::create_dir(&source).expect("source dir");
            std::fs::create_dir(&worktree_path).expect("worktree dir");
            let record = worktree::TaskWorktreeRecord {
                id: "homeboy@fix-runtime".to_string(),
                component_id: "homeboy".to_string(),
                source_checkout: source.display().to_string(),
                worktree_path: worktree_path.display().to_string(),
                branch: "fix/runtime".to_string(),
                base_ref: "origin/main".to_string(),
                task_url: Some("https://github.com/Extra-Chill/homeboy/issues/3653".to_string()),
                run_id: None,
                cleanup_policy: worktree::CleanupPolicy::PreserveOnFailure,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                state: worktree::TaskWorktreeState::Active,
            };
            let store = homeboy::core::paths::observation_db()
                .expect("observation db")
                .parent()
                .expect("data root")
                .join("task-worktrees");
            std::fs::create_dir_all(&store).expect("store dir");
            std::fs::write(
                store.join(format!(
                    "{}.json",
                    homeboy::core::paths::sanitize_path_segment("homeboy@fix-runtime")
                )),
                serde_json::to_string_pretty(&record).expect("record json"),
            )
            .expect("write record");

            let plan = build_dispatch_plan(&dispatch_args(DispatchArgOverrides {
                prompt: Some("Cook Homeboy workspace.".to_string()),
                workspace: Some("homeboy@fix-runtime".to_string()),
                ..DispatchArgOverrides::default()
            }))
            .expect("dispatch workspace plan");

            assert_eq!(plan.group_key.as_deref(), Some("homeboy"));
            assert_eq!(
                plan.tasks[0].workspace.root.as_deref(),
                Some(worktree_path.to_str().expect("worktree utf8"))
            );
            assert_eq!(
                plan.tasks[0].workspace.kind.as_deref(),
                Some("homeboy-worktree")
            );
            assert_eq!(
                plan.tasks[0].workspace.component_id.as_deref(),
                Some("homeboy")
            );
            assert_eq!(
                plan.tasks[0].workspace.branch.as_deref(),
                Some("fix/runtime")
            );
            assert_eq!(
                plan.tasks[0].executor.config["workspace"]["id"],
                "homeboy@fix-runtime"
            );
        });
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
                outputs: Value::Null,
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
        workspace: Option<String>,
        repo: Option<String>,
        task_url: Option<String>,
        secret_env: Vec<String>,
        client_context: Option<String>,
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
            workspace: overrides.workspace,
            repo: overrides.repo,
            task_url: overrides.task_url,
            backend: "fixture".to_string(),
            selector: None,
            model: None,
            secret_env: overrides.secret_env,
            provider_config: None,
            client_context: overrides.client_context,
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
