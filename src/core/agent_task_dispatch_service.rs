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
    AgentTaskProviderCatalog,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskScheduler,
};
use crate::core::agent_task_service::{aggregate_exit_code, AgentTaskRunResult};
use crate::core::{Error, Result};

pub const DISPATCH_RESULT_SCHEMA: &str = "homeboy/agent-task-dispatch/v1";

/// Where the effective agent-task backend selection came from. Surfaced before
/// dispatch so operators can see whether the backend was an explicit override or
/// resolved from the configured default policy (#5685).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendSelectionSource {
    /// Explicit `--backend` flag on the command line.
    Cli,
    /// Resolved from a component- or extension-scoped `agent_task.default_backend`
    /// policy (higher priority than the Homeboy config default).
    Policy,
    /// Resolved from the Homeboy config `agent_task.default_backend`.
    Config,
}

impl BackendSelectionSource {
    /// Short human label for summary/warning rendering.
    pub fn label(self) -> &'static str {
        match self {
            BackendSelectionSource::Cli => "cli (--backend)",
            BackendSelectionSource::Policy => "config (component/extension default)",
            BackendSelectionSource::Config => "config (homeboy default)",
        }
    }
}

/// The effective backend plus where it came from and whether it overrides the
/// configured default. Attached to the dispatch report and rendered as a
/// pre-dispatch summary by the command adapter (#5685).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendSelection {
    /// The backend that will actually be dispatched.
    pub backend: String,
    /// Where the selection came from.
    pub source: BackendSelectionSource,
    /// The configured default backend, when any policy declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
    /// True when an explicit `--backend` differs from the configured default.
    pub overrides_default: bool,
}

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
    /// Effective backend selection metadata, surfaced before dispatch (#5685).
    pub backend_selection: Option<BackendSelection>,
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
    /// Effective backend selection + source, mirrored into the report so logs and
    /// status metadata record the override decision (#5685).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_selection: Option<BackendSelection>,
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
    let catalog = AgentTaskProviderCatalog::discover();
    dispatch_with_provider_catalog(request, executor, &catalog)
}

pub fn dispatch_with_provider_catalog<E>(
    request: AgentTaskDispatchRequest,
    executor: E,
    catalog: &AgentTaskProviderCatalog,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>>
where
    E: AgentTaskExecutorAdapter,
{
    let backend_selection = request.backend_selection.clone();
    let mut plan =
        build_dispatch_plan_with_provider_requirements(&request, |backend, selector| {
            catalog.provider_requires_cwd_git_checkout(backend, selector)
        })?;
    catalog.apply_provider_runner_secret_env_contracts(&mut plan);
    preflight_dispatch_provider_secrets(&plan)?;
    let submitted = lifecycle::submit_plan(&plan, request.run_id.as_deref())?;
    let run_id = submitted.run_id.clone();

    if request.core.queue_only {
        return Ok(AgentTaskRunResult {
            value: dispatch_report(submitted, None, true, backend_selection),
            exit_code: 0,
        });
    }

    lifecycle::mark_running(&run_id)?;
    let aggregate = AgentTaskScheduler::new(executor).run(plan.clone());
    let record = lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = aggregate_exit_code(&aggregate);

    Ok(AgentTaskRunResult {
        value: dispatch_report(record, Some(aggregate), false, backend_selection),
        exit_code,
    })
}

pub fn dispatch_with_provider_requirements<E>(
    request: AgentTaskDispatchRequest,
    executor: E,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>>
where
    E: AgentTaskExecutorAdapter,
{
    let backend_selection = request.backend_selection.clone();
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
            value: dispatch_report(submitted, None, true, backend_selection),
            exit_code: 0,
        });
    }

    lifecycle::mark_running(&run_id)?;
    let aggregate = AgentTaskScheduler::new(executor).run(plan.clone());
    let record = lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = aggregate_exit_code(&aggregate);

    Ok(AgentTaskRunResult {
        value: dispatch_report(record, Some(aggregate), false, backend_selection),
        exit_code,
    })
}

