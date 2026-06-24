//! Dispatch plan construction for durable agent-task dispatch.
//!
//! Owns request validation, workspace resolution, provider-config and
//! secret-env assembly, tasks-JSON parsing, and the `AgentTaskPlan` build. The
//! dispatch service consumes [`build_dispatch_plan`] /
//! [`build_dispatch_plan_with_provider_requirements`] and keeps lifecycle and
//! cook-handoff orchestration.

use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy,
    AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspace, AgentTaskWorkspaceMode,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_config_materialization::materialize_provider_config_refs;
use crate::core::agent_task_prompts;
use crate::core::agent_task_provider::provider_requires_cwd_git_checkout;
use crate::core::agent_task_runtime_dependency_graph;
use crate::core::agent_task_scheduler::{AgentTaskPlan, AgentTaskRetryPolicy};
use crate::core::agent_task_secrets::validate_secret_env;
use crate::core::{config, defaults, worktree, Error, Result};

use super::agent_task_dispatch_service::AgentTaskDispatchRequest;

pub fn build_dispatch_plan(request: &AgentTaskDispatchRequest) -> Result<AgentTaskPlan> {
    build_dispatch_plan_with_provider_requirements(request, provider_requires_cwd_git_checkout)
}

pub fn build_dispatch_plan_with_provider_requirements(
    request: &AgentTaskDispatchRequest,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> Result<AgentTaskPlan> {
    if request.prompt.is_none() && request.tasks.is_empty() && request.core.tasks_json.is_none() {
        return Err(Error::validation_invalid_argument(
            "prompt",
            "agent-task cook requires --prompt, --prompt @file, --prompt -, repeated --task inputs, or --tasks @tasks.json",
            None,
            Some(vec![
                "Example: homeboy agent-task cook --repo sample-plugin --cwd /path/to/worktree --to-worktree sample-plugin@fix-issue --verify 'npm test' --prompt @task.txt".to_string(),
                "Wave input: homeboy agent-task cook --tasks @tasks.json --concurrency 8 --to-worktree sample-plugin@fix-issue --verify 'npm test'".to_string(),
            ]),
        ));
    }

    let workspace_target = resolve_dispatch_workspace(request)?;
    validate_dispatch_workspace_target(
        &request.backend,
        request.selector.as_deref(),
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
        prompt_specs.push(DispatchPromptSpec::new(prompt.clone()));
    }
    prompt_specs.extend(request.tasks.iter().cloned().map(DispatchPromptSpec::new));
    prompt_specs.extend(read_dispatch_tasks_json(
        request.core.tasks_json.as_deref(),
    )?);

    let client_context = dispatch_client_context(request)?;
    let mut provider_config =
        dispatch_provider_config(request, &repo, workspace_target.as_ref(), &client_context)?;
    let component_contracts = dispatch_component_contracts(&provider_config, &client_context)?;
    // Resolve + materialize the declared runtime dependency graph at known refs
    // and preflight-fail before dispatch when a required runtime dependency is
    // unresolved or stale. The resolved refs/paths are recorded in plan/task
    // metadata below so run evidence shows exactly which component refs a Lab
    // proof ran against (#6121).
    let runtime_dependency_graph =
        agent_task_runtime_dependency_graph::resolve_runtime_dependency_graph(
            &component_contracts,
        )?;
    promote_component_contracts_to_provider_config(&mut provider_config, &component_contracts);
    let secret_env = dispatch_secret_env(request, &provider_config);
    let runtime_dependency_graph_evidence = (!runtime_dependency_graph.is_empty())
        .then(|| runtime_dependency_graph.to_evidence_value());
    let mut tasks = Vec::new();
    for (index, prompt_spec) in prompt_specs.iter().enumerate() {
        let instructions = dispatch_instructions(
            read_text_spec(&prompt_spec.prompt, "prompt")?,
            request.task_url.as_deref(),
        );
        let task_id = prompt_spec
            .task_id
            .clone()
            .unwrap_or_else(|| dispatch_task_id(repo.as_deref(), index));
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
                runtime_selection: None,
                required_capabilities: request.required_capabilities.clone(),
                secret_env: secret_env.clone(),
                model: request.model.clone(),
                config: provider_config.clone(),
            },
            instructions,
            inputs: dispatch_request_inputs(&client_context),
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
            component_contracts: component_contracts.clone(),
            policy: AgentTaskPolicy {
                read: "workspace".to_string(),
                write: "patch".to_string(),
                apply: "manual".to_string(),
                tools: Default::default(),
            },
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec![
                "patch".to_string(),
                "agent_result".to_string(),
                "transcript".to_string(),
            ],
            artifact_declarations: Vec::new(),
            metadata: serde_json::json!({
                "repo": repo,
                "client_context": client_context,
                "workspace": workspace_target.as_ref().map(|target| target.metadata.clone()),
                "task_url": request.task_url,
                "prompt_source": &prompt_spec.prompt,
                "dispatch": "agent-task cook",
                "required_capabilities": request.required_capabilities,
                "runtime_dependency_graph": runtime_dependency_graph_evidence,
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
        max_attempts: request.core.attempts.max(1),
        ..AgentTaskRetryPolicy::default()
    };
    plan.metadata = serde_json::json!({
        "kind": "agent-task-dispatch",
        "repo": repo,
        "workspace": workspace_target.as_ref().map(|target| target.metadata.clone()),
        "workspace_root": workspace_root.map(|path| path.display().to_string()),
        "client_context": client_context,
        "task_url": request.task_url,
        "runtime_dependency_graph": runtime_dependency_graph_evidence,
    });

    Ok(plan)
}

fn validate_dispatch_workspace_target(
    backend: &str,
    selector: Option<&str>,
    workspace: Option<&DispatchWorkspaceTarget>,
    provider_requires_cwd_git_checkout: &impl Fn(&str, Option<&str>) -> bool,
) -> Result<()> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    if !provider_requires_cwd_git_checkout(backend, selector) {
        return Ok(());
    }
    if is_git_checkout(&workspace.root) {
        return Ok(());
    }
    let argument = if workspace.kind.as_deref() == Some("cwd") {
        "cwd"
    } else {
        "workspace"
    };

    Err(Error::validation_invalid_argument(
        argument,
        "selected agent-task provider requires the dispatch workspace to be a git checkout so generated files can be returned as a patch artifact",
        Some(workspace.root.display().to_string()),
        Some(vec![
            "Use a Homeboy worktree or another git checkout for write-capable agent tasks.".to_string(),
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

pub fn preflight_dispatch_provider_secrets(plan: &AgentTaskPlan) -> Result<()> {
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
                    "agent-task cook workspace '{}' is not a directory",
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
                "agent-task cook workspace '{}' is neither an existing directory nor a Homeboy worktree record",
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
    let Some(spec) = &request.core.client_context else {
        return Ok(serde_json::json!({}));
    };

    let raw = read_text_spec(spec, "client-context")?;
    let context = serde_json::from_str::<Value>(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task cook client context".to_string()),
            Some(raw),
        )
    })?;

    if !context.is_object() {
        return Err(Error::validation_invalid_argument(
            "client-context",
            "agent-task cook --client-context must resolve to a JSON object",
            None,
            None,
        ));
    }

    Ok(context)
}

/// Error returned when a resolved provider config is not a JSON object. Shared
/// by the explicit-spec and post-default validation paths so both surface an
/// identical message (#5091).
fn provider_config_must_be_object_error() -> Error {
    Error::validation_invalid_argument(
        "provider-config",
        "agent-task cook --provider-config must resolve to a JSON object",
        None,
        None,
    )
}

fn dispatch_provider_config(
    request: &AgentTaskDispatchRequest,
    repo: &Option<String>,
    workspace: Option<&DispatchWorkspaceTarget>,
    client_context: &Value,
) -> Result<Value> {
    let mut config = Value::Object(defaults::load_config().settings.into_iter().collect());
    if let Some(spec) = &request.core.provider_config {
        let raw = read_text_spec(spec, "provider-config")?;
        let explicit = serde_json::from_str::<Value>(&raw).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("agent-task cook provider config".to_string()),
                Some(raw),
            )
        })?;

        if !explicit.is_object() {
            return Err(provider_config_must_be_object_error());
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
        return Err(provider_config_must_be_object_error());
    }

    let mut config = materialize_provider_config_refs(config)?;
    if !config.is_object() {
        return Err(Error::validation_invalid_argument(
            "provider-config",
            "agent-task cook --provider-config root must remain a JSON object after materializing configured refs",
            None,
            None,
        ));
    }
    let map = config.as_object_mut().expect("provider config object");
    map.entry("repo".to_string())
        .or_insert_with(|| serde_json::json!(repo));
    map.entry("workspace".to_string())
        .or_insert_with(|| serde_json::json!(workspace.map(|target| target.metadata.clone())));
    map.entry("workspace_required".to_string())
        .or_insert_with(|| serde_json::json!(workspace.is_some()));
    map.entry("workspace_root".to_string()).or_insert_with(|| {
        serde_json::json!(workspace.map(|target| target.root.display().to_string()))
    });
    map.entry("client_context".to_string())
        .or_insert_with(|| client_context.clone());
    if let Some(artifact_dependencies) = client_context.get("artifact_dependencies") {
        map.entry("artifact_dependencies".to_string())
            .or_insert_with(|| artifact_dependencies.clone());
    }
    map.entry("task_url".to_string())
        .or_insert_with(|| serde_json::json!(request.task_url));

    Ok(config)
}

fn dispatch_component_contracts(
    provider_config: &Value,
    client_context: &Value,
) -> Result<Vec<AgentTaskComponentContract>> {
    let mut contracts = Vec::new();
    collect_component_contracts_from_value(provider_config, "provider-config", &mut contracts)?;
    collect_component_contracts_from_value(client_context, "client-context", &mut contracts)?;
    if let Some(inputs) = client_context.get("inputs") {
        collect_component_contracts_from_value(inputs, "client-context.inputs", &mut contracts)?;
    }
    Ok(contracts)
}

fn dispatch_request_inputs(client_context: &Value) -> Value {
    client_context.get("inputs").cloned().unwrap_or(Value::Null)
}

fn collect_component_contracts_from_value(
    value: &Value,
    label: &str,
    contracts: &mut Vec<AgentTaskComponentContract>,
) -> Result<()> {
    for key in ["component_contracts", "runtime_component_contracts"] {
        let Some(raw) = value.get(key) else {
            continue;
        };
        let mut parsed: Vec<AgentTaskComponentContract> = serde_json::from_value(raw.clone())
            .map_err(|error| {
                Error::validation_invalid_argument(
                    format!("{label}.{key}"),
                    format!("agent-task cook {label}.{key} must be an array of component contracts: {error}"),
                    Some(raw.to_string()),
                    None,
                )
            })?;
        contracts.append(&mut parsed);
    }
    Ok(())
}

fn promote_component_contracts_to_provider_config(
    provider_config: &mut Value,
    component_contracts: &[AgentTaskComponentContract],
) {
    if component_contracts.is_empty() {
        return;
    }
    let Some(map) = provider_config.as_object_mut() else {
        return;
    };
    map.entry("component_contracts".to_string())
        .or_insert_with(|| serde_json::to_value(component_contracts).unwrap_or(Value::Null));
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
    if let Some(prompt) = agent_task_prompts::resolve_stored_prompt_ref(spec)? {
        return Ok(prompt);
    }

    config::read_json_spec_to_string(spec).map_err(|error| {
        Error::internal_unexpected(format!(
            "failed to read agent-task cook {label} input: {error}"
        ))
    })
}

struct DispatchPromptSpec {
    prompt: String,
    task_id: Option<String>,
}

impl DispatchPromptSpec {
    fn new(prompt: String) -> Self {
        Self {
            prompt,
            task_id: None,
        }
    }
}

fn read_dispatch_tasks_json(spec: Option<&str>) -> Result<Vec<DispatchPromptSpec>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };

    let raw = read_text_spec(spec, "tasks")?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task cook tasks".to_string()),
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

fn task_prompts_from_json_items(items: Vec<Value>) -> Result<Vec<DispatchPromptSpec>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| task_prompt_from_json_item(item, index))
        .collect()
}

