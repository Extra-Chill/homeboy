//! Core service for durable agent-task dispatch.
//!
//! The CLI adapter owns clap parsing and JSON rendering. This service owns the
//! typed dispatch request, plan construction, provider preflight, durable
//! lifecycle transitions, and scheduler orchestration.

use serde::Serialize;
use serde_json::Value;

use crate::agent_task_dispatch_plan::{
    build_dispatch_plan, build_dispatch_plan_with_provider_requirements,
    preflight_dispatch_provider_secrets,
};
use crate::agent_task_lifecycle as lifecycle;
use crate::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
use crate::agent_task_provider::{
    default_backend_for_component, preflight_plan_provider_config_with_providers,
    preflight_provider_credentials_for_backend, resolve_provider_for_backend,
    AgentTaskProviderCatalog, ProviderResolution,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskPlan, AgentTaskProviderRotationPolicy, AgentTaskRetryPolicy,
    AgentTaskScheduler, SharedAgentTaskExecutor,
};
use crate::agent_task_service::{aggregate_exit_code, terminal_run_result, AgentTaskRunResult};
use homeboy_core::{Error, Result};

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

/// The initial model decision persisted with a dispatch plan. This avoids
/// deriving execution provenance from presentation-oriented disclosure text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentTaskModelSelection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    pub reason: AgentTaskModelSelectionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskModelSelectionReason {
    ExplicitRequest,
    PolicyRotation,
    Default,
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
    /// `None` means the caller did not ask for a value, so the configured
    /// provider rotation gets to fund its own reachability (#11082).
    pub attempts: Option<u32>,
    /// Explicit same-provider retry budget after the initial execution.
    /// `None` means unspecified, which resolves to zero: same-provider retries
    /// fund Cook gate and review-form remediation and are never derived from a
    /// rotation chain.
    pub same_provider_retries: Option<u32>,
    /// Explicit cross-provider rotation budget after the initial execution.
    /// `None` means unspecified, which resolves from the configured rotation.
    pub provider_rotations: Option<u32>,
    /// Persist the run for a daemon/runner but do not execute immediately.
    pub queue_only: bool,
    /// Optional provider wall-clock timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Internal Lab handoff input, compiled on the controller.
    pub resolved_provider_policy: Option<ResolvedAgentTaskProviderPolicy>,
    /// Command patterns the provider agent must not run, additive to the
    /// host-level `agent_task.command_policy` config (#11481).
    pub deny_command: Vec<String>,
    /// Command patterns the provider agent may run. Any entry switches the
    /// effective policy to allow-list mode.
    pub allow_command: Vec<String>,
    /// Operator explanation returned to the agent with every refusal.
    pub command_policy_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskDispatchRequest {
    pub prompt: Option<String>,
    /// The CLI captured this prompt from a structured source already. Treat it
    /// as literal task text rather than parsing it as another source spec.
    pub prompt_is_literal: bool,
    pub tasks: Vec<String>,
    pub cwd: Option<String>,
    pub workspace: Option<String>,
    pub repo: Option<String>,
    pub component: Option<String>,
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

/// The initial backend, provider selector, and model Cook will use after its
/// configured rotation policy is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskInitialProviderRoute {
    pub backend: String,
    pub selector: Option<String>,
    pub model: Option<String>,
    pub provider_config: Value,
    pub rotation: Option<AgentTaskProviderRotationPolicy>,
    pub rotation_selected_initial: bool,
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

pub fn dispatch(
    request: AgentTaskDispatchRequest,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>> {
    let catalog = AgentTaskProviderCatalog::discover();
    dispatch_with_provider_catalog(request, executor, &catalog)
}

fn dispatch_with_provider_catalog(
    request: AgentTaskDispatchRequest,
    executor: SharedAgentTaskExecutor,
    catalog: &AgentTaskProviderCatalog,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>> {
    let backend_selection = request.backend_selection.clone();
    let plan = build_dispatch_plan_with_provider_requirements(&request, |backend, selector| {
        catalog.provider_requires_cwd_git_checkout(backend, selector)
    })?;
    let execution_plan =
        match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
            &plan,
            catalog,
            &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
        ) {
            Ok(plan) => {
                validate_selected_execution_plan(&plan, catalog)?;
                plan
            }
            Err(error) if error.retryable == Some(true) => plan.clone(),
            Err(error) => {
                preflight_dispatch_provider_secrets(&plan)?;
                return Err(with_declared_credential_hints(error, &plan, catalog));
            }
        };
    // Keep the complete route chain durable. The scheduler evaluates readiness
    // immediately before each possible execution and records zero-dispatch
    // exhaustion evidence when every route is unavailable.
    run_dispatch_plan(
        plan,
        execution_plan,
        request.run_id.as_deref(),
        request.core.queue_only,
        backend_selection,
        executor,
    )
}

/// Validate the reachable provider routes needed to dispatch this request.
pub fn preflight_dispatch_provider_admission(
    request: &AgentTaskDispatchRequest,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    let mut plan = build_dispatch_plan_with_provider_requirements(request, |backend, selector| {
        catalog.provider_requires_cwd_git_checkout(backend, selector)
    })?;
    let plan = match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
        &plan,
        catalog,
        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            if !matches!(
                resolve_provider_for_backend(
                    catalog.providers(),
                    &request.backend,
                    request.selector.as_deref()
                ),
                ProviderResolution::Resolved(_)
            ) {
                crate::agent_task_provider::validate_provider_runner_readiness_for_backend_with_catalog(
                catalog,
                &request.backend,
                request.selector.as_deref(),
            )?;
            }
            preflight_provider_credentials_for_backend(
                catalog.providers(),
                &request.backend,
                request.selector.as_deref(),
            )?;
            catalog.apply_provider_runner_secret_env_contracts(&mut plan);
            catalog.validate_selected_models(&plan)?;
            preflight_dispatch_provider_secrets(&plan)?;
            preflight_plan_provider_config_with_providers(&plan, catalog.providers())?;
            crate::agent_task_provider::preflight_plan_provider_dispatchability_without_runtime_with_providers(
            &plan,
            catalog,
            &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
        )?;
            return Err(error);
        }
    };
    catalog.validate_selected_models(&plan)?;
    preflight_dispatch_provider_secrets(&plan)?;
    preflight_plan_provider_config_with_providers(&plan, catalog.providers())?;
    crate::agent_task_provider::preflight_plan_provider_dispatchability_without_runtime_with_providers(
        &plan,
        catalog,
        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
    )
}

