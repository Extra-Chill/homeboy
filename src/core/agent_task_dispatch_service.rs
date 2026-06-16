//! Core service for durable agent-task dispatch.
//!
//! The CLI adapter owns clap parsing and JSON rendering. This service owns the
//! typed dispatch request, plan construction, provider preflight, durable
//! lifecycle transitions, and scheduler orchestration.

use serde::Serialize;
use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_config_materialization::materialize_provider_config_refs;
use crate::core::agent_task_lifecycle as lifecycle;
use crate::core::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
use crate::core::agent_task_provider::provider_requires_cwd_git_checkout;
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskRetryPolicy,
    AgentTaskScheduler,
};
use crate::core::agent_task_secrets::validate_secret_env;
use crate::core::agent_task_service::{aggregate_exit_code, AgentTaskRunResult};
use crate::core::{config, defaults, worktree, Error, Result};

pub const DISPATCH_RESULT_SCHEMA: &str = "homeboy/agent-task-dispatch/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskDispatchRequest {
    pub prompt: Option<String>,
    pub tasks: Vec<String>,
    pub tasks_json: Option<String>,
    pub cwd: Option<String>,
    pub workspace: Option<String>,
    pub repo: Option<String>,
    pub task_url: Option<String>,
    pub backend: String,
    pub selector: Option<String>,
    pub model: Option<String>,
    pub required_capabilities: Vec<String>,
    pub secret_env: Vec<String>,
    pub provider_config: Option<String>,
    pub client_context: Option<String>,
    pub concurrency: usize,
    pub attempts: u32,
    pub run_id: Option<String>,
    pub queue_only: bool,
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
    let plan = build_dispatch_plan_with_provider_requirements(
        &request,
        provider_requires_cwd_git_checkout,
    )?;
    preflight_dispatch_provider_secrets(&plan)?;
    let submitted = lifecycle::submit_plan(&plan, request.run_id.as_deref())?;
    let run_id = submitted.run_id.clone();

    if request.queue_only {
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

pub fn build_dispatch_plan(request: &AgentTaskDispatchRequest) -> Result<AgentTaskPlan> {
    build_dispatch_plan_with_provider_requirements(request, provider_requires_cwd_git_checkout)
}

pub fn build_dispatch_plan_with_provider_requirements(
    request: &AgentTaskDispatchRequest,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> Result<AgentTaskPlan> {
    if request.prompt.is_none() && request.tasks.is_empty() && request.tasks_json.is_none() {
        return Err(Error::validation_invalid_argument(
            "prompt",
            "agent-task dispatch requires --prompt, --prompt @file, --prompt -, repeated --task inputs, or --tasks @tasks.json",
            None,
            Some(vec![
                "Example: homeboy agent-task dispatch --repo data-machine --cwd /path/to/worktree --prompt @task.txt".to_string(),
                "Wave input: homeboy agent-task dispatch --tasks @tasks.json --concurrency 8".to_string(),
            ]),
        ));
    }

    let workspace_target = resolve_dispatch_workspace(request)?;
    validate_dispatch_workspace_target(
        request,
        workspace_target.as_ref(),
        &provider_requires_cwd_git_checkout,
    )?;
    let workspace_root = workspace_target.as_ref().map(|target| target.root.clone());
    let repo = request
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
    if let Some(prompt) = &request.prompt {
        prompt_specs.push(prompt.clone());
    }
    prompt_specs.extend(request.tasks.clone());
    prompt_specs.extend(read_dispatch_tasks_json(request.tasks_json.as_deref())?);

    let client_context = dispatch_client_context(request)?;
    let provider_config =
        dispatch_provider_config(request, &repo, workspace_target.as_ref(), &client_context)?;
    let secret_env = dispatch_secret_env(request, &provider_config);
    let mut tasks = Vec::new();
    for (index, prompt_spec) in prompt_specs.iter().enumerate() {
        let instructions = dispatch_instructions(
            read_text_spec(prompt_spec, "prompt")?,
            request.task_url.as_deref(),
        );
        let task_id = dispatch_task_id(repo.as_deref(), index);
        let mut source_refs = Vec::new();
        if let Some(task_url) = &request.task_url {
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
                backend: request.backend.clone(),
                selector: request.selector.clone(),
                required_capabilities: request.required_capabilities.clone(),
                secret_env: secret_env.clone(),
                model: request.model.clone(),
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
                task_url: request.task_url.clone(),
                cleanup: Some("preserve".to_string()),
                materialization: dispatch_workspace_materialization(
                    workspace_target.as_ref(),
                    &repo,
                ),
            },
            component_contracts: Vec::new(),
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
                "task_url": request.task_url,
                "prompt_source": prompt_spec,
                "dispatch": "agent-task dispatch",
                "required_capabilities": request.required_capabilities,
            }),
        });
    }

    let mut plan = AgentTaskPlan::new(
        format!("agent-task-dispatch-{}", uuid::Uuid::new_v4()),
        tasks,
    );
    plan.group_key = repo.clone();
    plan.options.max_concurrency = request.concurrency.max(1);
    plan.options.retry = AgentTaskRetryPolicy {
        max_attempts: request.attempts.max(1),
        ..AgentTaskRetryPolicy::default()
    };
    plan.metadata = serde_json::json!({
        "kind": "repo-cooking-dispatch",
        "repo": repo,
        "workspace": workspace_target.as_ref().map(|target| target.metadata.clone()),
        "workspace_root": workspace_root.map(|path| path.display().to_string()),
        "client_context": client_context,
        "task_url": request.task_url,
    });

    Ok(plan)
}

