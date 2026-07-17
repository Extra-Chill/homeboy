//! Core service for durable agent-task dispatch.
//!
//! The CLI adapter owns clap parsing and JSON rendering. This service owns the
//! typed dispatch request, plan construction, provider preflight, durable
//! lifecycle transitions, and scheduler orchestration.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_dispatch_plan::{
    build_dispatch_plan, build_dispatch_plan_with_provider_requirements,
    preflight_dispatch_provider_secrets,
};
use crate::agent_task_lifecycle as lifecycle;
use crate::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
use crate::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, default_backend_for_component,
    enforce_runtime_preflight_checks_for_plan, preflight_plan_provider_config_with_providers,
    resolve_provider_for_backend, AgentTaskProviderCatalog, ProviderResolution,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskProviderRotationPolicy,
    AgentTaskRetryPolicy, AgentTaskScheduler,
};
use crate::agent_task_service::{aggregate_exit_code, terminal_run_result, AgentTaskRunResult};
use crate::{Error, Result};

pub const DISPATCH_RESULT_SCHEMA: &str = "homeboy/agent-task-dispatch/v1";

// `ResolvedAgentTaskProviderPolicy` is a plain data struct that lives in the
// leaf `agent_task_schedule` module (beside its `AgentTaskProviderRotationPolicy`
// / `AgentTaskRetryPolicy` fields) so the lab-contract type layer can depend on
// it without pulling in this service's dispatch machinery. Re-exported here to
// keep existing `agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy`
// call sites stable.
pub use crate::agent_task_schedule::ResolvedAgentTaskProviderPolicy;

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
    /// Total provider executions per task, including the first attempt.
    pub attempts: u32,
    /// Explicit same-provider retry budget after the initial execution.
    pub same_provider_retries: u32,
    /// Explicit cross-provider rotation budget after the initial execution.
    pub provider_rotations: u32,
    /// Persist the run for a daemon/runner but do not execute immediately.
    pub queue_only: bool,
    /// Optional provider wall-clock timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Internal Lab handoff input, compiled on the controller.
    pub resolved_provider_policy: Option<ResolvedAgentTaskProviderPolicy>,
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
    pub task_id: Option<String>,
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

fn dispatch_with_provider_catalog<E>(
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
    catalog.enforce_runtime_preflight_checks_for_plan(&plan)?;
    preflight_dispatch_provider_secrets(&plan)?;
    preflight_plan_provider_config_with_providers(&plan, catalog.providers())?;
    run_dispatch_plan(
        plan,
        request.run_id.as_deref(),
        request.core.queue_only,
        backend_selection,
        executor,
    )
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
    enforce_runtime_preflight_checks_for_plan(&plan)?;
    preflight_dispatch_provider_secrets(&plan)?;
    preflight_plan_provider_config_with_providers(
        &plan,
        &AgentTaskProviderCatalog::discover().providers,
    )?;
    run_dispatch_plan(
        plan,
        request.run_id.as_deref(),
        request.core.queue_only,
        backend_selection,
        executor,
    )
}