pub fn dispatch_with_provider_requirements(
    request: AgentTaskDispatchRequest,
    executor: SharedAgentTaskExecutor,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>> {
    let backend_selection = request.backend_selection.clone();
    let plan = build_dispatch_plan_with_provider_requirements(
        &request,
        provider_requires_cwd_git_checkout,
    )?;
    let catalog = AgentTaskProviderCatalog::discover();
    let execution_plan =
        match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
        ) {
            Ok(plan) => {
                validate_selected_execution_plan(&plan, &catalog)?;
                plan
            }
            Err(error) if error.retryable == Some(true) => plan.clone(),
            Err(error) => {
                preflight_dispatch_provider_secrets(&plan)?;
                return Err(with_declared_credential_hints(error, &plan, &catalog));
            }
        };
    run_dispatch_plan(
        plan,
        execution_plan,
        request.run_id.as_deref(),
        request.core.queue_only,
        backend_selection,
        executor,
    )
}

fn validate_selected_execution_plan(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    catalog.validate_selected_models(plan)?;
    catalog.enforce_runtime_preflight_checks_for_plan(plan)?;
    preflight_dispatch_provider_secrets(plan)?;
    preflight_plan_provider_config_with_providers(plan, catalog.providers())?;
    crate::agent_task_provider::preflight_plan_provider_dispatchability_without_runtime_with_providers(
        plan,
        catalog,
        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
    )
}

