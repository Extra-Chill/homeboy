//! Core service for durable agent-task dispatch.
//!
//! The CLI adapter owns clap parsing and JSON rendering. This service owns the
//! typed dispatch request, plan construction, and provider preflight. Durable
//! submission, running transitions, and scheduler invocation belong to
//! [`crate::agent_task_submission_service`].

use serde::Serialize;
use serde_json::Value;

use crate::agent_task_builtin_backend::BUILTIN_DEFAULT_BACKEND;
use crate::agent_task_dispatch_plan::{
    build_dispatch_plan, build_dispatch_plan_with_provider_requirements,
    preflight_dispatch_provider_secrets,
};
use crate::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
use crate::agent_task_provider::{
    default_backend_for_component, resolve_provider_for_backend, AgentTaskProviderCatalog,
    ProviderResolution,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskPlan, AgentTaskProviderRotationPolicy, AgentTaskRetryPolicy,
    SharedAgentTaskExecutor,
};
use crate::agent_task_service::AgentTaskRunResult;
use crate::agent_task_submission_service::{
    prepared_submission_request, submit_prepared_plan, PreparedAgentTaskSubmission,
};
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
    /// No policy names a backend, so Cook selected its built-in coding backend
    /// (Pi) because the provider catalog declares it (#15932).
    Builtin,
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
    /// Internal provenance for Homeboy-generated batch fanout context. This is
    /// retained in task metadata and is never included in provider config.
    pub generated_fanout_context: bool,
    /// Total provider executions per task, including the first attempt.
    /// `None` means the caller did not ask for a value, so the configured
    /// provider rotation gets to fund its own reachability (#11082).
    pub attempts: Option<u32>,
    /// Explicit same-provider retry budget after the initial execution.
    /// `None` means unspecified, which resolves to zero: same-provider retries
    /// fund Cook gate and review-form remediation and are never derived from a
    /// rotation chain.
    pub same_provider_retries: Option<u32>,
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
    /// Explicit operator acknowledgement that a requested model may displace
    /// configured model routes.
    pub acknowledge_model_override: bool,
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
    let mut readiness_cache = crate::agent_task_provider::ProviderRuntimeReadinessCache::default();
    require_model_override_acknowledgement(&request)?;
    let backend_selection = request.backend_selection.clone();
    let plan = build_dispatch_plan_with_provider_requirements(&request, |backend, selector| {
        catalog.provider_requires_cwd_git_checkout(backend, selector)
    })?;
    let execution_plan =
        match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
            &plan,
            catalog,
            &mut readiness_cache,
        ) {
            Ok(plan) => {
                validate_selected_execution_plan(&plan, catalog)?;
                plan
            }
            Err(error) if error.retryable == Some(true) => {
                return Err(record_retryable_dispatch_admission_failure(
                    &plan,
                    request.run_id.as_deref(),
                    error,
                ));
            }
            Err(error) => {
                preflight_dispatch_provider_secrets(&plan)?;
                return Err(error);
            }
        };
    // The admitted plan retains the complete route chain while binding execution
    // to the selected route and its forward-only rotation cursor.
    run_dispatch_plan(
        execution_plan,
        request.run_id.as_deref(),
        request.core.queue_only,
        backend_selection,
        executor,
    )
}