fn invalid_tasks_json_error() -> Error {
    Error::validation_invalid_argument(
        "tasks",
        "agent-task cook --tasks expects a JSON array or object with a tasks array",
        None,
        None,
    )
}

fn task_prompt_from_json_item(item: Value, index: usize) -> Result<DispatchPromptSpec> {
    match item {
        Value::String(prompt) => {
            validate_task_prompt(&prompt, index)?;
            Ok(DispatchPromptSpec::new(prompt))
        }
        Value::Object(mut object) => {
            validate_task_object_keys(&object, index)?;
            let prompt = object
                .remove("prompt")
                .or_else(|| object.remove("instructions"))
                .and_then(|value| value.as_str().map(str::to_string))
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "tasks",
                        format!(
                            "agent-task cook task item {} must include a string prompt or instructions field",
                            index + 1
                        ),
                        None,
                        None,
                    )
                })?;
            validate_task_prompt(&prompt, index)?;
            let task_id = object
                .remove("task_id")
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "tasks",
                            format!(
                                "agent-task cook task item {} task_id field must be a string",
                                index + 1
                            ),
                            None,
                            None,
                        )
                    })
                })
                .transpose()?;
            Ok(DispatchPromptSpec { prompt, task_id })
        }
        _ => Err(Error::validation_invalid_argument(
            "tasks",
            format!(
                "agent-task cook task item {} must be a string or object",
                index + 1
            ),
            None,
            None,
        )),
    }
}