fn validate_dispatch_workspace_target(
    request: &AgentTaskDispatchRequest,
    workspace: Option<&DispatchWorkspaceTarget>,
    provider_requires_cwd_git_checkout: &impl Fn(&str, Option<&str>) -> bool,
) -> Result<()> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    if workspace.kind.as_deref() != Some("cwd")
        || !provider_requires_cwd_git_checkout(&request.backend, request.selector.as_deref())
    {
        return Ok(());
    }
    if is_git_checkout(&workspace.root) {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "cwd",
        "selected agent-task provider requires --cwd to be a git checkout so generated files can be returned as a patch artifact",
        Some(workspace.root.display().to_string()),
        Some(vec![
            "Use a Homeboy/Data Machine Code worktree for write-capable agent tasks.".to_string(),
            "Initialize the target as a git checkout before dispatching.".to_string(),
            "Use a provider without a git-checkout materialization requirement only if it has an explicit non-git apply-back artifact contract.".to_string(),
        ]),
    ))
}

fn is_git_checkout(path: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn preflight_dispatch_provider_secrets(plan: &AgentTaskPlan) -> Result<()> {
    let mut names = Vec::new();
    for task in &plan.tasks {
        for name in &task.executor.secret_env {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }

    validate_secret_env(&names).map_err(|error| {
        Error::validation_invalid_argument(
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
    if task_url.is_none() || instructions.contains("Generated change guardrails") {
        return instructions;
    }

    format!(
        "{instructions}\n\nGenerated change guardrails:\n- First look for nearby existing predicates, contracts, or implementation families that already cover the requested behavior.\n- Keep generated changes bounded to the source evidence and task scope; preserve evidence-specific discriminators unless the task explicitly asks for broader behavior.\n- If dependency freshness, lifecycle, or validation gates fail, update and verify the relevant dependency checkout or declared component reference; do not bypass, disable, or skip the gate to reach a PR.\n- If existing behavior plus evidence/test coverage already resolves the task, prefer evidence-only or test-only output over a broader runtime change.\n- In PR evidence, report source relationship, change kind, verification capability, and why any runtime change is not broader than the source evidence."
    )
}

fn resolve_dispatch_workspace(
    request: &AgentTaskDispatchRequest,
) -> Result<Option<DispatchWorkspaceTarget>> {
    if let Some(cwd) = &request.cwd {
        let path = std::path::PathBuf::from(cwd);
        if !path.is_dir() {
            return Err(Error::validation_invalid_argument(
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

    let Some(workspace) = &request.workspace else {
        return Ok(None);
    };

    let path = std::path::PathBuf::from(workspace);
    if path.is_dir() {
        return Ok(Some(DispatchWorkspaceTarget::path(path, "workspace-path")));
    }

    let record = worktree::resolve(workspace).map_err(|_| {
        Error::validation_invalid_argument(
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
        return Err(Error::validation_invalid_argument(
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

fn dispatch_client_context(request: &AgentTaskDispatchRequest) -> Result<Value> {
    let Some(spec) = &request.client_context else {
        return Ok(serde_json::json!({}));
    };

    let raw = read_text_spec(spec, "client-context")?;
    let context = serde_json::from_str::<Value>(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task dispatch client context".to_string()),
            Some(raw),
        )
    })?;

    if !context.is_object() {
        return Err(Error::validation_invalid_argument(
            "client-context",
            "agent-task dispatch --client-context must resolve to a JSON object",
            None,
            None,
        ));
    }

    Ok(context)
}

fn dispatch_provider_config(
    request: &AgentTaskDispatchRequest,
    repo: &Option<String>,
    workspace: Option<&DispatchWorkspaceTarget>,
    client_context: &Value,
) -> Result<Value> {
    let mut config = Value::Object(defaults::load_config().settings.into_iter().collect());
    if let Some(spec) = &request.provider_config {
        let raw = read_text_spec(spec, "provider-config")?;
        let explicit = serde_json::from_str::<Value>(&raw).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task dispatch provider config".to_string()),
                Some(raw),
            )
        })?;

        if !explicit.is_object() {
            return Err(Error::validation_invalid_argument(
                "provider-config",
                "agent-task dispatch --provider-config must resolve to a JSON object",
                None,
                None,
            ));
        }

        let map = config.as_object_mut().expect("global settings object");
        map.extend(
            explicit
                .as_object()
                .expect("explicit provider config object")
                .clone(),
        );
    }

    if !config.is_object() {
        return Err(Error::validation_invalid_argument(
            "provider-config",
            "agent-task dispatch --provider-config must resolve to a JSON object",
            None,
            None,
        ));
    }

    let mut config = materialize_provider_config_refs(config)?;
    if !config.is_object() {
        return Err(Error::validation_invalid_argument(
            "provider-config",
            "agent-task dispatch --provider-config root must remain a JSON object after materializing configured refs",
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
        .or_insert_with(|| serde_json::json!(request.task_url));

    Ok(config)
}

fn dispatch_secret_env(request: &AgentTaskDispatchRequest, provider_config: &Value) -> Vec<String> {
    let mut names = request.secret_env.clone();
    names.extend(provider_config_secret_env(provider_config));
    names.sort();
    names.dedup();
    names
}

fn provider_config_secret_env(provider_config: &Value) -> Vec<String> {
    let Some(config) = provider_config.as_object() else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(Value::Array(items)) => {
                names.extend(
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string)),
                );
            }
            Some(Value::String(name)) => names.push(name.clone()),
            _ => {}
        }
    }
    names.sort();
    names.dedup();
    names
}

fn read_text_spec(spec: &str, label: &str) -> Result<String> {
    config::read_json_spec_to_string(spec).map_err(|error| {
        Error::internal_unexpected(format!(
            "failed to read agent-task dispatch {label} input: {error}"
        ))
    })
}

fn read_dispatch_tasks_json(spec: Option<&str>) -> Result<Vec<String>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };

    let raw = read_text_spec(spec, "tasks")?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
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

fn task_prompts_from_json_items(items: Vec<Value>) -> Result<Vec<String>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| task_prompt_from_json_item(item, index))
        .collect()
}

fn invalid_tasks_json_error() -> Error {
    Error::validation_invalid_argument(
        "tasks",
        "agent-task dispatch --tasks expects a JSON array or object with a tasks array",
        None,
        None,
    )
}

fn task_prompt_from_json_item(item: Value, index: usize) -> Result<String> {
    match item {
        Value::String(prompt) => Ok(prompt),
        Value::Object(mut object) => object
            .remove("prompt")
            .or_else(|| object.remove("instructions"))
            .and_then(|value| value.as_str().map(str::to_string))
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "tasks",
                    format!(
                        "agent-task dispatch task item {} must include a string prompt or instructions field",
                        index + 1
                    ),
                    None,
                    None,
                )
            }),
        _ => Err(Error::validation_invalid_argument(
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
    use crate::core::agent_task::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
        AGENT_TASK_OUTCOME_SCHEMA,
    };
    use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;
    use std::process::Command;

    #[test]
    fn builds_repo_cooking_plan_from_prompt_file_and_client_context() {
        let workspace = tempfile::tempdir().expect("workspace");
        let prompt = tempfile::NamedTempFile::new().expect("prompt file");
        std::fs::write(prompt.path(), "Cook the issue cleanly.").expect("write prompt");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some(format!("@{}", prompt.path().display())),
            cwd: Some(workspace.path().display().to_string()),
            repo: Some("data-machine".to_string()),
            client_context: Some(
                r#"{"surface":"chat","conversation_id":"opaque-123"}"#.to_string(),
            ),
            ..DispatchRequestOverrides::default()
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
    fn accepts_tasks_json_wave_input() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(
            tasks.path(),
            r#"{"tasks":["Cook issue A",{"prompt":"Cook issue B"}]}"#,
        )
        .expect("write tasks");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            tasks_json: Some(format!("@{}", tasks.path().display())),
            repo: Some("homeboy".to_string()),
            concurrency: 8,
            attempts: 3,
            ..DispatchRequestOverrides::default()
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
    fn dispatch_plan_preserves_abstract_executor_requirements() {
        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Run the declared workflow.".to_string()),
            required_capabilities: vec!["tool:repo-inspector".to_string()],
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(
            plan.tasks[0].executor.required_capabilities,
            vec!["tool:repo-inspector".to_string()]
        );
        assert_eq!(
            plan.tasks[0].metadata["required_capabilities"],
            serde_json::json!(["tool:repo-inspector"])
        );
    }

    #[test]
    fn queue_only_returns_durable_run_without_executing() {
        with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");

            let result = dispatch(
                dispatch_request(DispatchRequestOverrides {
                    prompt: Some("Queue this minion.".to_string()),
                    cwd: Some(workspace.path().display().to_string()),
                    queue_only: true,
                    run_id: Some("dispatch-queued".to_string()),
                    ..DispatchRequestOverrides::default()
                }),
                NoopExecutor,
            )
            .expect("dispatch queued");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.schema, DISPATCH_RESULT_SCHEMA);
            assert_eq!(result.value.run_id, "dispatch-queued");
            assert!(result.value.queued);
            assert_eq!(result.value.state, AgentTaskRunState::Queued);
            assert!(result.value.plan_path.ends_with("plan.json"));
            assert!(result.value.aggregate.is_none());
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

            let err = dispatch(
                dispatch_request(DispatchRequestOverrides {
                    prompt: Some("Cook with missing provider auth.".to_string()),
                    secret_env: vec![missing.clone()],
                    run_id: Some("dispatch-missing-secret".to_string()),
                    ..DispatchRequestOverrides::default()
                }),
                NoopExecutor,
            )
            .expect_err("missing secret should fail before submit");

            assert!(err.to_string().contains(&missing));
            assert!(lifecycle::status("dispatch-missing-secret").is_err());
        });
    }

    #[test]
    fn git_checkout_provider_rejects_non_git_cwd() {
        let workspace = tempfile::tempdir().expect("workspace");

        let error = build_dispatch_plan_with_provider_requirements(
            &dispatch_request(DispatchRequestOverrides {
                prompt: Some("Generate files here.".to_string()),
                cwd: Some(workspace.path().display().to_string()),
                backend: Some("git-required".to_string()),
                ..DispatchRequestOverrides::default()
            }),
            |backend, _selector| backend == "git-required",
        )
        .expect_err("non-git cwd should be rejected");

        assert!(error.to_string().contains("git checkout"));
        assert!(error.to_string().contains("patch artifact"));
    }

    #[test]
    fn git_checkout_provider_accepts_git_cwd() {
        let workspace = tempfile::tempdir().expect("workspace");
        git(workspace.path(), &["init"]);

        let plan = build_dispatch_plan_with_provider_requirements(
            &dispatch_request(DispatchRequestOverrides {
                prompt: Some("Generate files here.".to_string()),
                cwd: Some(workspace.path().display().to_string()),
                backend: Some("git-required".to_string()),
                ..DispatchRequestOverrides::default()
            }),
            |backend, _selector| backend == "git-required",
        )
        .expect("git cwd should be accepted");

        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.path().to_str().expect("workspace utf8"))
        );
    }

    #[test]
    fn dispatch_promotes_provider_config_secret_env() {
        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook with provider-config auth.".to_string()),
            provider_config: Some(
                serde_json::json!({
                    "provider": "example",
                    "secret_env": ["HOMEBOY_PROVIDER_ACCESS_TOKEN"],
                    "secretEnv": "HOMEBOY_PROVIDER_REFRESH_TOKEN"
                })
                .to_string(),
            ),
            secret_env: vec!["OPENAI_API_KEY".to_string()],
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(
            plan.tasks[0].executor.secret_env,
            vec![
                "HOMEBOY_PROVIDER_ACCESS_TOKEN".to_string(),
                "HOMEBOY_PROVIDER_REFRESH_TOKEN".to_string(),
                "OPENAI_API_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn dispatch_merges_global_settings_into_executor_config() {
        with_isolated_home(|_| {
            let runtime = tempfile::tempdir().expect("runtime repo");
            init_git_repo(runtime.path());

            defaults::save_config(&defaults::HomeboyConfig {
                settings: HashMap::from([
                    ("provider".to_string(), serde_json::json!("example")),
                    (
                        "provider_plugin_paths".to_string(),
                        serde_json::json!(["/providers/openai"]),
                    ),
                    (
                        "runtime_overlays".to_string(),
                        serde_json::json!([{ "repo": runtime.path().display().to_string(), "ref": "HEAD" }]),
                    ),
                ]),
                ..defaults::HomeboyConfig::default()
            })
            .expect("save config");

            let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
                prompt: Some("Cook with configured provider defaults.".to_string()),
                provider_config: Some(serde_json::json!({ "provider": "override" }).to_string()),
                ..DispatchRequestOverrides::default()
            }))
            .expect("dispatch plan");

            assert_eq!(
                plan.tasks[0].executor.config["provider_plugin_paths"],
                serde_json::json!(["/providers/openai"])
            );
            assert!(plan.tasks[0].executor.config["runtime_overlays"][0]
                .as_str()
                .is_some_and(|path| std::path::Path::new(path).exists()));
            assert_eq!(plan.tasks[0].executor.config["provider"], "override");
        });
    }

    fn init_git_repo(path: &std::path::Path) {
        std::fs::write(path.join("README.md"), "runtime\n").expect("write runtime file");
        run_git(path, &["init", "-b", "main"]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Homeboy Test"]);
        run_git(path, &["add", "README.md"]);
        run_git(path, &["commit", "-m", "init"]);
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git command runs");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn resolves_workspace_path_without_specialized_coupling() {
        let worktree = tempfile::tempdir().expect("workspace");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook generic workspace.".to_string()),
            workspace: Some(worktree.path().display().to_string()),
            ..DispatchRequestOverrides::default()
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

    #[allow(dead_code)]
    struct PatchExecutor;

    impl AgentTaskExecutorAdapter for PatchExecutor {
        fn execute(
            &self,
            _request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-task".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("patch ready".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-1".to_string(),
                    kind: "patch".to_string(),
                    name: Some("changes.patch".to_string()),
                    path: Some("/tmp/changes.patch".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                }],
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
    struct DispatchRequestOverrides {
        prompt: Option<String>,
        tasks_json: Option<String>,
        cwd: Option<String>,
        workspace: Option<String>,
        repo: Option<String>,
        task_url: Option<String>,
        secret_env: Vec<String>,
        provider_config: Option<String>,
        client_context: Option<String>,
        required_capabilities: Vec<String>,
        concurrency: usize,
        attempts: u32,
        queue_only: bool,
        run_id: Option<String>,
        backend: Option<String>,
    }

    fn dispatch_request(overrides: DispatchRequestOverrides) -> AgentTaskDispatchRequest {
        AgentTaskDispatchRequest {
            prompt: overrides.prompt,
            tasks: Vec::new(),
            tasks_json: overrides.tasks_json,
            cwd: overrides.cwd,
            workspace: overrides.workspace,
            repo: overrides.repo,
            task_url: overrides.task_url,
            backend: overrides.backend.unwrap_or_else(|| "fixture".to_string()),
            selector: None,
            model: None,
            required_capabilities: overrides.required_capabilities,
            secret_env: overrides.secret_env,
            provider_config: overrides.provider_config,
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

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