fn with_declared_credential_hints(
    mut error: Error,
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
) -> Error {
    for provider in catalog.providers().iter().filter(|provider| {
        plan.tasks
            .iter()
            .any(|task| task.executor.backend == provider.backend)
    }) {
        for name in crate::agent_task_provider::provider_required_secret_env_names(provider) {
            if !error.hints.iter().any(|hint| hint.message.contains(&name)) {
                error.hints.push(homeboy_core::error::Hint {
                    message: format!("Required provider credential: {name}"),
                });
            }
        }
    }
    error
}

/// The only durable dispatch-to-scheduler path. Both provider-catalog entry
/// points prepare a plan differently, then share lifecycle transitions,
/// scheduler execution, aggregate persistence, and report construction here.
fn run_dispatch_plan(
    durable_plan: AgentTaskPlan,
    execution_plan: AgentTaskPlan,
    requested_run_id: Option<&str>,
    queue_only: bool,
    backend_selection: Option<BackendSelection>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>> {
    let submitted = lifecycle::submit_plan(&durable_plan, requested_run_id)?;
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
                    &durable_plan,
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
        .run(execution_plan);
    let record = lifecycle::record_run_aggregate(&run_id, &durable_plan, &aggregate)?;
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
    pub prompt_is_literal: bool,
    pub tasks: Vec<String>,
    pub cwd: Option<String>,
    pub workspace: Option<String>,
    pub repo: Option<String>,
    /// Configured execution component when it differs from the owning repo.
    pub component: Option<String>,
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
    let catalog = AgentTaskProviderCatalog::discover();
    controller_resolved_execution_policy_with_sources(
        request,
        &catalog,
        configured_rotation_policy(),
    )
}

/// Resolve the initial provider route using the same default and rotation
/// selection that Cook persists into its dispatch plan.
pub fn resolve_cook_initial_provider_route(
    command: AgentTaskDispatchCommand,
) -> Result<AgentTaskInitialProviderRoute> {
    let catalog = AgentTaskProviderCatalog::discover();
    resolve_cook_initial_provider_route_with_catalog(command, &catalog)
}

/// Resolve Cook's initial route against a caller-supplied catalog. Controller
/// preflight and provider introspection use this to share one exact selection.
pub fn resolve_cook_initial_provider_route_with_catalog(
    command: AgentTaskDispatchCommand,
    catalog: &AgentTaskProviderCatalog,
) -> Result<AgentTaskInitialProviderRoute> {
    let request = resolve_dispatch_request(command)?;
    let policy = request
        .core
        .resolved_provider_policy
        .clone()
        .unwrap_or_else(|| {
            controller_resolved_execution_policy_with_sources(
                &request,
                catalog,
                configured_rotation_policy(),
            )
        });
    let mut route = initial_provider_route_from_policy(policy);
    // Provider configuration is the configured-model source used by Cook when
    // policy and CLI do not select one. Keep discovery on the same default.
    if route.model.is_none() {
        route.model = homeboy_core::defaults::load_config()
            .settings
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string);
    }
    Ok(route)
}

fn configured_rotation_policy() -> Option<AgentTaskProviderRotationPolicy> {
    homeboy_core::defaults::load_config()
        .agent_task
        .rotation
        .and_then(|rotation| serde_json::from_value(rotation).ok())
}

/// Project the first invocation from a resolved provider policy. The dispatch
/// plan builder uses this too, keeping operator introspection aligned with Cook.
pub fn initial_provider_route_from_policy(
    policy: ResolvedAgentTaskProviderPolicy,
) -> AgentTaskInitialProviderRoute {
    let mut backend = policy.backend;
    let mut selector = policy.selector;
    let mut model = policy.model;
    let mut provider_config = Value::Null;
    let mut rotation = policy.rotation;
    if policy.rotation_starts_with_first_entry {
        if let Some(rotation) = rotation.as_mut() {
            if !rotation.entries.is_empty() {
                let first = rotation.entries.remove(0);
                backend = first.backend.unwrap_or(backend);
                selector = first.selector.or(selector);
                model = first.model.or(model);
                provider_config = first.provider_config;
            }
        }
    }
    AgentTaskInitialProviderRoute {
        backend,
        selector,
        model,
        provider_config,
        rotation,
        rotation_selected_initial: policy.rotation_starts_with_first_entry,
    }
}