/// An explicit model is a spend-affecting selection when configured model
/// routes exist. Keep this check before provider admission so an unacknowledged
/// override cannot probe or invoke a paid provider.
pub fn require_model_override_acknowledgement(request: &AgentTaskDispatchRequest) -> Result<()> {
    let Some(selected_model) = request.model.as_deref() else {
        return Ok(());
    };
    if selected_model.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "model",
            "an explicit model override must be non-empty",
            None,
            None,
        ));
    }
    let mut configured_models = std::collections::BTreeSet::new();
    let configured_primary_model =
        if let Some(policy) = request.core.resolved_provider_policy.as_ref() {
            policy
                .rotation
                .as_ref()
                .and_then(|rotation| rotation.entries.first())
                .and_then(|entry| entry.model.as_deref())
                .or(policy.model.as_deref())
                .filter(|model| !model.is_empty())
                .map(str::to_string)
        } else {
            let configured_default_model = homeboy_core::defaults::load_config()
                .settings
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .map(str::to_string);
            configured_rotation_policy()
                .as_ref()
                .and_then(|rotation| rotation.entries.first())
                .and_then(|entry| entry.model.as_deref())
                .map(str::to_string)
                .or(configured_default_model)
                .filter(|model| !model.is_empty())
        };
    if let Some(policy) = request.core.resolved_provider_policy.as_ref() {
        if let Some(model) = policy
            .model
            .as_deref()
            .filter(|model| !model.is_empty() && *model != selected_model)
        {
            configured_models.insert(model.to_string());
        }
        if let Some(rotation) = policy.rotation.as_ref() {
            configured_models.extend(
                rotation
                    .entries
                    .iter()
                    .filter_map(|entry| entry.model.as_deref())
                    .filter(|model| !model.is_empty())
                    .map(str::to_string),
            );
        }
    } else {
        if let Some(model) = homeboy_core::defaults::load_config()
            .settings
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
        {
            configured_models.insert(model.to_string());
        }
        if let Some(rotation) = configured_rotation_policy() {
            configured_models.extend(
                rotation
                    .entries
                    .iter()
                    .filter_map(|entry| entry.model.as_deref())
                    .filter(|model| !model.is_empty())
                    .map(str::to_string),
            );
        }
    }
    let selected_policy_model_without_routes = request
        .core
        .resolved_provider_policy
        .as_ref()
        .is_some_and(|policy| policy.model.as_deref() == Some(selected_model));
    if configured_models.is_empty() && !selected_policy_model_without_routes {
        return Ok(());
    }
    if request.core.acknowledge_model_override {
        return Ok(());
    }
    let configured_models = configured_models.into_iter().collect::<Vec<_>>();
    let mut error = Error::validation_invalid_argument(
        "model",
        format!(
            "explicit model `{selected_model}` pins the provider route; explicit acknowledgement is required before displacing configured rotation"
        ),
        Some(selected_model.to_string()),
        Some(vec![format!(
            "Replay with --acknowledge-model-override to authorize `{selected_model}` over configured model route(s): {}",
            configured_models.join(", ")
        )]),
    );
    error.details["schema"] =
        Value::String("homeboy/agent-task-model-override-confirmation-required/v1".to_string());
    error.details["confirmation_required"] = Value::Bool(true);
    error.details["selected_model"] = Value::String(selected_model.to_string());
    error.details["configured_primary_model"] = configured_primary_model
        .map(Value::String)
        .unwrap_or(Value::Null);
    error.details["configured_models"] =
        serde_json::to_value(configured_models).expect("model route list serializes");
    error.details["acknowledgement_flag"] =
        Value::String("--acknowledge-model-override".to_string());
    Err(error)
}

/// Validate the reachable provider routes needed to dispatch this request.
pub fn preflight_dispatch_provider_admission(
    request: &AgentTaskDispatchRequest,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    let mut readiness_cache = crate::agent_task_provider::ProviderRuntimeReadinessCache::default();
    require_model_override_acknowledgement(request)?;
    let mut plan = build_dispatch_plan_with_provider_requirements(request, |backend, selector| {
        catalog.provider_requires_cwd_git_checkout(backend, selector)
    })?;
    let plan = match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
        &plan,
        catalog,
        &mut readiness_cache,
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
            catalog.apply_provider_runner_secret_env_contracts(&mut plan);
            catalog.validate_selected_models(&plan)?;
            preflight_dispatch_provider_secrets(&plan)?;
            crate::agent_task_provider::preflight_plan_provider_dispatchability_without_runtime_with_providers(
            &plan,
            catalog,
            &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
        )?;
            return Err(error);
        }
    };
    // `plan` is the admission-derived selected route (fixture/gate bypass
    // already applied). Re-deriving dispatchability from scratch here would
    // drop that bypass and hard-fail a route admission just approved (#13974).
    catalog.validate_selected_models(&plan)?;
    preflight_dispatch_provider_secrets(&plan)
}

fn validate_selected_execution_plan(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    // `plan` already cleared full dispatchability (including live runtime
    // probing) for its selected route during admission, with the fixture/gate
    // bypass that route selection depends on. Re-deriving dispatchability here
    // from scratch would re-run that evaluation without the bypass and hard-fail
    // a plan admission just approved (#13974). Only run the checks admission
    // does not already cover: declared model support, provider-declared runtime
    // preflight checks and task secret material.
    catalog.validate_selected_models(plan)?;
    catalog.enforce_runtime_preflight_checks_for_plan(plan)?;
    preflight_dispatch_provider_secrets(plan)
}

/// A retryable admission denial is still a denied execution. Materialize the
/// submitted plan and terminal pre-execution diagnostic rather than letting a
/// scheduler reservation choose a fresh route from the original plan.
fn record_retryable_dispatch_admission_failure(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    mut error: Error,
) -> Error {
    let result = (|| {
        let request = crate::agent_task_submission_service::prepared_submission_request(
            requested_run_id,
            false,
            "homeboy-typed-dispatch",
        )?;
        let submitted = crate::agent_task_submission_service::reject_prepared_plan(
            &request,
            crate::agent_task_submission_service::PreparedAgentTaskSubmission::new(plan.clone()),
            "admit_plan_provider_dispatchability",
            &error,
        )?;
        error.details["run_id"] = serde_json::json!(submitted.run_id);
        Ok(())
    })();
    if let Err(record_error) = result {
        return record_error;
    }
    error
}