fn dispatch_report(
    record: AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
    queued: bool,
    backend_selection: Option<BackendSelection>,
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
        backend_selection,
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
    resolve_dispatch_request_with_default_and_config(
        command,
        default_backend,
        config_default_backend,
    )
}

/// The configured Homeboy-config `agent_task.default_backend`, when set. Read via
/// the public `defaults` surface so the dispatch service can name the selection
/// source without touching fleet-owned provider policy code (#5685).
fn config_default_backend() -> Option<String> {
    crate::core::defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty())
}

/// Resolution core that also takes the Homeboy-config default resolver so tests
/// can drive deterministic source classification (#5685).
pub fn resolve_dispatch_request_with_default_and_config(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    config_default: impl FnOnce() -> Option<String>,
) -> Result<AgentTaskDispatchRequest> {
    let config_default = config_default();
    let (backend, source) = match command.backend {
        Some(backend) => (backend, BackendSelectionSource::Cli),
        None => {
            let resolved = default_backend(command.repo.as_deref())?.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "backend",
                    "agent-task dispatch requires --backend because no default backend policy is configured",
                    None,
                    Some(vec![
                        "Set agent_task.default_backend in component, extension, or Homeboy config policy, or pass --backend explicitly.".to_string(),
                    ]),
                )
            })?;
            // The policy resolver prefers component/extension defaults over the
            // Homeboy config default; if the resolved value matches the config
            // default we attribute it to config, otherwise to higher-priority
            // component/extension policy.
            let source = if config_default.as_deref() == Some(resolved.as_str()) {
                BackendSelectionSource::Config
            } else {
                BackendSelectionSource::Policy
            };
            (resolved, source)
        }
    };

    let overrides_default = source == BackendSelectionSource::Cli
        && config_default
            .as_deref()
            .map(|default| default != backend.as_str())
            .unwrap_or(false);

    let backend_selection = Some(BackendSelection {
        backend: backend.clone(),
        source,
        default_backend: config_default,
        overrides_default,
    });

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
        backend_selection,
    })
}

/// Render a human-readable, single-line-per-field backend selection summary for
/// the command adapter to print to stderr before dispatch (#5685). Kept in the
/// service so the wording stays consistent across cook and dispatch surfaces.
pub fn render_backend_selection_summary(selection: &BackendSelection) -> String {
    let mut lines = vec![
        "agent-task backend selection:".to_string(),
        format!("  backend: {}", selection.backend),
        format!("  source:  {}", selection.source.label()),
    ];
    match selection.default_backend.as_deref() {
        Some(default) => lines.push(format!("  default: {default}")),
        None => lines.push("  default: <none configured>".to_string()),
    }
    if selection.overrides_default {
        if let Some(default) = selection.default_backend.as_deref() {
            lines.push(format!(
                "  warning: --backend '{}' overrides the configured default '{}'",
                selection.backend, default
            ));
        }
    }
    lines.join("\n")
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
    let catalog = AgentTaskProviderCatalog::discover();
    run_dispatch_command_with_provider_catalog(command, executor, &catalog)
}