fn controller_resolved_execution_policy_with_sources(
    request: &AgentTaskDispatchRequest,
    catalog: &AgentTaskProviderCatalog,
    rotation: Option<AgentTaskProviderRotationPolicy>,
) -> ResolvedAgentTaskProviderPolicy {
    let requested_runtime_identity = selected_runtime_identity(request, catalog);
    let explicit_backend = request
        .backend_selection
        .as_ref()
        .is_some_and(|selection| selection.source == BackendSelectionSource::Cli);
    let rotation = rotation.map(|mut rotation| {
        if explicit_backend {
            // A controller pin describes one immutable runtime. Global fallback
            // entries for another runtime must not replace an explicit choice.
            rotation.entries.retain(|entry| {
                entry
                    .backend
                    .as_deref()
                    .is_none_or(|backend| backend == request.backend)
                    && entry.selector.as_deref().is_none_or(|selector| {
                        requested_runtime_identity
                            .as_ref()
                            .is_some_and(|identity| identity.provider_id == selector)
                    })
            });
        }
        rotation
    });
    let rotation_starts_with_first_entry = !explicit_backend && request.model.is_none();
    let runtime_identity = if rotation_starts_with_first_entry {
        if let Some(entry) = rotation
            .as_ref()
            .and_then(|rotation| rotation.entries.first())
        {
            let backend = entry.backend.as_deref().unwrap_or(&request.backend);
            let selector = entry.selector.as_deref().or(request.selector.as_deref());
            selected_runtime_identity_for_route(backend, selector, catalog).or_else(|| {
                (backend == request.backend && selector == request.selector.as_deref())
                    .then(|| requested_runtime_identity.clone())
                    .flatten()
            })
        } else {
            requested_runtime_identity.clone()
        }
    } else {
        requested_runtime_identity
    };
    ResolvedAgentTaskProviderPolicy {
        backend: request.backend.clone(),
        selector: request.selector.clone(),
        model: request.model.clone(),
        retry: AgentTaskRetryPolicy {
            max_attempts: request.core.attempts.unwrap_or(1).max(1),
            ..AgentTaskRetryPolicy::default()
        },
        liveness_timeout_ms: rotation
            .as_ref()
            .and_then(|policy| policy.liveness_timeout_ms),
        rotation,
        // An explicit model is immutable for the initial invocation. Rotation
        // remains available for retries, but cannot silently replace it.
        rotation_starts_with_first_entry,
        runtime_identity,
    }
}

fn selected_runtime_identity(
    request: &AgentTaskDispatchRequest,
    catalog: &AgentTaskProviderCatalog,
) -> Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity> {
    selected_runtime_identity_for_route(&request.backend, request.selector.as_deref(), catalog)
}

pub(crate) fn selected_runtime_identity_for_route(
    backend: &str,
    selector: Option<&str>,
    catalog: &AgentTaskProviderCatalog,
) -> Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity> {
    let ProviderResolution::Resolved(provider) =
        resolve_provider_for_backend(catalog.providers(), backend, selector)
    else {
        return None;
    };
    runtime_identity_for_provider(provider)
}

pub(crate) fn runtime_identity_for_provider(
    provider: &crate::agent_task_provider::AgentTaskExecutorProvider,
) -> Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity> {
    let plan = provider.extra.get("runtime_materialization_plan")?;
    let plan: homeboy_core::agent_runtime_manifest::AgentRuntimeMaterializationPlan =
        serde_json::from_value(plan.clone()).ok()?;
    let source_revision = plan.source_revision.clone()?;
    if !homeboy_core::agent_runtime_manifest::is_immutable_revision(&source_revision) {
        return None;
    }
    let materialization_plan = serde_json::to_value(&plan).ok()?;
    Some(
        homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
            runtime_id: plan.runtime_id,
            provider_id: plan.provider_id,
            source_selector: plan.source_selector,
            source_revision,
            freshness: match plan.freshness {
                homeboy_core::agent_runtime_manifest::AgentRuntimeFreshness::Pinned => {
                    homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Pinned
                }
                homeboy_core::agent_runtime_manifest::AgentRuntimeFreshness::Unverifiable => {
                    homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable
                }
            },
            provider: serde_json::to_value(provider).ok()?,
            materialization_plan,
        },
    )
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