fn run_dispatch_plan(
    execution_plan: AgentTaskPlan,
    requested_run_id: Option<&str>,
    queue_only: bool,
    backend_selection: Option<BackendSelection>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskDispatchReport>> {
    let outcome = submit_prepared_plan(
        &prepared_submission_request(requested_run_id, queue_only, "homeboy-dispatch")?,
        PreparedAgentTaskSubmission::new(execution_plan),
        executor,
    )?;
    let record = if queue_only {
        outcome.submitted
    } else {
        outcome.record
    };
    Ok(AgentTaskRunResult {
        value: dispatch_report(
            record,
            outcome.aggregate,
            outcome.acknowledgement.queued,
            backend_selection,
        ),
        exit_code: outcome.exit_code,
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
        state: record.state(),
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

impl AgentTaskDispatchCommand {
    /// The registered component this command names, if any.
    pub fn declared_component(&self) -> Option<homeboy_core::component::Component> {
        let id = self.component.as_deref().or(self.repo.as_deref())?;
        homeboy_core::component::load(id).ok()
    }
}

/// Resolve a typed dispatch request from command-surface inputs, applying the
/// declared default-backend policy from the caller-resolved component when
/// `--backend` is absent. `None` uses extension and global policy only.
pub fn resolve_dispatch_request(
    command: AgentTaskDispatchCommand,
    component: Option<&homeboy_core::component::Component>,
) -> Result<AgentTaskDispatchRequest> {
    resolve_dispatch_request_with_default(command, |_| default_backend_for_component(component))
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

/// Resolve Cook's initial route against a caller-supplied catalog. Controller
/// preflight and provider introspection use this to share one exact selection.
pub fn resolve_cook_initial_provider_route_with_catalog(
    command: AgentTaskDispatchCommand,
    catalog: &AgentTaskProviderCatalog,
    component: Option<&homeboy_core::component::Component>,
) -> Result<AgentTaskInitialProviderRoute> {
    let request = resolve_dispatch_request(command, component)?;
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

/// The configured global rotation policy (`agent_task.rotation`), as the
/// dispatch path reads it. Shared readers (for example the `agent-task
/// capacity` rotation walk) must use this rather than re-parsing the config
/// value, so every consumer agrees on the policy shape.
pub fn configured_rotation_policy() -> Option<AgentTaskProviderRotationPolicy> {
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
                        resolve_provider_for_backend(
                            catalog.providers(),
                            &request.backend,
                            request.selector.as_deref(),
                        )
                        .resolved()
                        .is_some_and(|provider| provider.id == selector)
                    })
            });
        }
        rotation
    });
    // An explicit backend narrows the rotation to that backend's entries; it
    // does not discard their model. Without `--model`, the first narrowed
    // entry supplies it, so Cook never dispatches a model-less runtime that
    // silently picks its own default (#16195).
    let rotation_starts_with_first_entry = request.model.is_none()
        && (!explicit_backend
            || rotation
                .as_ref()
                .is_some_and(|rotation| !rotation.entries.is_empty()));
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
        runtime_identity: None,
    }
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
    resolve_dispatch_request_with_sources(command, default_backend, config_default_backend, || {
        AgentTaskProviderCatalog::discover().backends()
    })
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

    tried.push(format!(
        "Install the built-in `{BUILTIN_DEFAULT_BACKEND}` coding backend: Cook selects it automatically when no policy names a backend."
    ));

    let mut error = Error::validation_invalid_argument("backend", problem, None, Some(tried));
    // Consumers distinguish an omitted route selection from an executor outage
    // without coupling to the rendered validation message.
    error.details["selection_required"] = Value::Bool(true);
    error
}