pub fn run_dispatch_command_with_provider_catalog<E>(
    command: AgentTaskDispatchCommand,
    executor: E,
    catalog: &AgentTaskProviderCatalog,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter,
{
    let request = resolve_dispatch_request(command)?;
    let result = dispatch_with_provider_catalog(request, executor, catalog)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

/// Run a cook invocation: dispatch the request and attach the cook handoff
/// envelope describing the patch-artifact boundary and promotion next actions.
pub fn run_cook_command<E>(command: AgentTaskDispatchCommand, executor: E) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter,
{
    let catalog = AgentTaskProviderCatalog::discover();
    run_cook_command_with_provider_catalog(command, executor, &catalog)
}

pub fn run_cook_command_with_provider_catalog<E>(
    command: AgentTaskDispatchCommand,
    executor: E,
    catalog: &AgentTaskProviderCatalog,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter,
{
    let target_worktree = command.workspace.clone();
    let (mut value, exit_code) =
        run_dispatch_command_with_provider_catalog(command, executor, catalog)?;
    value["handoff"] = cook_handoff(&value, target_worktree.as_deref());
    Ok((value, exit_code))
}

fn command_json_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Error::internal_json(error.to_string(), None))
}

fn cook_handoff(value: &Value, to_worktree: Option<&str>) -> Value {
    let run_id = value["run_id"].as_str().unwrap_or("<run-id>");
    let run_command = agent_task_run_command(value, run_id);
    let run_next_command = agent_task_run_next_command(value);
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
            "queued only; run `{run_command}` or let a daemon claim it with `{run_next_command}`"
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

fn agent_task_run_command(value: &Value, run_id: &str) -> String {
    match value
        .pointer("/record/metadata/runner_id")
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty())
    {
        Some(runner_id) => format!("homeboy --runner {runner_id} agent-task run {run_id}"),
        None => format!("homeboy agent-task run {run_id}"),
    }
}

fn agent_task_run_next_command(value: &Value) -> String {
    match value
        .pointer("/record/metadata/runner_id")
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty())
    {
        Some(runner_id) => format!("homeboy --runner {runner_id} agent-task run-next"),
        None => "homeboy agent-task run-next".to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn command_with_backend(backend: Option<&str>) -> AgentTaskDispatchCommand {
        AgentTaskDispatchCommand {
            prompt: Some("task".to_string()),
            backend: backend.map(str::to_string),
            ..AgentTaskDispatchCommand::default()
        }
    }

    #[test]
    fn explicit_backend_is_attributed_to_cli_source() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("claude-code")),
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "claude-code");
        assert_eq!(selection.source, BackendSelectionSource::Cli);
        assert_eq!(selection.default_backend.as_deref(), Some("opencode"));
        assert!(selection.overrides_default);
    }

    #[test]
    fn explicit_backend_matching_default_does_not_warn() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("opencode")),
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.source, BackendSelectionSource::Cli);
        assert!(!selection.overrides_default);
    }

    #[test]
    fn config_default_backend_is_attributed_to_config_source() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(None),
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "opencode");
        assert_eq!(selection.source, BackendSelectionSource::Config);
        assert!(!selection.overrides_default);
    }

    #[test]
    fn component_or_extension_default_is_attributed_to_policy_source() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(None),
            |_| Ok(Some("component-backend".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "component-backend");
        assert_eq!(selection.source, BackendSelectionSource::Policy);
        // Default-policy selections never count as an explicit override.
        assert!(!selection.overrides_default);
    }

    #[test]
    fn summary_includes_override_warning_only_for_cli_override() {
        let override_selection = BackendSelection {
            backend: "claude-code".to_string(),
            source: BackendSelectionSource::Cli,
            default_backend: Some("opencode".to_string()),
            overrides_default: true,
        };
        let summary = render_backend_selection_summary(&override_selection);
        assert!(summary.contains("backend: claude-code"));
        assert!(summary.contains("cli (--backend)"));
        assert!(summary.contains("warning:"));
        assert!(summary.contains("overrides the configured default 'opencode'"));

        let default_selection = BackendSelection {
            backend: "opencode".to_string(),
            source: BackendSelectionSource::Config,
            default_backend: Some("opencode".to_string()),
            overrides_default: false,
        };
        let summary = render_backend_selection_summary(&default_selection);
        assert!(!summary.contains("warning:"));
    }

    #[test]
    fn queued_cook_handoff_uses_recorded_runner_context() {
        let handoff = cook_handoff(
            &serde_json::json!({
                "run_id": "agent-task-queued",
                "queued": true,
                "record": {
                    "metadata": {
                        "runner_id": "homeboy-lab"
                    }
                }
            }),
            None,
        );

        let next_action = handoff["next_actions"][0].as_str().expect("next action");
        assert!(
            next_action.contains("homeboy --runner homeboy-lab agent-task run agent-task-queued")
        );
        assert!(next_action.contains("homeboy --runner homeboy-lab agent-task run-next"));
    }
}