/// Resolve a dispatch request against one observed provider catalog.
///
/// Admission callers use this when the selected route and the remediation
/// choices must come from the same catalog snapshot.
pub fn resolve_dispatch_request_with_default_and_catalog(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    catalog: &AgentTaskProviderCatalog,
) -> Result<AgentTaskDispatchRequest> {
    resolve_dispatch_request_with_sources(command, default_backend, config_default_backend, || {
        catalog.backends()
    })
}

/// The configured Homeboy-config `agent_task.default_backend`, when set. Read via
/// the public `defaults` surface so the dispatch service can name the selection
/// source without touching fleet-owned provider policy code (#5685).
fn config_default_backend() -> Option<String> {
    homeboy_core::defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty())
}

/// The validation error raised when `agent-task cook` was given no `--backend`
/// and no policy layer supplies a default.
///
/// The error enumerates the backends the discovered provider catalog declares
/// so the operator gets a usable value from the failing command itself (#11478).
/// Enumeration is *declaration*, not readiness — a listed backend can still fail
/// its runner/config preflight at dispatch — so the message points at
/// `agent-task providers --validate-readiness` rather than promising usability.
///
/// That hint deliberately omits `--backend`: the unscoped form validates every
/// declared backend and reports each verdict, so the operator is sent to one
/// command that answers "which backend is usable here?" instead of guessing a
/// backend per invocation (#12569).
///
/// A fresh or reset `agent_task` config (`{}`) hits this same path with no
/// discoverable way back to a working default short of reading source or an
/// old backup, so the remediation also names `providers --set-default`, which
/// derives and writes a live-verified `default_backend`/`rotation` in one
/// step (#13634).
fn missing_default_backend_error(available_backends: &[String]) -> Error {
    const PROBLEM: &str =
        "agent-task cook requires --backend because no default backend policy is configured";

    let mut tried = vec![
        "Set agent_task.default_backend in component, extension, or Homeboy config policy, or pass --backend explicitly.".to_string(),
        "Run `homeboy agent-task providers --set-default` to derive and persist a working default_backend (and rotation) from live backend readiness.".to_string(),
    ];

    let problem = if available_backends.is_empty() {
        tried.push(
            "No agent-task executor providers were discovered here; run `homeboy agent-task providers` to diagnose runtime discovery."
                .to_string(),
        );
        PROBLEM.to_string()
    } else {
        tried.push(
            "Listed backends are declared, not verified: run `homeboy agent-task providers --validate-readiness` to see every listed backend's readiness and pick one that is usable."
                .to_string(),
        );
        format!(
            "{PROBLEM}; available backends: {}",
            available_backends.join(", ")
        )
    };

    let mut error = Error::validation_invalid_argument("backend", problem, None, Some(tried));
    // Consumers distinguish an omitted route selection from an executor outage
    // without coupling to the rendered validation message.
    error.details["selection_required"] = Value::Bool(true);
    error
}

/// Resolution core that also takes the Homeboy-config default resolver so tests
/// can drive deterministic source classification (#5685).
fn resolve_dispatch_request_with_default_and_config(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    config_default: impl FnOnce() -> Option<String>,
) -> Result<AgentTaskDispatchRequest> {
    resolve_dispatch_request_with_sources(command, default_backend, config_default, || {
        AgentTaskProviderCatalog::discover().backends()
    })
}