/// Inject configuration defaults for deterministic source-classification tests.
#[cfg(test)]
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
    if command
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "model",
            "an explicit model override must be non-empty",
            None,
            None,
        ));
    }
    let submitted_policy = command.core.resolved_provider_policy.clone();
    let (backend, source) = match submitted_policy.as_ref() {
        Some(policy) => (policy.backend.clone(), BackendSelectionSource::Policy),
        None => match command.backend.clone() {
            Some(backend) => (backend, BackendSelectionSource::Cli),
            None => {
                match default_backend(command.component.as_deref().or(command.repo.as_deref()))? {
                    Some(resolved) => {
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
                    None => {
                        // The provider catalog already knows every dispatchable
                        // backend at this point, so discover it rather than making
                        // the operator run `agent-task providers` and parse JSON to
                        // answer a question this command can answer (#11478).
                        // Discovery only happens when no policy names a backend.
                        let available = available_backends();
                        if available
                            .iter()
                            .any(|backend| backend == BUILTIN_DEFAULT_BACKEND)
                        {
                            // Pi is the built-in coding backend (#15932).
                            (
                                BUILTIN_DEFAULT_BACKEND.to_string(),
                                BackendSelectionSource::Builtin,
                            )
                        } else {
                            return Err(missing_default_backend_error(&available));
                        }
                    }
                }
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
        // The policy owns the resolved runtime model. This field records only
        // an explicit caller choice, so inherited models cannot become pins.
        model: command.model,
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
    let component = command.declared_component();
    let request = resolve_dispatch_request(command, component.as_ref())?;
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

    fn model_override_request(model: &str, acknowledged: bool) -> AgentTaskDispatchRequest {
        AgentTaskDispatchRequest {
            prompt: Some("run".to_string()),
            prompt_is_literal: true,
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            repo: None,
            component: None,
            task_url: None,
            backend: "test".to_string(),
            selector: None,
            model: Some(model.to_string()),
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            task_id: None,
            core: DispatchCoreInputs {
                acknowledge_model_override: acknowledged,
                resolved_provider_policy: Some(ResolvedAgentTaskProviderPolicy {
                    backend: "test".to_string(),
                    selector: None,
                    model: None,
                    rotation: Some(AgentTaskProviderRotationPolicy {
                        entries: vec![
                            crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                model: Some("configured-model".to_string()),
                                ..Default::default()
                            },
                            crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                model: Some("fallback-model".to_string()),
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
        }
    }

    #[test]
    fn explicit_model_override_requires_acknowledgement_without_running_provider() {
        let error = require_model_override_acknowledgement(&model_override_request(
            "operator-model",
            false,
        ))
        .expect_err("an unacknowledged model override must be refused");

        assert_eq!(
            error.details["schema"],
            "homeboy/agent-task-model-override-confirmation-required/v1"
        );
        assert_eq!(error.details["confirmation_required"], true);
        assert_eq!(error.details["selected_model"], "operator-model");
        assert_eq!(error.details["configured_models"][0], "configured-model");
    }

    #[test]
    fn explicit_models_require_confirmation_even_when_configured() {
        require_model_override_acknowledgement(&model_override_request("operator-model", true))
            .expect("explicit acknowledgement admits the override");
        require_model_override_acknowledgement(&model_override_request("configured-model", false))
            .expect_err("a configured-model pin still disables fallback");
    }

    #[test]
    fn configured_alternative_model_pin_requires_confirmation() {
        require_model_override_acknowledgement(&model_override_request("fallback-model", false))
            .expect_err("selecting an alternative must not silently disable fallback");
    }

    #[test]
    fn inherited_policy_model_and_acknowledgement_alone_do_not_pin_rotation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut core = model_override_request("configured-model", false).core;
            core.resolved_provider_policy.as_mut().unwrap().model =
                Some("configured-model".to_string());
            core.acknowledge_model_override = true;
            let mut request = resolve_dispatch_request(
                AgentTaskDispatchCommand {
                    prompt: Some("use the configured policy".to_string()),
                    core,
                    ..Default::default()
                },
                None,
            )
            .expect("resolve inherited model");
            assert!(
                request.model.is_none(),
                "the caller did not supply a model override"
            );
            require_model_override_acknowledgement(&request).expect("no model pin to confirm");
            let plan =
                build_controller_dispatch_plan(&mut request).expect("compile configured rotation");
            assert_eq!(
                plan.tasks[0].executor.model.as_deref(),
                Some("configured-model")
            );
            assert!(plan.options.execution_budget.max_provider_rotations > 0);
            assert!(plan.metadata["model_override_confirmation"].is_null());
        });
    }

    #[test]
    fn empty_model_cannot_authorize_a_pin() {
        for model in ["", " "] {
            let error =
                require_model_override_acknowledgement(&model_override_request(model, true))
                    .expect_err("a confirmed pin still needs a concrete model");
            assert_eq!(error.details["field"], "model");
        }
    }

    #[test]
    fn configured_rotation_executes_fallback_and_only_a_confirmed_model_can_pin_it() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let capture = home.path().join("provider-models.jsonl");
            let fake = crate::agent_task_provider::test_pi::FakePi::with_probes(
                &format!(
                    "while [ \"$1\" != --model ]; do shift; done; model=$2; printf '\"%s\"\\n' \"$model\" >> '{}'; if [ \"$model\" = test/model ]; then printf '%s\\n' '{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"stopReason\":\"error\",\"errorMessage\":\"primary unavailable\"}}}}'; exit 0; fi; {}",
                    capture.display(),
                    crate::agent_task_provider::test_pi::FakePi::answer("fallback completed")
                ),
                "printf 'test-pi\\n'",
                "printf 'test model\\ntest fallback\\n'",
            );
            let catalog = AgentTaskProviderCatalog {
                providers: vec![fake.provider()],
                ..Default::default()
            };

            let mut request = model_override_request("configured-model", false);
            request.backend = "pi".to_string();
            request.core.provider_config = Some(fake.config().to_string());
            request.model = None;
            request.core.timeout_ms = Some(10_000);
            let policy = request.core.resolved_provider_policy.as_mut().unwrap();
            policy.backend = "pi".to_string();
            policy.rotation_starts_with_first_entry = true;
            for (entry, model) in policy
                .rotation
                .as_mut()
                .unwrap()
                .entries
                .iter_mut()
                .zip(["test/model", "test/fallback"])
            {
                entry.backend = Some("pi".to_string());
                entry.model = Some(model.to_string());
            }
            // This test owns routing, not transcript harvesting. Preserve the
            // dispatch artifact contract with a transcript of the fake's result.
            struct WithTranscript(crate::agent_task_provider::ExtensionProviderAgentTaskExecutor);
            impl AgentTaskExecutorAdapter for WithTranscript {
                fn provider_route_capacity_key(&self, request: &AgentTaskRequest) -> String {
                    self.0.provider_route_capacity_key(request)
                }
                fn provider_route_readiness(
                    &self,
                    request: &AgentTaskRequest,
                ) -> crate::agent_task_scheduler::ProviderRouteReadiness {
                    self.0.provider_route_readiness(request)
                }
                fn execute(
                    &self,
                    request: AgentTaskRequest,
                    context: AgentTaskExecutionContext,
                ) -> AgentTaskOutcome {
                    let mut outcome = self.0.execute(request, context);
                    if outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Succeeded {
                        outcome.status = crate::agent_task::AgentTaskOutcomeStatus::NoOp;
                    }
                    outcome.typed_artifacts.push(
                        serde_json::from_value(serde_json::json!({
                            "name": "transcript", "payload": outcome.summary
                        }))
                        .unwrap(),
                    );
                    outcome
                }
            }
            let executor = || {
                Arc::new(WithTranscript(
                    crate::agent_task_provider::ExtensionProviderAgentTaskExecutor::from_catalog(
                        catalog.clone(),
                    ),
                ))
            };
            let result = dispatch_with_provider_catalog(request.clone(), executor(), &catalog)
                .expect("execute configured rotation");
            assert_eq!(result.exit_code, 0, "{:?}", result.value.aggregate);
            assert!(result.value.record.state().is_terminal());
            let attempted = || {
                std::fs::read_to_string(&capture)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<String>(line).unwrap())
                    .collect::<Vec<_>>()
            };
            assert_eq!(attempted(), ["test/model", "test/fallback"]);

            let mut capped = request.clone();
            capped.core.attempts = Some(1);
            let error = dispatch_with_provider_catalog(capped, executor(), &catalog)
                .expect_err("a cap cannot suppress configured fallback");
            assert_eq!(error.details["field"], "max-provider-executions");
            assert_eq!(
                attempted().len(),
                2,
                "invalid budget starts no provider process"
            );

            request.model = Some("test/model".to_string());
            let policy = request.core.resolved_provider_policy.as_mut().unwrap();
            policy.model = request.model.clone();
            policy.rotation_starts_with_first_entry = false;
            let error = dispatch_with_provider_catalog(request.clone(), executor(), &catalog)
                .expect_err("a model pin needs confirmation");
            assert_eq!(error.details["confirmation_required"], true);
            assert_eq!(
                attempted().len(),
                2,
                "unconfirmed pin starts no provider process"
            );

            request.core.acknowledge_model_override = true;
            request.core.attempts = Some(1);
            let pinned = dispatch_with_provider_catalog(request, executor(), &catalog)
                .expect("execute confirmed model pin");
            assert_ne!(
                pinned.exit_code, 0,
                "the unavailable pinned model cannot use fallback"
            );
            assert_eq!(attempted(), ["test/model", "test/fallback", "test/model"]);
            let plan = crate::agent_task_lifecycle::load_controller_plan(&pinned.value.run_id)
                .expect("read persisted pin");
            assert_eq!(
                plan.metadata["model_override_confirmation"]["acknowledged"],
                true
            );
            assert_eq!(plan.options.execution_budget.max_provider_rotations, 0);
        });
    }

    #[test]
    fn policy_model_does_not_self_authorize_an_unconfigured_override() {
        let mut request = model_override_request("operator-model", false);
        request
            .core
            .resolved_provider_policy
            .as_mut()
            .expect("resolved policy")
            .model = Some("operator-model".to_string());

        let error = require_model_override_acknowledgement(&request)
            .expect_err("a changed policy model cannot self-authorize");
        assert_eq!(error.details["confirmation_required"], true);
    }

    #[test]
    fn explicit_policy_model_without_configured_routes_requires_acknowledgement() {
        let mut request = model_override_request("operator-model", false);
        let policy = request
            .core
            .resolved_provider_policy
            .as_mut()
            .expect("resolved policy");
        policy.model = Some("operator-model".to_string());
        policy.rotation = None;

        let error = require_model_override_acknowledgement(&request)
            .expect_err("a selected policy model must not authorize itself");

        assert_eq!(
            error.details["schema"],
            "homeboy/agent-task-model-override-confirmation-required/v1"
        );
        assert_eq!(error.details["configured_models"], serde_json::json!([]));
    }

    #[test]
    fn dispatch_refuses_override_before_fixture_executor_is_reached() {
        let mut request = model_override_request("operator-model", false);
        request.backend = "fixture".to_string();
        request
            .core
            .resolved_provider_policy
            .as_mut()
            .expect("resolved policy")
            .backend = "fixture".to_string();
        let error = dispatch_with_provider_catalog(
            request,
            Arc::new(NeverRunExecutor),
            &AgentTaskProviderCatalog::default(),
        )
        .expect_err("unacknowledged override must stop before execution");

        assert_eq!(error.details["confirmation_required"], true);
    }

    fn configure_fake_pi(fake: &crate::agent_task_provider::test_pi::FakePi) {
        let mut config = homeboy_core::defaults::load_config();
        config
            .settings
            .extend(fake.config().as_object().unwrap().clone());
        homeboy_core::defaults::save_config(&config).unwrap();
    }

    #[test]
    fn generic_preflight_admits_a_ready_fallback_when_primary_credentials_are_missing() {
        // Preflight materializes provider refs under the home. The ambient home
        // is whichever one a parallel test isolated last, and it vanishes when
        // that test exits (#14832).
        let _home = homeboy_core::test_support::HomeGuard::new();
        let fake = crate::agent_task_provider::test_pi::FakePi::with_probes(
            "exit 99",
            "printf 'test-pi\\n'",
            "printf 'test fallback\\n'",
        );
        configure_fake_pi(&fake);
        let catalog = AgentTaskProviderCatalog {
            providers: vec![fake.provider()],
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
            backend: "pi".to_string(),
            selector: Some("pi.agent-task-executor".to_string()),
            model: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            task_id: None,
            core: DispatchCoreInputs {
                resolved_provider_policy: Some(ResolvedAgentTaskProviderPolicy {
                    backend: "pi".to_string(),
                    selector: Some("pi.agent-task-executor".to_string()),
                    model: Some("test/model".to_string()),
                    rotation: Some(AgentTaskProviderRotationPolicy {
                        entries: vec![
                            crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                model: Some("test/fallback".to_string()),
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

        // The same unready primary, pinned by an acknowledged explicit model,
        // fails admission instead of binding the configured fallback
        // (#15837, #15409, #15018).
        let mut pinned = request;
        pinned.model = Some("test/model".to_string());
        pinned.core.acknowledge_model_override = true;
        let error = preflight_dispatch_provider_admission(&pinned, &catalog)
            .expect_err("a pinned model is never substituted by a rotation entry");
        assert!(
            format!("{} {:?} {}", error.message, error.hints, error.details).contains("test/model"),
            "the failure names the unavailable pinned model: {} {}",
            error.message,
            error.details
        );
    }

    #[test]
    fn queued_dispatch_persists_the_admitted_fallback_binding() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let fake = crate::agent_task_provider::test_pi::FakePi::with_probes(
                "exit 99",
                "printf 'test-pi\\n'",
                "printf 'test fallback\\n'",
            );
            let catalog = AgentTaskProviderCatalog {
                providers: vec![fake.provider()],
                ..Default::default()
            };
            configure_fake_pi(&fake);
            let run_id = format!("queued-fallback-{}", uuid::Uuid::new_v4());
            let request = AgentTaskDispatchRequest {
                prompt: Some("run".to_string()),
                prompt_is_literal: false,
                tasks: Vec::new(),
                cwd: None,
                workspace: None,
                repo: None,
                component: None,
                task_url: None,
                backend: "pi".to_string(),
                selector: Some("pi.agent-task-executor".to_string()),
                model: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                concurrency: 1,
                run_id: Some(run_id.clone()),
                task_id: None,
                core: DispatchCoreInputs {
                    queue_only: true,
                    resolved_provider_policy: Some(ResolvedAgentTaskProviderPolicy {
                        backend: "pi".to_string(),
                        selector: Some("pi.agent-task-executor".to_string()),
                        model: Some("test/model".to_string()),
                        rotation: Some(AgentTaskProviderRotationPolicy {
                            entries: vec![
                                crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                    model: Some("test/fallback".to_string()),
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

            let result =
                dispatch_with_provider_catalog(request, Arc::new(NeverRunExecutor), &catalog)
                    .expect("queue admitted fallback");
            assert!(result.value.queued);
            let plan =
                crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .expect("lifecycle store")
                    .read_controller_plan(&run_id)
                    .expect("queued controller plan");

            assert_eq!(plan.tasks[0].executor.model(), Some("test/fallback"));
            assert_eq!(
                plan.tasks[0].metadata["provider_readiness_routing"]["next_rotation_index"],
                1
            );
            assert_eq!(plan.options.rotation.as_ref().unwrap().entries.len(), 1);
        });
    }

    #[test]
    fn retryable_direct_admission_denial_never_runs_or_rederives_a_route() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let fake = crate::agent_task_provider::test_pi::FakePi::with_probes(
                "exit 99",
                "printf 'test-pi\\n'",
                "exit 1",
            );
            let catalog = AgentTaskProviderCatalog {
                providers: vec![fake.provider()],
                ..Default::default()
            };
            let run_id = format!("retryable-admission-{}", uuid::Uuid::new_v4());
            let mut request = AgentTaskDispatchRequest {
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
                model: Some("primary".to_string()),
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                concurrency: 1,
                run_id: Some(run_id.clone()),
                task_id: None,
                core: DispatchCoreInputs {
                    acknowledge_model_override: true,
                    resolved_provider_policy: Some(ResolvedAgentTaskProviderPolicy {
                        backend: "test".to_string(),
                        selector: Some("test.primary".to_string()),
                        model: Some("primary".to_string()),
                        rotation: Some(AgentTaskProviderRotationPolicy {
                            entries: vec![
                                crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                    selector: Some("test.fallback".to_string()),
                                    model: Some("fallback".to_string()),
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

            request.backend = "pi".to_string();
            request.selector = Some("pi.agent-task-executor".to_string());
            request.model = Some("test/model".to_string());
            request.core.provider_config = Some(fake.config().to_string());
            let policy = request.core.resolved_provider_policy.as_mut().unwrap();
            policy.backend = request.backend.clone();
            policy.selector = request.selector.clone();
            policy.model = request.model.clone();
            policy.rotation.as_mut().unwrap().entries[0].selector = request.selector.clone();
            policy.rotation.as_mut().unwrap().entries[0].model = Some("test/fallback".to_string());
            let error =
                dispatch_with_provider_catalog(request, Arc::new(NeverRunExecutor), &catalog)
                    .expect_err("temporary route unavailability denies direct dispatch");
            let record = crate::agent_task_lifecycle::reconcile_status(&run_id)
                .expect("durable admission failure");
            let persisted = crate::agent_task_lifecycle::load_plan(&run_id)
                .expect("unadmitted plan remains inspectable");

            assert_eq!(error.retryable, Some(true));
            assert_eq!(error.details["run_id"], run_id);
            assert_ne!(
                record.lifecycle.execution.state,
                homeboy_core::run_lifecycle_record::RunExecutionState::Running
            );
            assert_eq!(
                record.metadata["pre_execution_failure"]["phase"],
                "admit_plan_provider_dispatchability"
            );
            assert_eq!(record.metadata["pre_execution_failure"]["retryable"], true);
            assert!(record
                .metadata
                .get("provider_executions")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty));
            assert_eq!(
                persisted.tasks[0].executor.selector.as_deref(),
                Some("pi.agent-task-executor")
            );
            assert_eq!(persisted.tasks[0].executor.model(), Some("test/model"));
            assert!(persisted.tasks[0]
                .metadata
                .get("provider_readiness_routing")
                .is_none());
        });
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
            "pi".to_string(),
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
                .contains("available backends: claude-code, codex, pi"),
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
    fn builtin_pi_backend_is_selected_when_no_policy_names_one() {
        let request = resolve_dispatch_request_with_sources(
            command_with_backend(None),
            |_| Ok(None),
            || None,
            || vec!["codex".to_string(), "pi".to_string()],
        )
        .expect("pi is declared");

        assert_eq!(request.backend, "pi");
        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.source, BackendSelectionSource::Builtin);
        assert!(!selection.overrides_default);
    }

    #[test]
    fn explicit_backend_is_attributed_to_cli_source() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("claude-code")),
            |_| Ok(Some("pi".to_string())),
            || Some("pi".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "claude-code");
        assert_eq!(selection.source, BackendSelectionSource::Cli);
        assert_eq!(selection.default_backend.as_deref(), Some("pi"));
        assert!(selection.overrides_default);
    }

    #[test]
    fn explicit_backend_matching_default_does_not_warn() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("pi")),
            |_| Ok(Some("pi".to_string())),
            || Some("pi".to_string()),
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
            |_| Ok(Some("pi".to_string())),
            || Some("pi".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "pi");
        assert_eq!(selection.source, BackendSelectionSource::Config);
        assert!(!selection.overrides_default);
    }

    #[test]
    fn cook_route_uses_the_supplied_component_without_reloading_by_id() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let component = homeboy_core::component::Component {
                id: "unregistered-cook-component".to_string(),
                extensions: Some(std::collections::HashMap::from([(
                    "policy".to_string(),
                    homeboy_core::component::ScopedExtensionConfig {
                        settings: std::collections::HashMap::from([(
                            "agent_task".to_string(),
                            serde_json::json!({"default_backend": "supplied-component-backend"}),
                        )]),
                        ..Default::default()
                    },
                )])),
                ..Default::default()
            };
            assert!(homeboy_core::component::load(&component.id).is_err());
            let command = AgentTaskDispatchCommand {
                repo: Some("owning-repository".to_string()),
                component: Some(component.id.clone()),
                model: Some("fixed-model".to_string()),
                ..Default::default()
            };
            let request = resolve_dispatch_request(command.clone(), Some(&component))
                .expect("resolve the supplied component policy");
            assert_eq!(request.backend, "supplied-component-backend");
            assert_eq!(request.repo.as_deref(), Some("owning-repository"));
            assert_eq!(request.component.as_deref(), Some(component.id.as_str()));
            let route = resolve_cook_initial_provider_route_with_catalog(
                command,
                &AgentTaskProviderCatalog::discover(),
                Some(&component),
            )
            .expect("resolve Cook route without a registered component");
            assert_eq!(route.backend, "supplied-component-backend");
        });
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
                Ok(Some("pi".to_string()))
            },
            || None,
        )
        .expect("component-scoped request");

        assert_eq!(request.repo.as_deref(), Some("blocks-engine"));
        assert_eq!(request.component.as_deref(), Some("php-transformer"));
        assert_eq!(request.backend, "pi");
    }

    #[test]
    fn configured_default_projects_the_same_initial_cook_route() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(None),
            |_| Ok(Some("pi".to_string())),
            || Some("pi".to_string()),
        )
        .expect("configured default request");
        let route =
            initial_provider_route_from_policy(controller_resolved_execution_policy_with_sources(
                &request,
                &AgentTaskProviderCatalog::default(),
                None,
            ));

        assert_eq!(route.backend, "pi");
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
            || Some("pi".to_string()),
        )
        .expect("request");

        let selection = request.backend_selection.expect("selection");
        assert_eq!(selection.backend, "component-backend");
        assert_eq!(selection.source, BackendSelectionSource::Policy);
        // Default-policy selections never count as an explicit override.
        assert!(!selection.overrides_default);
    }

    #[test]
    fn explicit_backend_without_model_takes_the_model_of_its_first_rotation_entry() {
        let request = resolve_dispatch_request_with_default_and_config(
            command_with_backend(Some("pi")),
            |_| Ok(Some("codex".to_string())),
            || Some("codex".to_string()),
        )
        .expect("explicit Pi request");
        let route =
            initial_provider_route_from_policy(controller_resolved_execution_policy_with_sources(
                &request,
                &AgentTaskProviderCatalog::default(),
                Some(AgentTaskProviderRotationPolicy {
                    entries: vec![
                        crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            backend: Some("codex".to_string()),
                            model: Some("codex-model".to_string()),
                            ..Default::default()
                        },
                        crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            backend: Some("pi".to_string()),
                            model: Some("roadie/subagent".to_string()),
                            ..Default::default()
                        },
                        crate::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            backend: Some("pi".to_string()),
                            model: Some("roadie/fallback".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
            ));

        assert_eq!(route.backend, "pi");
        assert_eq!(route.model.as_deref(), Some("roadie/subagent"));
        assert!(route.rotation_selected_initial);
        let remaining = route.rotation.expect("rotation").entries;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].model.as_deref(), Some("roadie/fallback"));
    }

    #[test]
    fn explicit_model_prevents_default_rotation_from_replacing_the_initial_invocation() {
        let request = resolve_dispatch_request_with_default_and_config(
            AgentTaskDispatchCommand {
                prompt: Some("Cook with Sol.".to_string()),
                model: Some("openai/gpt-5.6-sol".to_string()),
                ..AgentTaskDispatchCommand::default()
            },
            |_| Ok(Some("pi".to_string())),
            || Some("pi".to_string()),
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