/// The only durable dispatch-to-scheduler path. Both provider-catalog entry
/// points prepare a plan differently, then share lifecycle transitions,
/// scheduler execution, aggregate persistence, and report construction here.
fn run_dispatch_plan<E>(
    plan: AgentTaskPlan,
    requested_run_id: Option<&str>,
    queue_only: bool,
    backend_selection: Option<BackendSelection>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>>
where
    E: AgentTaskExecutorAdapter,
{
    let submitted = lifecycle::submit_plan(&plan, requested_run_id)?;
    let run_id = submitted.run_id.clone();

    if queue_only {
        return Ok(AgentTaskRunResult {
            value: dispatch_report(submitted, None, true, backend_selection),
            exit_code: 0,
        });
    }

    if let Some(result) = terminal_run_result(&run_id)? {
        return Ok(AgentTaskRunResult {
            value: dispatch_report(submitted, Some(result.value), false, backend_selection),
            exit_code: result.exit_code,
        });
    }

    let harvest_context =
        match crate::agent_task_scheduler::HarvestExecutionContext::from_current_process() {
            Ok(context) => context,
            Err(error) => {
                lifecycle::record_pre_execution_failure(
                    &run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
    lifecycle::mark_running(&run_id)?;
    let aggregate = AgentTaskScheduler::new_controller(executor)
        .with_harvest_context(harvest_context)
        .with_run_id(run_id.clone())
        .run(plan.clone());
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
    pub task_id: Option<String>,
    pub core: DispatchCoreInputs,
}

/// Resolve a typed dispatch request from command-surface inputs, applying the
/// declared default-backend policy when `--backend` is absent.
pub fn resolve_dispatch_request(
    command: AgentTaskDispatchCommand,
) -> Result<AgentTaskDispatchRequest> {
    resolve_dispatch_request_with_default(command, default_backend_for_component)
}

/// Resolve the provider policy the controller passes to an execution boundary.
/// The first configured rotation entry is the initial attempt; remaining
/// entries are failover attempts.
pub fn controller_resolved_execution_policy(
    request: &AgentTaskDispatchRequest,
) -> ResolvedAgentTaskProviderPolicy {
    let runtime_identity = selected_runtime_identity(request);
    let explicit_backend = request
        .backend_selection
        .as_ref()
        .is_some_and(|selection| selection.source == BackendSelectionSource::Cli);
    let rotation = crate::defaults::load_config()
        .agent_task
        .rotation
        .map(|mut rotation| {
            if explicit_backend {
                // A controller pin describes one immutable runtime. Global fallback
                // entries for another runtime must not replace an explicit choice.
                rotation.entries.retain(|entry| {
                    entry
                        .backend
                        .as_deref()
                        .is_none_or(|backend| backend == request.backend)
                        && entry.selector.as_deref().is_none_or(|selector| {
                            runtime_identity
                                .as_ref()
                                .is_some_and(|identity| identity.provider_id == selector)
                        })
                });
            }
            rotation
        });
    ResolvedAgentTaskProviderPolicy {
        backend: request.backend.clone(),
        selector: request.selector.clone(),
        model: request.model.clone(),
        retry: AgentTaskRetryPolicy {
            max_attempts: request.core.attempts.max(1),
            ..AgentTaskRetryPolicy::default()
        },
        liveness_timeout_ms: rotation
            .as_ref()
            .and_then(|policy| policy.liveness_timeout_ms),
        rotation,
        // An explicit CLI backend/model is the first attempt. Default policy
        // rotation retains its existing first-entry selection behavior.
        rotation_starts_with_first_entry: !explicit_backend,
        runtime_identity,
    }
}

fn selected_runtime_identity(
    request: &AgentTaskDispatchRequest,
) -> Option<crate::agent_task_config::ResolvedAgentTaskRuntimeIdentity> {
    let catalog = AgentTaskProviderCatalog::discover();
    let ProviderResolution::Resolved(provider) = resolve_provider_for_backend(
        catalog.providers(),
        &request.backend,
        request.selector.as_deref(),
    ) else {
        return None;
    };
    let plan = provider.extra.get("runtime_materialization_plan")?;
    let plan: crate::agent_runtime_manifest::AgentRuntimeMaterializationPlan =
        serde_json::from_value(plan.clone()).ok()?;
    let source_revision = plan.source_revision.clone()?;
    if !crate::agent_runtime_manifest::is_immutable_revision(&source_revision) {
        return None;
    }
    let materialization_plan = serde_json::to_value(&plan).ok()?;
    Some(crate::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
        runtime_id: plan.runtime_id,
        provider_id: plan.provider_id,
        source_selector: plan.source_selector,
        source_revision,
        freshness: match plan.freshness {
            crate::agent_runtime_manifest::AgentRuntimeFreshness::Pinned => {
                crate::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Pinned
            }
            crate::agent_runtime_manifest::AgentRuntimeFreshness::Unverifiable => {
                crate::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable
            }
        },
        provider: serde_json::to_value(provider).ok()?,
        materialization_plan,
    })
}

/// Build a controller-owned plan with a durable execution policy when the
/// caller did not submit one explicitly.
pub fn build_controller_dispatch_plan(
    request: &mut AgentTaskDispatchRequest,
) -> Result<AgentTaskPlan> {
    if request.core.resolved_provider_policy.is_none() {
        request.core.resolved_provider_policy = Some(controller_resolved_execution_policy(request));
    }
    build_dispatch_plan(request)
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
    crate::defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty())
}

/// Resolution core that also takes the Homeboy-config default resolver so tests
/// can drive deterministic source classification (#5685).
fn resolve_dispatch_request_with_default_and_config(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    config_default: impl FnOnce() -> Option<String>,
) -> Result<AgentTaskDispatchRequest> {
    let config_default = config_default();
    let submitted_policy = command.core.resolved_provider_policy.clone();
    let (backend, source) = match submitted_policy.as_ref() {
        Some(policy) => (policy.backend.clone(), BackendSelectionSource::Policy),
        None => match command.backend.clone() {
            Some(backend) => (backend, BackendSelectionSource::Cli),
            None => {
                let resolved = default_backend(command.repo.as_deref())?.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "backend",
                    "agent-task cook requires --backend because no default backend policy is configured",
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
        },
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
        selector: submitted_policy
            .as_ref()
            .and_then(|policy| policy.selector.clone())
            .or(command.selector),
        model: submitted_policy
            .as_ref()
            .and_then(|policy| policy.model.clone())
            .or(command.model),
        required_capabilities: command.required_capabilities,
        secret_env: command.secret_env,
        concurrency: command.concurrency,
        run_id: command.run_id,
        task_id: command.task_id,
        core: command.core,
        backend_selection,
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

fn command_json_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Error::internal_json(error.to_string(), None))
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
    fn explicit_backend_keeps_its_runtime_pin_when_default_rotation_targets_another_backend() {
        crate::test_support::with_isolated_home(|home| {
            let runtimes = home.path().join(".config/homeboy/agent-runtimes");
            for (runtime_id, provider_id, backend) in [
                ("opencode", "opencode.agent-task-executor", "opencode"),
                ("codex", "codex.agent-task-executor", "codex"),
            ] {
                let runtime = runtimes.join(runtime_id);
                std::fs::create_dir_all(&runtime).expect("runtime directory");
                std::fs::write(
                    runtime.join(format!("{runtime_id}.json")),
                    serde_json::json!({
                        "schema": crate::agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                        "id": runtime_id,
                        "source_revision": "0123456789abcdef0123456789abcdef01234567",
                        "agent_task_executors": [{
                            "schema": crate::agent_task_provider::AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
                            "id": provider_id,
                            "backend": backend,
                            "invocation": { "argv": ["provider"] }
                        }]
                    })
                    .to_string(),
                )
                .expect("runtime manifest");
                for args in [
                    vec!["init"],
                    vec!["add", "."],
                    vec![
                        "-c",
                        "user.name=Homeboy Test",
                        "-c",
                        "user.email=homeboy-test@example.com",
                        "commit",
                        "-m",
                        "runtime manifest",
                    ],
                ] {
                    let status = std::process::Command::new("git")
                        .args(args)
                        .current_dir(&runtime)
                        .status()
                        .expect("run git");
                    assert!(status.success(), "initialize runtime source");
                }
            }

            let mut config = crate::defaults::load_config();
            config.agent_task.default_backend = Some("opencode".to_string());
            config.agent_task.rotation = Some(AgentTaskProviderRotationPolicy {
                entries: vec![
                    crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                        backend: Some("opencode".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            });
            crate::defaults::save_config(&config).expect("save config");

            let request = resolve_dispatch_request_with_default_and_config(
                command_with_backend(Some("codex")),
                |_| Ok(Some("opencode".to_string())),
                || Some("opencode".to_string()),
            )
            .expect("explicit Codex request");
            let policy = controller_resolved_execution_policy(&request);

            assert_eq!(policy.backend, "codex");
            assert!(!policy.rotation_starts_with_first_entry);
            assert!(policy.rotation.expect("rotation").entries.is_empty());
            let identity = policy.runtime_identity.expect("Codex runtime identity");
            assert_eq!(identity.runtime_id, "codex");
            assert_eq!(identity.provider_id, "codex.agent-task-executor");
            assert_eq!(identity.provider["backend"], "codex");
        });
    }
}