fn resolve_dispatch_request_with_sources(
    command: AgentTaskDispatchCommand,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
    config_default: impl FnOnce() -> Option<String>,
    available_backends: impl FnOnce() -> Vec<String>,
) -> Result<AgentTaskDispatchRequest> {
    let config_default = config_default();
    let submitted_policy = command.core.resolved_provider_policy.clone();
    let (backend, source) = match submitted_policy.as_ref() {
        Some(policy) => (policy.backend.clone(), BackendSelectionSource::Policy),
        None => match command.backend.clone() {
            Some(backend) => (backend, BackendSelectionSource::Cli),
            None => {
                let resolved =
                    default_backend(command.component.as_deref().or(command.repo.as_deref()))?
                        .ok_or_else(|| {
                            // The provider catalog already knows every dispatchable
                            // backend at this point, so discover it rather than making
                            // the operator run `agent-task providers` and parse JSON to
                            // answer a question this command can answer (#11478).
                            // Discovery only happens on the failure path.
                            missing_default_backend_error(&available_backends())
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
        prompt_is_literal: command.prompt_is_literal,
        tasks: command.tasks,
        cwd: command.cwd,
        workspace: command.workspace,
        repo: command.repo,
        component: command.component,
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
pub fn run_dispatch_command(
    command: AgentTaskDispatchCommand,
    executor: SharedAgentTaskExecutor,
) -> Result<(Value, i32)> {
    let catalog = AgentTaskProviderCatalog::discover();
    run_dispatch_command_with_provider_catalog(command, executor, &catalog)
}

pub fn run_dispatch_command_with_provider_catalog(
    command: AgentTaskDispatchCommand,
    executor: SharedAgentTaskExecutor,
    catalog: &AgentTaskProviderCatalog,
) -> Result<(Value, i32)> {
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
    use crate::agent_task::{AgentTaskOutcome, AgentTaskRequest};
    use crate::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskExecutorAdapter};
    use std::sync::Arc;

    /// An executor that must never be reached. A credential gap is a
    /// configuration failure, so it must be reported before a provider
    /// execution is spent against the task's budget (#11479).
    struct NeverRunExecutor;

    impl AgentTaskExecutorAdapter for NeverRunExecutor {
        fn execute(
            &self,
            _request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            panic!("a credential preflight failure must not consume a provider execution");
        }
    }

    #[test]
    fn dispatch_rejects_a_backend_whose_declared_credential_is_missing_without_executing() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(serde_json::json!({
                "id": "claude-code.agent-task-executor",
                "backend": "claude-code",
                "capabilities": ["cli_runtime", "provider_owned_auth"],
                "invocation": { "argv": ["claude-code"] },
                "provider_defaults": {
                    "claude-code": {
                        "secret_env": [required.clone()],
                        "required_secret_env": [required.clone()]
                    }
                }
            }))
            .expect("provider fixture")],
            ..Default::default()
        };
        let request = AgentTaskDispatchRequest {
            prompt: Some("Cook the task.".to_string()),
            prompt_is_literal: false,
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            repo: None,
            component: None,
            task_url: None,
            backend: "claude-code".to_string(),
            selector: None,
            model: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            task_id: None,
            core: DispatchCoreInputs::default(),
            backend_selection: None,
        };

        let error = dispatch_with_provider_catalog(request, Arc::new(NeverRunExecutor), &catalog)
            .expect_err("a missing declared credential must fail dispatch");

        assert_eq!(error.details["field"], "provider_dispatchability");
        assert!(
            error
                .hints
                .iter()
                .any(|hint| hint.message.contains(&required)),
            "the failure must name the credential: {}",
            error.message,
        );
    }

    #[test]
    fn dispatch_does_not_credential_gate_a_provider_that_declares_nothing() {
        let providers = vec![serde_json::from_value(serde_json::json!({
            "id": "local-shell.agent-task-executor",
            "backend": "local-shell",
            "invocation": { "argv": ["local-shell"] }
        }))
        .expect("provider fixture")];

        crate::agent_task_provider::preflight_provider_credentials_for_backend(
            &providers,
            "local-shell",
            None,
        )
        .expect("a provider that declares no credential is dispatchable");
    }

    #[test]
    fn generic_preflight_admits_a_ready_fallback_when_primary_credentials_are_missing() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let primary = serde_json::from_value(serde_json::json!({
            "id": "test.primary",
            "backend": "test",
            "secret_env": [required],
        }))
        .expect("primary provider");
        let fallback = serde_json::from_value(serde_json::json!({
            "id": "test.fallback",
            "backend": "test",
        }))
        .expect("fallback provider");
        let catalog = AgentTaskProviderCatalog {
            providers: vec![primary, fallback],
            ..Default::default()
        };
        let request = AgentTaskDispatchRequest {
            prompt: Some("run".to_string()),
            prompt_is_literal: false,
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            repo: None,
            component: None,
            task_url: None,
            backend: "test".to_string(),
            selector: Some("test.primary".to_string()),
            model: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            task_id: None,
            core: DispatchCoreInputs {
                resolved_provider_policy: Some(ResolvedAgentTaskProviderPolicy {
                    backend: "test".to_string(),
                    selector: Some("test.primary".to_string()),
                    model: None,
                    rotation: Some(AgentTaskProviderRotationPolicy {
                        entries: vec![
                            crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                selector: Some("test.fallback".to_string()),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    }),
                    rotation_starts_with_first_entry: false,
                    retry: Default::default(),
                    liveness_timeout_ms: None,
                    runtime_identity: None,
                }),
                ..Default::default()
            },
            backend_selection: None,
        };

        preflight_dispatch_provider_admission(&request, &catalog)
            .expect("the generic path admits the reachable fallback");
    }

    fn command_with_backend(backend: Option<&str>) -> AgentTaskDispatchCommand {
        AgentTaskDispatchCommand {
            prompt: Some("task".to_string()),
            backend: backend.map(str::to_string),
            ..AgentTaskDispatchCommand::default()
        }
    }

    #[test]
    fn missing_default_backend_error_enumerates_available_backends() {
        let error = missing_default_backend_error(&[
            "claude-code".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ]);

        assert!(
            error
                .message
                .contains("requires --backend because no default backend policy is configured"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("available backends: claude-code, codex, opencode"),
            "the failing command must name the values it will accept: {}",
            error.message
        );
        // The configuration fix stays first; readiness is a caveat, not a promise.
        assert_eq!(
            error.details["tried"][0].as_str(),
            Some("Set agent_task.default_backend in component, extension, or Homeboy config policy, or pass --backend explicitly.")
        );
        // A lost or empty `agent_task` config must recover without archaeology:
        // point at the command that derives and writes a working default from
        // live readiness (#13634).
        assert!(error.details["tried"][1]
            .as_str()
            .expect("set-default remediation")
            .contains("--set-default"));
        assert!(error.details["tried"][2]
            .as_str()
            .expect("readiness caveat")
            .contains("--validate-readiness"));
        assert_eq!(error.details["selection_required"], true);
    }

    #[test]
    fn missing_default_backend_error_without_providers_points_at_discovery() {
        let error = missing_default_backend_error(&[]);

        assert!(
            !error.message.contains("available backends"),
            "an empty catalog must not advertise an empty list: {}",
            error.message
        );
        assert!(error.details["tried"][2]
            .as_str()
            .expect("discovery hint")
            .contains("homeboy agent-task providers"));
    }

    #[test]
    fn missing_default_backend_is_raised_when_no_policy_resolves_a_backend() {
        let error = resolve_dispatch_request_with_sources(
            command_with_backend(None),
            |_| Ok(None),
            || None,
            Vec::new,
        )
        .expect_err("no backend and no default policy");

        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(error.details["field"].as_str(), Some("backend"));
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
    fn component_policy_resolution_does_not_replace_repository_identity() {
        let mut command = command_with_backend(None);
        command.repo = Some("blocks-engine".to_string());
        command.component = Some("php-transformer".to_string());

        let request = resolve_dispatch_request_with_default_and_config(
            command,
            |component| {
                assert_eq!(component, Some("php-transformer"));
                Ok(Some("opencode".to_string()))
            },
            || None,
        )
        .expect("component-scoped request");

        assert_eq!(request.repo.as_deref(), Some("blocks-engine"));
        assert_eq!(request.component.as_deref(), Some("php-transformer"));
        assert_eq!(request.backend, "opencode");
    }

    #[test]
    fn configured_default_projects_the_same_initial_cook_route() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(None),
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("configured default request");
        let route =
            initial_provider_route_from_policy(controller_resolved_execution_policy_with_sources(
                &request,
                &AgentTaskProviderCatalog::default(),
                None,
            ));

        assert_eq!(route.backend, "opencode");
        assert!(route.selector.is_none());
        assert!(route.model.is_none());
        assert!(route.rotation_selected_initial);
    }

    #[test]
    fn unavailable_default_with_rotation_projects_the_first_rotation_route() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(None),
            |_| Ok(Some("unavailable-default".to_string())),
            || Some("unavailable-default".to_string()),
        )
        .expect("configured default request");
        let route =
            initial_provider_route_from_policy(controller_resolved_execution_policy_with_sources(
                &request,
                &AgentTaskProviderCatalog::default(),
                Some(AgentTaskProviderRotationPolicy {
                    entries: vec![
                        crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            backend: Some("available-rotation".to_string()),
                            selector: Some("rotation-provider".to_string()),
                            model: Some("rotation-model".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
            ));

        assert_eq!(route.backend, "available-rotation");
        assert_eq!(route.selector.as_deref(), Some("rotation-provider"));
        assert_eq!(route.model.as_deref(), Some("rotation-model"));
        assert!(route.rotation_selected_initial);
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
        let mut provider: crate::agent_task_provider::AgentTaskExecutorProvider =
            serde_json::from_value(serde_json::json!({
                "id": "codex.agent-task-executor",
                "backend": "codex",
                "invocation": { "argv": ["provider"] }
            }))
            .expect("provider fixture");
        provider.extra.insert(
            "runtime_materialization_plan".to_string(),
            serde_json::json!({
                "schema": homeboy_core::agent_runtime_manifest::AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA,
                "runtime_id": "codex",
                "provider_id": "codex.agent-task-executor",
                "source_selector": "codex-runtime",
                "source_revision": "0123456789abcdef0123456789abcdef01234567",
                "freshness": "pinned"
            }),
        );
        let catalog = AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        };
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("codex")),
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("explicit Codex request");
        let policy = controller_resolved_execution_policy_with_sources(
            &request,
            &catalog,
            Some(AgentTaskProviderRotationPolicy {
                entries: vec![
                    crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                        backend: Some("opencode".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
        );

        assert_eq!(policy.backend, "codex");
        assert!(!policy.rotation_starts_with_first_entry);
        assert!(policy.rotation.expect("rotation").entries.is_empty());
        let identity = policy.runtime_identity.expect("Codex runtime identity");
        assert_eq!(identity.runtime_id, "codex");
        assert_eq!(identity.provider_id, "codex.agent-task-executor");
        assert_eq!(identity.provider["backend"], "codex");
    }

    #[test]
    fn explicit_model_prevents_default_rotation_from_replacing_the_initial_invocation() {
        let request = resolve_dispatch_request_with_default_and_config(
            AgentTaskDispatchCommand {
                prompt: Some("Cook with Sol.".to_string()),
                model: Some("openai/gpt-5.6-sol".to_string()),
                ..AgentTaskDispatchCommand::default()
            },
            |_| Ok(Some("opencode".to_string())),
            || Some("opencode".to_string()),
        )
        .expect("request");
        let policy = controller_resolved_execution_policy_with_sources(
            &request,
            &AgentTaskProviderCatalog::default(),
            None,
        );

        assert_eq!(policy.model.as_deref(), Some("openai/gpt-5.6-sol"));
        assert!(!policy.rotation_starts_with_first_entry);
    }
}