fn validate_task_prompt(prompt: &str, index: usize) -> Result<()> {
    if !prompt.trim().is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "tasks",
        format!(
            "agent-task cook task item {} must include a non-empty prompt or instructions field",
            index + 1
        ),
        None,
        None,
    ))
}

fn validate_task_object_keys(object: &serde_json::Map<String, Value>, index: usize) -> Result<()> {
    for key in object.keys() {
        if key != "prompt" && key != "instructions" && key != "task_id" {
            return Err(Error::validation_invalid_argument(
                "tasks",
                format!(
                    "agent-task cook task item {} includes unsupported field {key:?}; supported fields are prompt, instructions, and task_id",
                    index + 1
                ),
                None,
                None,
            ));
        }
    }
    Ok(())
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
        AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
    };
    use crate::core::agent_task_dispatch_service::{
        dispatch, DispatchCoreInputs, DISPATCH_RESULT_SCHEMA,
    };
    use crate::core::agent_task_lifecycle::{self as lifecycle, AgentTaskRunState};
    use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskExecutorAdapter};
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;
    use std::process::Command;

    #[test]
    fn builds_dispatch_plan_from_prompt_file_and_client_context() {
        let workspace = tempfile::tempdir().expect("workspace");
        let prompt = tempfile::NamedTempFile::new().expect("prompt file");
        std::fs::write(prompt.path(), "Cook the issue cleanly.").expect("write prompt");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some(format!("@{}", prompt.path().display())),
            cwd: Some(workspace.path().display().to_string()),
            repo: Some("sample-plugin".to_string()),
            core: DispatchCoreInputs {
                client_context: Some(
                    r#"{"surface":"chat","conversation_id":"opaque-123"}"#.to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.group_key.as_deref(), Some("sample-plugin"));
        assert_eq!(plan.tasks[0].task_id, "cook-sample-plugin");
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
    fn builds_dispatch_plan_from_stored_prompt_ref() {
        with_isolated_home(|_| {
            crate::core::agent_task_prompts::save_prompt(
                "review-fix",
                "# Review fix\nCook from stored markdown.\n",
            )
            .expect("save prompt");

            let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
                prompt: Some("prompt:review-fix".to_string()),
                repo: Some("homeboy".to_string()),
                ..DispatchRequestOverrides::default()
            }))
            .expect("dispatch plan");

            assert_eq!(plan.tasks.len(), 1);
            assert_eq!(
                plan.tasks[0].instructions,
                "# Review fix\nCook from stored markdown.\n"
            );
        });
    }

    #[test]
    fn dispatch_plan_rejects_invalid_explicit_cwd_inside_git_repo() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init"]);
        let previous_cwd = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(repo.path()).expect("enter repo");
        let missing = repo.path().join("missing-workspace");

        let error = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook inside the materialized workspace.".to_string()),
            cwd: Some(missing.display().to_string()),
            ..DispatchRequestOverrides::default()
        }));

        std::env::set_current_dir(previous_cwd).expect("restore cwd");
        let error = error.expect_err("invalid explicit cwd should fail");
        assert!(error.to_string().contains("cwd"));
        assert!(error.to_string().contains("is not a directory"));
    }

    #[test]
    fn dispatch_plan_resolves_valid_explicit_cwd() {
        let workspace = tempfile::tempdir().expect("workspace");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook inside the explicit workspace.".to_string()),
            cwd: Some(workspace.path().display().to_string()),
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.path().to_str().expect("workspace utf8"))
        );
        assert_eq!(plan.tasks[0].metadata["workspace"]["kind"], "cwd");
    }

    #[test]
    fn accepts_tasks_json_wave_input() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(
            tasks.path(),
            r#"{"tasks":["Cook issue A",{"prompt":"Cook issue B"},{"instructions":"Cook issue C","task_id":"custom-cook-c"}]}"#,
        )
        .expect("write tasks");

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            repo: Some("homeboy".to_string()),
            concurrency: 8,
            core: DispatchCoreInputs {
                tasks_json: Some(format!("@{}", tasks.path().display())),
                attempts: 3,
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch wave plan");

        assert_eq!(plan.tasks.len(), 3);
        assert_eq!(plan.options.max_concurrency, 8);
        assert_eq!(plan.options.retry.max_attempts, 3);
        assert_eq!(plan.tasks[0].task_id, "cook-homeboy");
        assert_eq!(plan.tasks[1].task_id, "cook-homeboy-2");
        assert_eq!(plan.tasks[2].task_id, "custom-cook-c");
        assert_eq!(plan.tasks[0].instructions, "Cook issue A");
        assert_eq!(plan.tasks[1].instructions, "Cook issue B");
        assert_eq!(plan.tasks[2].instructions, "Cook issue C");
    }

    #[test]
    fn rejects_tasks_json_items_with_unknown_object_keys() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(
            tasks.path(),
            r#"{"tasks":[{"prompt":"Cook issue A","priority":"high"}]}"#,
        )
        .expect("write tasks");

        let error = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            core: DispatchCoreInputs {
                tasks_json: Some(format!("@{}", tasks.path().display())),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect_err("unknown task object field should fail");

        assert!(error.to_string().contains("unsupported field"));
        assert!(error.to_string().contains("priority"));
    }

    #[test]
    fn rejects_tasks_json_empty_string_prompt_after_trimming() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(tasks.path(), r#"{"tasks":["  \n\t  "]}"#).expect("write tasks");

        let error = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            core: DispatchCoreInputs {
                tasks_json: Some(format!("@{}", tasks.path().display())),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect_err("empty task string should fail");

        assert_eq!(error.details["field"], "tasks");
        assert!(error
            .to_string()
            .contains("non-empty prompt or instructions"));
    }

    #[test]
    fn rejects_tasks_json_object_without_usable_prompt() {
        let tasks = tempfile::NamedTempFile::new().expect("tasks file");
        std::fs::write(
            tasks.path(),
            r#"{"tasks":[{"task_id":"missing-prompt"},{"prompt":"  "}]}"#,
        )
        .expect("write tasks");

        let error = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            core: DispatchCoreInputs {
                tasks_json: Some(format!("@{}", tasks.path().display())),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect_err("object without usable prompt should fail");

        assert_eq!(error.details["field"], "tasks");
        assert!(error.to_string().contains("task item 1"));
        assert!(error.to_string().contains("prompt or instructions field"));
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
    fn dispatch_plan_promotes_client_context_artifact_dependencies_to_provider_config() {
        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Run the declared workflow.".to_string()),
            core: DispatchCoreInputs {
                client_context: Some(
                    serde_json::json!({
                        "schema": "homeboy/repo-loop-workflow-context/v1",
                        "artifact_dependencies": [{
                            "artifact_id": "concept_packet",
                            "kind": "typed_packet",
                            "required": true,
                            "producer_workflow_ids": ["store-idea"]
                        }]
                    })
                    .to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        assert_eq!(
            plan.tasks[0].executor.config["artifact_dependencies"],
            serde_json::json!([{
                "artifact_id": "concept_packet",
                "kind": "typed_packet",
                "required": true,
                "producer_workflow_ids": ["store-idea"]
            }])
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
                    run_id: Some("dispatch-queued".to_string()),
                    core: DispatchCoreInputs {
                        queue_only: true,
                        ..DispatchCoreInputs::default()
                    },
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
            secret_env: vec!["OPENAI_API_KEY".to_string()],
            core: DispatchCoreInputs {
                provider_config: Some(
                    serde_json::json!({
                        "provider": "example",
                        "secret_env": ["HOMEBOY_PROVIDER_ACCESS_TOKEN"],
                        "secretEnv": "HOMEBOY_PROVIDER_REFRESH_TOKEN"
                    })
                    .to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
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
                core: DispatchCoreInputs {
                    provider_config: Some(
                        serde_json::json!({ "provider": "override" }).to_string(),
                    ),
                    ..DispatchCoreInputs::default()
                },
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

    #[test]
    fn patch_provider_requires_workspace_path_to_be_git_checkout() {
        let worktree = tempfile::tempdir().expect("workspace");

        let err = build_dispatch_plan_with_provider_requirements(
            &dispatch_request(DispatchRequestOverrides {
                prompt: Some("Cook generic workspace.".to_string()),
                workspace: Some(worktree.path().display().to_string()),
                backend: Some("patch-provider".to_string()),
                ..DispatchRequestOverrides::default()
            }),
            |backend, _| backend == "patch-provider",
        )
        .expect_err("workspace path should require git checkout");

        assert_eq!(err.details["field"], "workspace");
        assert!(err.message.contains("dispatch workspace"));
    }

    #[test]
    fn dispatch_plan_promotes_runtime_component_contracts_from_client_context_inputs() {
        let component = tempfile::tempdir().expect("agents-api component");
        init_git_repo(component.path());
        let component_path = component.path().display().to_string();

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook with runtime components.".to_string()),
            core: DispatchCoreInputs {
                client_context: Some(
                    serde_json::json!({
                        "schema": "homeboy/repo-loop-workflow-context/v1",
                        "inputs": {
                            "runtime_component_contracts": [{
                                "slug": "agents-api",
                                "path": component_path,
                                "loadAs": "plugin",
                                "activate": true,
                                "opaque": { "preserve": "yes" }
                            }]
                        }
                    })
                    .to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        let contracts = &plan.tasks[0].component_contracts;
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].slug.as_deref(), Some("agents-api"));
        assert_eq!(contracts[0].path.as_deref(), Some(component_path.as_str()));
        assert_eq!(contracts[0].load_as.as_deref(), Some("plugin"));
        assert_eq!(contracts[0].activate, Some(true));
        assert_eq!(contracts[0].extra["opaque"]["preserve"], "yes");
        assert_eq!(
            plan.tasks[0].executor.config["component_contracts"][0]["path"],
            component_path
        );

        // The declared runtime dependency graph is resolved at a known ref and
        // recorded in run evidence (#6121).
        let graph = &plan.tasks[0].metadata["runtime_dependency_graph"];
        assert_eq!(
            graph["schema"],
            crate::core::agent_task_runtime_dependency_graph::RUNTIME_DEPENDENCY_GRAPH_SCHEMA
        );
        assert_eq!(graph["dependencies"][0]["slug"], "agents-api");
        assert!(graph["dependencies"][0]["resolved_ref"].is_string());
        assert!(graph["dependencies"][0]["git_provenance"]
            .as_bool()
            .unwrap());
        assert_eq!(
            plan.metadata["runtime_dependency_graph"]["dependencies"][0]["slug"],
            "agents-api"
        );
    }

    #[test]
    fn dispatch_plan_accepts_component_contracts_from_provider_config() {
        let component = tempfile::tempdir().expect("runtime-helper component");
        init_git_repo(component.path());
        let component_path = component.path().display().to_string();

        let plan = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook with provider runtime components.".to_string()),
            core: DispatchCoreInputs {
                provider_config: Some(
                    serde_json::json!({
                        "component_contracts": [{
                            "slug": "runtime-helper",
                            "path": component_path,
                            "loadAs": "mu-plugin",
                            "activate": false,
                            "opaque_provider_hint": { "preserved": true }
                        }]
                    })
                    .to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect("dispatch plan");

        let contract = &plan.tasks[0].component_contracts[0];
        assert_eq!(contract.slug.as_deref(), Some("runtime-helper"));
        assert_eq!(contract.path.as_deref(), Some(component_path.as_str()));
        assert_eq!(contract.load_as.as_deref(), Some("mu-plugin"));
        assert_eq!(contract.activate, Some(false));
        assert_eq!(contract.extra["opaque_provider_hint"]["preserved"], true);
    }

    #[test]
    fn dispatch_plan_preflight_fails_on_unresolved_runtime_dependency() {
        let error = build_dispatch_plan(&dispatch_request(DispatchRequestOverrides {
            prompt: Some("Cook with a missing runtime dependency.".to_string()),
            core: DispatchCoreInputs {
                provider_config: Some(
                    serde_json::json!({
                        "component_contracts": [{
                            "slug": "data-machine",
                            "path": "/nonexistent/data-machine/checkout",
                            "loadAs": "plugin",
                            "activate": true
                        }]
                    })
                    .to_string(),
                ),
                ..DispatchCoreInputs::default()
            },
            ..DispatchRequestOverrides::default()
        }))
        .expect_err("missing runtime dependency fails dispatch preflight");

        assert!(error.message.contains("could not be resolved"));
        assert!(error.message.contains("data-machine"));
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
                typed_artifacts: Vec::new(),
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
        cwd: Option<String>,
        workspace: Option<String>,
        repo: Option<String>,
        task_url: Option<String>,
        secret_env: Vec<String>,
        required_capabilities: Vec<String>,
        concurrency: usize,
        run_id: Option<String>,
        backend: Option<String>,
        core: DispatchCoreInputs,
    }

    fn dispatch_request(overrides: DispatchRequestOverrides) -> AgentTaskDispatchRequest {
        AgentTaskDispatchRequest {
            prompt: overrides.prompt,
            tasks: Vec::new(),
            cwd: overrides.cwd,
            workspace: overrides.workspace,
            repo: overrides.repo,
            task_url: overrides.task_url,
            backend: overrides.backend.unwrap_or_else(|| "fixture".to_string()),
            selector: None,
            model: None,
            required_capabilities: overrides.required_capabilities,
            secret_env: overrides.secret_env,
            concurrency: if overrides.concurrency == 0 {
                1
            } else {
                overrides.concurrency
            },
            run_id: overrides.run_id,
            core: DispatchCoreInputs {
                tasks_json: overrides.core.tasks_json,
                provider_config: overrides.core.provider_config,
                client_context: overrides.core.client_context,
                attempts: if overrides.core.attempts == 0 {
                    1
                } else {
                    overrides.core.attempts
                },
                queue_only: overrides.core.queue_only,
            },
            backend_selection: None,
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
