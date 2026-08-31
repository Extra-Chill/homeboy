//! Agent-task plan execution lifecycle: submit/run/resume/retry, workspace
//! preparation and component-worktree normalization, secret-env preflight,
//! and the shared `AgentTaskRunResult` envelope. Pure move out of the former
//! `agent_task_service.rs` god-file.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::agent_task::{
    AgentTaskFailureClassification, AgentTaskOutcomeStatus, AgentTaskRequest,
    AgentTaskWorkspaceMode,
};
use crate::agent_task_lifecycle::{self, AgentTaskRunArtifacts, AgentTaskRunRecord};
use crate::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, provider_secret_sources_for_plan,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutionBudget, AgentTaskPlan, AgentTaskScheduler,
    SharedAgentTaskExecutor,
};
use crate::agent_task_secrets::validate_secret_env_with_fallbacks;
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::{config, worktree, worktree_provider, Error, Result};

pub const AGENT_TASK_PLAN_VALIDATION_SCHEMA: &str = "homeboy/agent-task-plan-validation/v1";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPlanValidationKind {
    InvalidInput,
    UnavailableCapability,
    TemporaryCapacity,
    MissingReadiness,
    PolicyDenied,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskPlanValidationReport {
    pub schema: String,
    pub valid: bool,
    pub scope: String,
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<AgentTaskPlanValidationFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskPlanValidationFailure {
    pub kind: AgentTaskPlanValidationKind,
    pub code: String,
    pub reason: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

use super::cook_baseline::DerivedCookBaselineCapability;
use super::discovery::source_uri;

#[derive(Debug, Clone)]
pub struct AgentTaskRunResult<T> {
    pub value: T,
    pub exit_code: i32,
}

fn transport_proxy_recovery_error(recovery: agent_task_lifecycle::TransportProxyRecovery) -> Error {
    let recovery_message = match &recovery {
        agent_task_lifecycle::TransportProxyRecovery::Resumed { .. } => {
            "was resumed on its recorded runner; await its durable result"
        }
        _ => "is owned by runner transport recovery; provider execution was not attempted",
    };
    Error::validation_invalid_argument(
        "run_id",
        format!(
            "agent-task run '{}' {recovery_message}",
            recovery.record().run_id,
        ),
        Some(recovery.record().run_id.clone()),
        None,
    )
    .with_hint(format!("Next: {}", recovery.next_action()))
    .with_retryable(true)
}

pub fn read_plan(spec: &str) -> Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    plan.validate_managed_services().map_err(|message| {
        Error::validation_invalid_argument("services.cleanup_deadline_ms", message, None, None)
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

/// Validate controller-visible plan syntax and provider readiness without
/// reserving a run, creating workspaces, or dispatching work.
pub fn validate_plan_spec(spec: &str) -> AgentTaskPlanValidationReport {
    let plan = match read_plan(spec) {
        Ok(plan) => plan,
        Err(error) => {
            return invalid_plan_report(None, AgentTaskPlanValidationKind::InvalidInput, error)
        }
    };
    let plan_id = Some(plan.plan_id.clone());
    if let Err(error) = validate_plan_structure(&plan) {
        return invalid_plan_report(plan_id, AgentTaskPlanValidationKind::InvalidInput, error);
    }
    let plan = plan;
    let catalog = crate::agent_task_provider::AgentTaskProviderCatalog::discover();
    let plan = match crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
        &plan,
        &catalog,
        &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            let classifications = error.details["route_evidence"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|route| route["classification"].as_str())
                .collect::<Vec<_>>();
            let kind = if classifications.contains(&"capability") {
                AgentTaskPlanValidationKind::UnavailableCapability
            } else if classifications.contains(&"capacity") {
                AgentTaskPlanValidationKind::TemporaryCapacity
            } else {
                AgentTaskPlanValidationKind::MissingReadiness
            };
            return invalid_plan_report(plan_id, kind, error);
        }
    };
    if let Err(error) = validate_plan_provider_capabilities(&plan, &catalog) {
        return invalid_plan_report(
            plan_id,
            AgentTaskPlanValidationKind::UnavailableCapability,
            error,
        );
    }
    for (kind, result) in [
        (
            AgentTaskPlanValidationKind::UnavailableCapability,
            catalog.validate_selected_models(&plan),
        ),
        (
            AgentTaskPlanValidationKind::MissingReadiness,
            catalog.enforce_runtime_preflight_checks_for_plan(&plan),
        ),
        (
            AgentTaskPlanValidationKind::MissingReadiness,
            preflight_plan_secret_env(&plan),
        ),
        (
            AgentTaskPlanValidationKind::MissingReadiness,
            crate::agent_task_provider::preflight_plan_provider_config_with_providers(
                &plan,
                catalog.providers(),
            ),
        ),
    ] {
        if let Err(error) = result {
            return invalid_plan_report(plan_id, kind, error);
        }
    }
    AgentTaskPlanValidationReport {
        schema: AGENT_TASK_PLAN_VALIDATION_SCHEMA.to_string(),
        valid: true,
        scope: plan_validation_scope(),
        plan_id,
        failures: Vec::new(),
    }
}

fn invalid_plan_report(
    plan_id: Option<String>,
    kind: AgentTaskPlanValidationKind,
    error: Error,
) -> AgentTaskPlanValidationReport {
    AgentTaskPlanValidationReport {
        schema: AGENT_TASK_PLAN_VALIDATION_SCHEMA.to_string(),
        valid: false,
        scope: plan_validation_scope(),
        plan_id,
        failures: vec![AgentTaskPlanValidationFailure {
            kind,
            code: error.code.as_str().to_string(),
            reason: error.message,
            retryable: error.retryable.unwrap_or(false),
            details: error.details,
            hints: error.hints.into_iter().map(|hint| hint.message).collect(),
        }],
    }
}

fn plan_validation_scope() -> String {
    homeboy_core::resource_policy_context::lab_execution_runner_id()
        .map(|runner_id| format!("runner:{runner_id}"))
        .unwrap_or_else(|| "local_controller".to_string())
}

fn validate_plan_structure(plan: &AgentTaskPlan) -> Result<()> {
    use crate::agent_task::{AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA};
    use crate::agent_task_schedule::AGENT_TASK_PLAN_SCHEMA;
    use std::collections::HashSet;

    if plan.schema != AGENT_TASK_PLAN_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "unsupported agent-task plan schema '{}'; supported schema is '{AGENT_TASK_PLAN_SCHEMA}'",
                plan.schema
            ),
            Some(plan.schema.clone()),
            None,
        ));
    }
    if plan.plan_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "plan_id",
            "agent-task plan_id must not be empty",
            Some(plan.plan_id.clone()),
            None,
        ));
    }
    if plan.tasks.is_empty() {
        return Err(Error::validation_invalid_argument(
            "tasks",
            "agent-task plan must contain at least one task",
            None,
            None,
        ));
    }

    let mut task_ids = HashSet::new();
    for task in &plan.tasks {
        if task.schema != AGENT_TASK_REQUEST_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "tasks.schema",
                format!(
                    "task '{}' uses unsupported request schema '{}'; supported schema is '{AGENT_TASK_REQUEST_SCHEMA}'",
                    task.task_id, task.schema
                ),
                Some(task.schema.clone()),
                None,
            ));
        }
        if task.task_id.trim().is_empty() || !task_ids.insert(task.task_id.as_str()) {
            return Err(Error::validation_invalid_argument(
                "tasks.task_id",
                format!(
                    "task ids must be non-empty and unique; invalid id '{}';",
                    task.task_id
                ),
                Some(task.task_id.clone()),
                None,
            ));
        }
        if task.instructions.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "tasks.instructions",
                format!("task '{}' has empty instructions", task.task_id),
                Some(task.task_id.clone()),
                None,
            ));
        }
        if task
            .source_refs
            .iter()
            .any(|source| source.kind.trim().is_empty() || source.uri.trim().is_empty())
        {
            return Err(Error::validation_invalid_argument(
                "tasks.source_refs",
                format!(
                    "task '{}' has a source reference with an empty kind or URI",
                    task.task_id
                ),
                Some(task.task_id.clone()),
                None,
            ));
        }
        if matches!(task.workspace.mode, AgentTaskWorkspaceMode::Existing)
            && task.workspace.root.as_deref().is_none_or(str::is_empty)
        {
            return Err(Error::validation_invalid_argument(
                "tasks.workspace.root",
                format!(
                    "task '{}' uses an existing workspace without a root",
                    task.task_id
                ),
                Some(task.task_id.clone()),
                None,
            ));
        }
        if task.workspace.kind.as_deref() == Some("component-worktree") {
            if task
                .workspace
                .component_id
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(Error::validation_invalid_argument(
                    "tasks.workspace.component_id",
                    format!(
                        "task '{}' component-worktree workspace requires component_id",
                        task.task_id
                    ),
                    Some(task.task_id.clone()),
                    None,
                ));
            }
            let has_root = task
                .workspace
                .root
                .as_deref()
                .is_some_and(|root| !root.is_empty())
                || materialization_string(&task.workspace.materialization, "root").is_some()
                || materialization_string(&task.workspace.materialization, "resolved_root")
                    .is_some();
            if !has_root && task.workspace.branch.as_deref().is_none_or(str::is_empty) {
                return Err(Error::validation_invalid_argument(
                    "tasks.workspace.branch",
                    format!(
                        "task '{}' component-worktree workspace requires branch before materialization",
                        task.task_id
                    ),
                    Some(task.task_id.clone()),
                    None,
                ));
            }
        }
        if task.limits.timeout_ms == Some(0)
            || task.limits.max_runtime_ms == Some(0)
            || task.limits.liveness_timeout_ms == Some(0)
            || task.limits.max_output_bytes == Some(0)
        {
            return Err(Error::validation_invalid_argument(
                "tasks.limits",
                format!("task '{}' has a zero-valued execution limit", task.task_id),
                Some(task.task_id.clone()),
                None,
            ));
        }
        task.capability_requirements().map_err(|problem| {
            Error::validation_invalid_argument(
                "tasks.required_capabilities",
                format!(
                    "task '{}' has invalid capability requirements: {problem}",
                    task.task_id
                ),
                Some(task.task_id.clone()),
                None,
            )
        })?;
    }
    for (task_id, dependencies) in &plan.output_dependencies {
        if !task_ids.contains(task_id.as_str())
            || dependencies
                .depends_on
                .iter()
                .any(|dependency| !task_ids.contains(dependency.as_str()) || dependency == task_id)
        {
            return Err(Error::validation_invalid_argument(
                "output_dependencies",
                format!("task '{task_id}' has an unknown or self-referential dependency"),
                Some(task_id.clone()),
                None,
            ));
        }
    }
    plan.validate_managed_services()
        .map_err(|problem| Error::validation_invalid_argument("services", problem, None, None))
}

fn validate_plan_provider_capabilities(
    plan: &AgentTaskPlan,
    catalog: &crate::agent_task_provider::AgentTaskProviderCatalog,
) -> Result<()> {
    use crate::agent_task_provider::{resolve_provider_for_backend, ProviderResolution};

    for task in &plan.tasks {
        let provider = match resolve_provider_for_backend(
            catalog.providers(),
            &task.executor.backend,
            task.executor.selector.as_deref(),
        ) {
            ProviderResolution::Resolved(provider) => provider,
            ProviderResolution::NotFound => {
                return Err(Error::runner_capability_missing(
                    "local_controller",
                    &task.task_id,
                    Vec::new(),
                    vec![task.executor.backend.clone()],
                ));
            }
            ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => {
                return Err(Error::validation_invalid_argument(
                    "tasks.executor.selector",
                    format!(
                        "task '{}' backend '{}' is ambiguous; select one of: {}",
                        task.task_id,
                        task.executor.backend,
                        candidate_ids.join(", ")
                    ),
                    task.executor.selector.clone(),
                    None,
                ));
            }
            ProviderResolution::SelectorMismatch { available_ids, .. } => {
                return Err(Error::validation_invalid_argument(
                    "tasks.executor.selector",
                    format!(
                        "task '{}' selector does not match backend '{}'; available providers: {}",
                        task.task_id,
                        task.executor.backend,
                        available_ids.join(", ")
                    ),
                    task.executor.selector.clone(),
                    None,
                ));
            }
        };
        let requirements = task.capability_requirements().map_err(|problem| {
            Error::validation_invalid_argument(
                "tasks.required_capabilities",
                problem,
                Some(task.task_id.clone()),
                None,
            )
        })?;
        let missing = requirements
            .provider
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(Error::runner_capability_missing(
                &provider.id,
                &task.task_id,
                missing,
                Vec::new(),
            ));
        }
    }
    Ok(())
}

pub fn run_loaded_plan(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    run_loaded_plan_with_derived_cook_baseline(plan, record_run_id, executor, None, None)
}

pub(crate) fn run_loaded_plan_with_derived_cook_baseline(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    supplied_harvest_context: Option<crate::agent_task_scheduler::HarvestExecutionContext>,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    run_loaded_plan_with_derived_cook_baseline_in_optional_store(
        None,
        plan,
        record_run_id,
        executor,
        derived_cook_baseline,
        supplied_harvest_context,
    )
}

pub(crate) fn run_loaded_plan_with_derived_cook_baseline_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    supplied_harvest_context: Option<crate::agent_task_scheduler::HarvestExecutionContext>,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    run_loaded_plan_with_derived_cook_baseline_in_optional_store(
        Some(lifecycle_store),
        plan,
        record_run_id,
        executor,
        derived_cook_baseline,
        supplied_harvest_context,
    )
}

fn run_loaded_plan_with_derived_cook_baseline_in_optional_store(
    lifecycle_store: Option<&agent_task_lifecycle::AgentTaskLifecycleStore>,
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    supplied_harvest_context: Option<crate::agent_task_scheduler::HarvestExecutionContext>,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    if let Some(run_id) = record_run_id {
        // Prepare before persistence so the lifecycle record and scheduler use
        // the same materialized workspace contract. In particular, Cook's
        // derived baseline capability must bind the persisted task workspace.
        if let Err(error) = prepare_plan_for_execution(&mut plan, Some(run_id)) {
            match lifecycle_store {
                Some(store) => {
                    store.submit_plan_with_current_runtime(&plan, run_id)?;
                    store.record_pre_execution_failure(
                        run_id,
                        &plan,
                        "prepare_plan_for_execution",
                        &error,
                    )?;
                }
                None => {
                    agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
                    agent_task_lifecycle::record_pre_execution_failure(
                        run_id,
                        &plan,
                        "prepare_plan_for_execution",
                        &error,
                    )?;
                }
            }
            return Err(error);
        }
        match lifecycle_store {
            Some(store) => {
                store.submit_plan_with_current_runtime(&plan, run_id)?;
            }
            None => {
                agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            }
        }
        let harvest_context = match supplied_harvest_context.clone().map(Ok).unwrap_or_else(
            crate::agent_task_scheduler::HarvestExecutionContext::from_current_process,
        ) {
            Ok(context) => context,
            Err(error) => {
                match lifecycle_store {
                    Some(store) => store.record_pre_execution_failure(
                        run_id,
                        &plan,
                        "validate_harvest_transport",
                        &error,
                    )?,
                    None => agent_task_lifecycle::record_pre_execution_failure(
                        run_id,
                        &plan,
                        "validate_harvest_transport",
                        &error,
                    )?,
                };
                return Err(error);
            }
        };
        if harvest_context.snapshot_signaled() {
            bind_runner_snapshot_workspace_attestations(&mut plan)?;
        }
        match lifecycle_store {
            Some(store) => {
                store.mark_running(run_id)?;
            }
            None => {
                agent_task_lifecycle::mark_running(run_id)?;
            }
        }
        let aggregate = run_plan_with_scheduler(
            lifecycle_store,
            plan.clone(),
            record_run_id,
            executor,
            derived_cook_baseline,
            harvest_context,
        )?;
        match lifecycle_store {
            Some(store) => {
                store.record_run_aggregate(run_id, &plan, &aggregate)?;
            }
            None => {
                agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
            }
        }
        return Ok(AgentTaskRunResult {
            exit_code: aggregate_exit_code(&aggregate),
            value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
        });
    } else {
        prepare_plan_for_execution(&mut plan, None)?;
    }

    let harvest_context = supplied_harvest_context
        .unwrap_or(crate::agent_task_scheduler::HarvestExecutionContext::from_current_process()?);
    if harvest_context.snapshot_signaled() {
        bind_runner_snapshot_workspace_attestations(&mut plan)?;
    }
    let aggregate = run_plan_with_scheduler(
        lifecycle_store,
        plan.clone(),
        record_run_id,
        executor,
        derived_cook_baseline,
        harvest_context,
    )?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
    })
}

/// A Lab plan carries its controller admission identity until its paths are
/// materialized on the runner. Bind that concrete runner snapshot before the
/// plan is persisted or executed; the predecessor remains audit provenance.
pub fn bind_runner_snapshot_workspace_attestations(plan: &mut AgentTaskPlan) -> Result<()> {
    for task in &mut plan.tasks {
        let Some(current_identity) = task.metadata.get("cook_workspace_identity").cloned() else {
            continue;
        };
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace",
                "Cook runner snapshot identity requires a workspace root",
                Some(task.task_id.clone()),
                None,
            )
        })?;
        let runner_identity =
            crate::agent_task_workspace_identity::attest_workspace(std::path::Path::new(root))?;
        task.metadata["cook_workspace_identity"] = runner_identity;
        if task
            .metadata
            .get("cook_workspace_identity_predecessor")
            .is_none()
        {
            task.metadata["cook_workspace_identity_predecessor"] = current_identity;
        }
    }
    Ok(())
}

pub fn submit_plan_spec(spec: &str, run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let plan = read_plan(spec)?;
    agent_task_lifecycle::submit_plan(&plan, run_id)
}

pub fn run_submitted(
    run_id: String,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    run_submitted_with_timeout(run_id, None, executor)
}

pub fn run_submitted_with_timeout(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    if let Some(result) = terminal_run_result(&run_id)? {
        return Ok(result);
    }
    if let Some(recovery) = agent_task_lifecycle::recover_transport_proxy(&run_id)? {
        if let Ok(aggregate) = agent_task_lifecycle::read_aggregate(&recovery.record().run_id) {
            return Ok(AgentTaskRunResult {
                exit_code: aggregate_exit_code(&aggregate),
                value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
            });
        }
        return Err(transport_proxy_recovery_error(recovery));
    }
    let mut plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
    if let Some(timeout_ms) = timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    prepare_plan_for_execution(&mut plan, Some(&run_id))?;
    let harvest_context =
        match crate::agent_task_scheduler::HarvestExecutionContext::from_current_process() {
            Ok(context) => context,
            Err(error) => {
                agent_task_lifecycle::record_pre_execution_failure(
                    &run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
    agent_task_lifecycle::mark_running(&run_id)?;
    run_prepared_claimed(run_id, plan, executor, harvest_context)
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskRunNextSkip {
    pub run_id: String,
    pub submitted_at: Option<String>,
    pub age_seconds: Option<i64>,
    pub dispatcher_kind: Option<String>,
    pub category: String,
    pub error_code: String,
    pub summary: String,
    pub provider_id: Option<String>,
    pub required_environment_variables: Vec<String>,
    pub reason: String,
    pub remediation: String,
}

#[derive(Debug)]
pub struct AgentTaskRunNextResult {
    pub value: Option<AgentTaskAggregate>,
    pub exit_code: i32,
    pub skipped: Vec<AgentTaskRunNextSkip>,
    pub queue_admission: AgentTaskQueueAdmission,
}

#[derive(Debug, Serialize)]
pub struct AgentTaskQueueAdmission {
    pub inspected: usize,
    pub limit_reached: bool,
}

pub fn run_next(executor: SharedAgentTaskExecutor) -> Result<AgentTaskRunNextResult> {
    run_next_with_cook_dispatcher(executor, |_| Ok(None), None)
}

pub fn run_next_with_cook_dispatcher(
    executor: SharedAgentTaskExecutor,
    dispatcher: impl Fn(
        &Value,
    ) -> Result<
        Option<std::sync::Arc<dyn super::cook::AgentTaskCookAttemptDispatcher>>,
    >,
    scoped_run_ids: Option<&HashSet<String>>,
) -> Result<AgentTaskRunNextResult> {
    run_next_with_cook_dispatcher_and_queue_preflight(
        executor,
        dispatcher,
        scoped_run_ids,
        |record, plan| {
            validate_queued_cook_identity(record)?;
            preflight_queued_plan_provider_eligibility(plan)
        },
    )
}

pub(crate) fn run_next_with_cook_dispatcher_and_queue_preflight(
    executor: SharedAgentTaskExecutor,
    dispatcher: impl Fn(
        &Value,
    ) -> Result<
        Option<std::sync::Arc<dyn super::cook::AgentTaskCookAttemptDispatcher>>,
    >,
    scoped_run_ids: Option<&HashSet<String>>,
    queue_preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskRunNextResult> {
    let mut skipped = Vec::new();
    let mut inspected = 0;
    if let Some(scoped_run_ids) = scoped_run_ids {
        let mut scoped_run_ids = scoped_run_ids.iter().collect::<Vec<_>>();
        scoped_run_ids.sort();
        for run_id in scoped_run_ids {
            let record = agent_task_lifecycle::exact_record(run_id)?;
            let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(claim) = super::claim_continuation_for(cook_id, run_id)? else {
                continue;
            };
            let result =
                consume_claimed_continuation(claim, executor.clone(), &dispatcher, skipped)?;
            if result.value.is_some() {
                return Ok(result);
            }
            skipped = result.skipped;
        }
    } else {
        loop {
            let continuation = super::claim_continuation_with_budget(
                agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS.saturating_sub(inspected),
            )?;
            inspected += continuation.inspected;
            if continuation.limit_reached {
                return Ok(AgentTaskRunNextResult {
                    value: None,
                    exit_code: 0,
                    skipped,
                    queue_admission: AgentTaskQueueAdmission {
                        inspected,
                        limit_reached: true,
                    },
                });
            }
            let Some(claim) = continuation.claim else {
                break;
            };
            let result =
                consume_claimed_continuation(claim, executor.clone(), &dispatcher, skipped)?;
            if result.value.is_some() {
                return Ok(AgentTaskRunNextResult {
                    queue_admission: AgentTaskQueueAdmission {
                        inspected,
                        limit_reached: false,
                    },
                    ..result
                });
            }
            skipped = result.skipped;
        }
    }
    let remaining_budget =
        agent_task_lifecycle::MAX_QUEUE_ADMISSION_RECORDS.saturating_sub(inspected);
    if remaining_budget == 0 {
        return Ok(AgentTaskRunNextResult {
            value: None,
            exit_code: 0,
            skipped,
            queue_admission: AgentTaskQueueAdmission {
                inspected,
                limit_reached: true,
            },
        });
    }
    let claim =
        agent_task_lifecycle::claim_next_eligible_queued_run_with_preflight_and_filter_and_limit(
            |record| scoped_run_ids.is_none_or(|run_ids| run_ids.contains(&record.run_id)),
            remaining_budget,
            |record, plan| queue_preflight(record, plan),
        )?;
    let queue_admission = AgentTaskQueueAdmission {
        inspected: inspected + claim.inspected,
        limit_reached: claim.admission_limit_reached,
    };
    skipped.extend(claim.skipped.into_iter().map(|skip| AgentTaskRunNextSkip {
        run_id: skip.run_id,
        submitted_at: skip.submitted_at,
        age_seconds: skip.age_seconds,
        dispatcher_kind: skip.dispatcher_kind,
        category: skip.category,
        error_code: skip.error_code,
        summary: skip.summary,
        provider_id: skip.provider_id,
        required_environment_variables: skip.required_environment_variables,
        reason: skip.reason,
        remediation: skip.remediation,
    }));
    let Some(record) = claim.record else {
        return Ok(AgentTaskRunNextResult {
            value: None,
            exit_code: 0,
            skipped,
            queue_admission,
        });
    };

    let result = run_claimed(record.run_id, executor)?;
    Ok(AgentTaskRunNextResult {
        value: Some(crate::agent_task_artifacts::reviewer_facing_aggregate(
            &result.value,
        )),
        exit_code: result.exit_code,
        skipped,
        queue_admission,
    })
}

fn validate_queued_cook_identity(record: &AgentTaskRunRecord) -> Result<()> {
    let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let recipe = super::load_recipe(cook_id)?;
    super::validate_recipe_attempt_record(&recipe, &record.run_id, record)
}

fn consume_claimed_continuation(
    claim: super::ClaimedCookContinuation,
    executor: SharedAgentTaskExecutor,
    dispatcher: &impl Fn(
        &Value,
    ) -> Result<
        Option<std::sync::Arc<dyn super::cook::AgentTaskCookAttemptDispatcher>>,
    >,
    mut skipped: Vec<AgentTaskRunNextSkip>,
) -> Result<AgentTaskRunNextResult> {
    let store = super::CookRecipeStore::from_current_data_root()?;
    let cook_id = claim.continuation().cook_id.clone();
    let run_id = claim.continuation().run_id.clone();
    let recipe = match store.load_recipe(&cook_id) {
        Ok(recipe) => recipe,
        Err(error) => {
            claim.fail(&redacted_continuation_failure(&error))?;
            skipped.push(continuation_skip(run_id, None, false, error));
            return Ok(unclaimed_continuation_result(skipped));
        }
    };
    let dispatcher_value = recipe.promotion_transport.pointer("/attempt_dispatch/kind");
    let dispatcher_kind = dispatcher_value
        .and_then(Value::as_str)
        .and_then(agent_task_lifecycle::trusted_dispatcher_kind);
    let unsupported_dispatcher_kind = dispatcher_value.is_some() && dispatcher_kind.is_none();
    if let Err(error) =
        dispatcher(&recipe.promotion_transport["attempt_dispatch"]).and_then(|attempt_dispatcher| {
            super::reconstruct_options_with_dispatcher(&recipe, attempt_dispatcher).map(|_| ())
        })
    {
        claim.fail(&redacted_continuation_failure(&error))?;
        skipped.push(continuation_skip(
            run_id,
            dispatcher_kind,
            unsupported_dispatcher_kind,
            error,
        ));
        return Ok(unclaimed_continuation_result(skipped));
    }
    let exit_code = store.consume_claimed_with_dispatcher(
        claim,
        |recipe| dispatcher(recipe),
        |options| {
            let lifecycle_store =
                crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
            super::CookService::run(
                options,
                super::CookRuntime::production(executor.clone(), &store, &lifecycle_store),
                super::CookMode::Resume,
            )
            .map(|result| result.exit_code)
        },
    )?;
    let latest_run_id = agent_task_lifecycle::cook_index(&cook_id)
        .map(|index| index.latest_run_id)
        .unwrap_or(run_id);
    let aggregate = agent_task_lifecycle::read_aggregate(&latest_run_id).ok();
    Ok(AgentTaskRunNextResult {
        value: aggregate
            .map(|aggregate| crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate)),
        exit_code,
        skipped,
        queue_admission: AgentTaskQueueAdmission {
            inspected: 0,
            limit_reached: false,
        },
    })
}

fn unclaimed_continuation_result(skipped: Vec<AgentTaskRunNextSkip>) -> AgentTaskRunNextResult {
    AgentTaskRunNextResult {
        value: None,
        exit_code: 0,
        skipped,
        queue_admission: AgentTaskQueueAdmission {
            inspected: 0,
            limit_reached: false,
        },
    }
}

fn redacted_continuation_failure(error: &Error) -> String {
    format!(
        "Cook continuation admission failed ({})",
        error.code.as_str()
    )
}

fn continuation_skip(
    run_id: String,
    dispatcher_kind: Option<String>,
    unsupported_dispatcher_kind: bool,
    error: Error,
) -> AgentTaskRunNextSkip {
    let submitted_at = agent_task_lifecycle::exact_record(&run_id)
        .ok()
        .map(|record| record.submitted_at);
    let age_seconds = submitted_at.as_deref().and_then(|submitted_at| {
        DateTime::parse_from_rfc3339(submitted_at)
            .ok()
            .map(|submitted_at| {
                (Utc::now() - submitted_at.with_timezone(&Utc))
                    .num_seconds()
                    .max(0)
            })
    });
    AgentTaskRunNextSkip {
        remediation: format!(
            "inspect retained diagnostics with: homeboy agent-task diagnose {run_id} --full"
        ),
        run_id,
        submitted_at,
        age_seconds,
        dispatcher_kind,
        category: if unsupported_dispatcher_kind {
            "cook_continuation_unsupported_dispatcher"
        } else {
            "cook_continuation_preflight_failed"
        }
        .to_string(),
        error_code: error.code.as_str().to_string(),
        summary: "Cook continuation failed admission preflight".to_string(),
        provider_id: None,
        required_environment_variables: Vec::new(),
        reason: "Cook continuation failed admission preflight".to_string(),
    }
}

pub fn resume(
    run_id: String,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    if let Some(result) = terminal_run_result(&run_id)? {
        return Ok(result);
    }
    if let Some(recovery) = agent_task_lifecycle::recover_transport_proxy(&run_id)? {
        if let Ok(aggregate) = agent_task_lifecycle::read_aggregate(&recovery.record().run_id) {
            return Ok(AgentTaskRunResult {
                exit_code: aggregate_exit_code(&aggregate),
                value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
            });
        }
        return Err(transport_proxy_recovery_error(recovery));
    }
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    agent_task_lifecycle::mark_resuming_in_store(&lifecycle_store, &run_id)?;
    run_claimed(run_id, executor)
}

/// Reconcile a completed run's controller-owned artifact projection without
/// resuming or redispatching provider execution.
pub fn reconcile_terminal_artifact_projection(run_id: &str) -> Result<bool> {
    agent_task_lifecycle::reconcile_terminal_artifact_projection(run_id)
}

/// Replay an authenticated terminal runner snapshot without resuming provider work.
pub fn recover_terminal_transport_proxy_evidence(run_id: &str) -> Result<bool> {
    agent_task_lifecycle::recover_terminal_transport_proxy_evidence(run_id)
}

pub fn terminal_transport_recovery_required(run_id: &str) -> bool {
    agent_task_lifecycle::read_aggregate(run_id).map_or(true, |aggregate| {
        aggregate.outcomes.is_empty()
            && (aggregate.totals.queued
                + aggregate.totals.running
                + aggregate.totals.blocked
                + aggregate.totals.skipped
                + aggregate.totals.succeeded
                + aggregate.totals.failed
                + aggregate.totals.cancelled
                + aggregate.totals.timed_out
                + aggregate.totals.candidate_recoverable
                + aggregate.totals.recoverable_candidates)
                > 0
    })
}

/// Return durable terminal evidence instead of attempting to transition a
/// completed child run back into execution during controller reconciliation.
pub fn terminal_run_result(run_id: &str) -> Result<Option<AgentTaskRunResult<AgentTaskAggregate>>> {
    let record = agent_task_lifecycle::reconcile_status(run_id)?;
    if !matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialFailure
            | agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::Cancelled
    ) {
        return Ok(None);
    }

    let aggregate = match agent_task_lifecycle::read_aggregate(&record.run_id) {
        Ok(aggregate) => aggregate,
        Err(_) => {
            // A terminal Lab result may have been persisted before its typed
            // aggregate projection. Reconcile only that recorded terminal
            // evidence; never resume or rerun the provider for this path.
            agent_task_lifecycle::recover_terminal_transport_proxy_evidence(&record.run_id)?;
            agent_task_lifecycle::read_aggregate(&record.run_id).map_err(|_| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is terminal with state {:?} but has no durable aggregate evidence",
                record.run_id, record.state
            ),
            Some(record.run_id.clone()),
            Some(vec![format!(
                "retry the child with: homeboy agent-task retry {} --run",
                record.run_id
            )]),
        )
            })?
        }
    };
    Ok(Some(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: crate::agent_task_artifacts::reviewer_facing_aggregate(&aggregate),
    }))
}

pub fn retry(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
    force: bool,
) -> Result<AgentTaskRetryServiceResult> {
    retry_with_preflight_and_timeout(run_id, new_run_id, run, force, None, |plan| {
        if plan.metadata.get("generic_lab_command_replay").is_some() {
            return Err(Error::validation_invalid_argument(
                "generic_lab_command_replay",
                "generic Lab replay requires controller workspace preflight",
                Some(plan.plan_id.clone()),
                None,
            ));
        }
        Ok(())
    })
}

/// Reserve a new Cook attempt with an explicit operator-approved provider
/// timeout increase. Unlike the generic run-time override, this is persisted in
/// the append-only Cook recipe before the provider can be dispatched.
pub fn retry_with_timeout_override(
    run_id: &str,
    timeout_ms: u64,
) -> Result<AgentTaskRetryServiceResult> {
    retry_with_preflight_and_timeout(run_id, None, false, false, Some(timeout_ms), |plan| {
        if plan.metadata.get("generic_lab_command_replay").is_some() {
            return Err(Error::validation_invalid_argument(
                "generic_lab_command_replay",
                "generic Lab replay requires controller workspace preflight",
                Some(plan.plan_id.clone()),
                None,
            ));
        }
        Ok(())
    })
}

pub(super) fn deferred_cleanup_receipt_is_terminal(
    outcome: &crate::agent_task::AgentTaskOutcome,
    run_id: &str,
) -> bool {
    outcome
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "cleanup_action")
        .and_then(|artifact| artifact.path.as_deref().map(|path| (artifact, path)))
        .and_then(|(artifact, path)| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .map(|receipt| (artifact, receipt))
        })
        .is_some_and(|(artifact, receipt)| {
            receipt["schema"] == "homeboy/agent-task-deferred-cleanup/v1"
                && receipt["run_id"] == run_id
                && receipt["task_id"] == outcome.task_id
                && receipt["attempt"] == artifact.metadata["attempt"]
                && matches!(
                    receipt["status"].as_str(),
                    Some("completed" | "completed_no_candidate" | "candidate_recovered" | "failed")
                )
        })
}

/// Reserve a retry only after the caller has revalidated the persisted action.
///
/// Generic Lab replays need controller-owned workspace validation that this
/// crate cannot perform. The ordinary service entry point rejects them; the
/// Lab route supplies that validation through this boundary before any retry
/// reservation or Cook recovery can mutate durable state.
pub fn retry_with_preflight<F>(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
    force: bool,
    preflight: F,
) -> Result<AgentTaskRetryServiceResult>
where
    F: Fn(&AgentTaskPlan) -> Result<()>,
{
    retry_with_preflight_and_timeout(run_id, new_run_id, run, force, None, preflight)
}

fn retry_with_preflight_and_timeout<F>(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
    force: bool,
    timeout_override_ms: Option<u64>,
    preflight: F,
) -> Result<AgentTaskRetryServiceResult>
where
    F: Fn(&AgentTaskPlan) -> Result<()>,
{
    // One lifecycle store for the whole retry. Reserving the successor,
    // proving the reservation is exact, persisting the controller plan, and
    // binding the Cook attempt are one durable lineage: a successor reserved in
    // one installation and bound in another is a retry that cannot be proved to
    // own the work it replaces (#7505).
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let source = agent_task_lifecycle::normalize_local_execution_placement_in_store(
        &lifecycle_store,
        run_id,
    )?;
    let source_plan =
        agent_task_lifecycle::load_controller_plan_in_store(&lifecycle_store, &source.run_id)?;
    preflight(&source_plan)?;
    let recovered_replacement = config::with_config_lock(|| {
        let Some(mut cook_retry) = retryable_cook_attempt(&lifecycle_store, &source)? else {
            return Ok(None);
        };
        if let Some(timeout_ms) = timeout_override_ms {
            apply_cook_timeout_override(&lifecycle_store, &source, &mut cook_retry, timeout_ms)?;
        }
        if !cook_retry.recipe_replacement {
            return Ok(None);
        }
        let retry_run_id = cook_retry
            .pending_run_id
            .as_deref()
            .expect("recipe replacement has a run id");
        let lifecycle_missing =
            !agent_task_lifecycle::run_record_exists_in_store(&lifecycle_store, retry_run_id)?;
        if lifecycle_missing {
            agent_task_lifecycle::with_retry_lineage_reservation_in_store(
                &lifecycle_store,
                &source.run_id,
                || {
                    // Recovery writes the missing successor directly, so it
                    // needs the same locked revalidation as a fresh retry.
                    preflight(&source_plan)?;
                    let recipe_store = super::cook_recipe::default_store()?;
                    super::cook_pre_execution::materialize_cook_attempt_with_stores(
                        &recipe_store,
                        &lifecycle_store,
                        &cook_retry.cook_id,
                        retry_run_id,
                        &cook_retry.plan,
                    )?;
                    agent_task_lifecycle::record_metadata_value_in_store(
                        &lifecycle_store,
                        retry_run_id,
                        "cook_retry_recipe_recovery",
                        json!({
                            "schema": "homeboy/cook-retry-recipe-recovery/v1",
                            "status": "materialized_orphaned_replacement",
                            "source_run_id": source.run_id,
                            "recovered_run_id": retry_run_id,
                        }),
                    )
                },
            )?;
        }
        Ok(Some(
            agent_task_lifecycle::reconcile_status_in_store(
                &lifecycle_store,
                retry_run_id,
                agent_task_lifecycle::AgentTaskStatusOptions::default(),
                false,
            )?
            .record,
        ))
    })?;
    if let Some(record) = recovered_replacement {
        let run = run && !record.state.is_terminal();
        return Ok(AgentTaskRetryServiceResult {
            record,
            run,
            created: false,
        });
    }
    let mut cook_retry = retry_admission_in_store(&lifecycle_store, &source, false)?;
    if let (Some(cook_retry), Some(timeout_ms)) = (&mut cook_retry, timeout_override_ms) {
        apply_cook_timeout_override(&lifecycle_store, &source, cook_retry, timeout_ms)?;
    }
    let record = match cook_retry {
        Some(cook_retry) => {
            let discovered_run_id =
                agent_task_lifecycle::find_unbound_cook_retry_successor_in_store(
                    &lifecycle_store,
                    &source.run_id,
                    &cook_retry.cook_id,
                    cook_retry.attempt,
                    &cook_retry.plan,
                )?
                .map(|record| record.run_id);
            let mut retry_run_id = cook_retry
                .pending_run_id
                .as_deref()
                .or(new_run_id)
                .map(str::to_string)
                .or(discovered_run_id)
                .unwrap_or_else(|| {
                    // All concurrent retries of one Cook attempt must reserve
                    // the same lifecycle successor before claim reconciliation.
                    format!("{}-retry", source.run_id)
                });
            let retry_exists =
                agent_task_lifecycle::run_record_exists_in_store(&lifecycle_store, &retry_run_id)?;
            if retry_exists
                && !is_exact_retry_reservation(
                    &lifecycle_store,
                    &source,
                    &cook_retry.plan,
                    &retry_run_id,
                )?
                && !is_pending_local_cook_retry_reservation(
                    &lifecycle_store,
                    &source,
                    &retry_run_id,
                )?
            {
                return Err(Error::validation_invalid_argument(
                    "new_run_id",
                    "retry run id is not the durable retry reservation for this Cook attempt",
                    Some(retry_run_id),
                    None,
                ));
            }
            // The lifecycle record is the durable reservation. It writes retry_of
            // before recipe/index binding, so recovery can prove ownership without
            // adopting an unrelated same-plan run.
            let mut created = false;
            if !retry_exists {
                let reservation = reserve_cook_retry_lifecycle(
                    &lifecycle_store,
                    &source,
                    &cook_retry,
                    &retry_run_id,
                    force || cook_retry.replaces_source_attempt,
                    &preflight,
                )?;
                retry_run_id = reservation.run_id;
                created = reservation.created;
            }
            // Recipe and index are one Cook-owned binding boundary. Serialize
            // concurrent claim observers so neither can overwrite the other's
            // append-only recipe revision between its read and write.
            let registration = config::with_config_lock(|| {
                // The retry reservation starts from the source plan. Persist
                // Cook-authored remediation inputs before binding either record
                // so a restarted executor loads the same plan as the recipe.
                agent_task_lifecycle::persist_controller_plan_in_store(
                    &lifecycle_store,
                    &retry_run_id,
                    &cook_retry.plan,
                )?;
                if cook_retry.replaces_source_attempt {
                    super::record_recipe_attempt_replacement(
                        &cook_retry.cook_id,
                        &source.run_id,
                        &retry_run_id,
                    )?;
                } else {
                    super::record_recipe_attempt(
                        &cook_retry.cook_id,
                        cook_retry.attempt,
                        &retry_run_id,
                        &cook_retry.plan,
                    )?;
                }
                agent_task_lifecycle::record_cook_attempt_locked_in_store(
                    &lifecycle_store,
                    &cook_retry.cook_id,
                    cook_retry.attempt,
                    &retry_run_id,
                )
            })?;
            registration.project_terminal_after_unlock()?;
            // `status` is this call with `Default::default()` and `exact = false`,
            // resolved against an ambient store. Same read, explicit root.
            let record = agent_task_lifecycle::reconcile_status_in_store(
                &lifecycle_store,
                &retry_run_id,
                agent_task_lifecycle::AgentTaskStatusOptions::default(),
                false,
            )?
            .record;
            if record.state.is_terminal() {
                return Ok(AgentTaskRetryServiceResult {
                    record,
                    run: false,
                    created,
                });
            }
            return Ok(AgentTaskRetryServiceResult {
                record,
                run,
                created,
            });
        }
        None => agent_task_lifecycle::retry_with_force_and_preflight_in_store(
            &lifecycle_store,
            &source.run_id,
            new_run_id,
            force,
            &preflight,
        )?,
    };
    Ok(AgentTaskRetryServiceResult {
        record,
        run,
        created: false,
    })
}

fn apply_cook_timeout_override(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    retry: &mut CookRetryAttempt,
    timeout_ms: u64,
) -> Result<()> {
    let aggregate = lifecycle_store.read_aggregate(&source.run_id)?;
    let review_form_only = retry
        .plan
        .tasks
        .iter()
        .any(crate::agent_task_cook_loop::request_is_review_form_only);
    let timeout_field = if review_form_only {
        "review-form-timeout-ms"
    } else {
        "timeout-ms"
    };
    let timeout_outcome = aggregate.outcomes.iter().find(|outcome| {
        outcome.status == AgentTaskOutcomeStatus::Timeout
            || outcome.failure_classification == Some(AgentTaskFailureClassification::Timeout)
            || outcome.diagnostics.iter().any(|diagnostic| {
                diagnostic.class == "agent_task.provider_timeout"
                    || diagnostic.class == "agent_task.review_form_timeout"
            })
    });
    let Some(timeout_outcome) = timeout_outcome else {
        return Err(Error::validation_invalid_argument(
            timeout_field,
            if review_form_only {
                "Cook review-form timeout override requires a terminal review-form timeout"
            } else {
                "Cook timeout override requires a terminal provider timeout"
            },
            Some(source.run_id.clone()),
            None,
        ));
    };
    if agent_task_lifecycle::has_active_provider_execution_in_store(
        lifecycle_store,
        &source.run_id,
    )? {
        return Err(Error::validation_invalid_argument(
            "timeout-ms",
            "timed-out provider or its deferred cleanup still owns the execution; wait for durable terminal ownership before retrying",
            Some(source.run_id.clone()),
            Some(vec![format!(
                "homeboy agent-task diagnose {} --full",
                source.run_id
            )]),
        ));
    }
    if timeout_outcome.metadata["deferred_cleanup_pending"] == Value::Bool(true)
        && !deferred_cleanup_receipt_is_terminal(timeout_outcome, &source.run_id)
    {
        return Err(Error::validation_invalid_argument(
            "timeout-ms",
            "timed-out provider still owns deferred cleanup; wait for its cleanup receipt before retrying",
            Some(source.run_id.clone()),
            Some(vec![format!(
                "homeboy agent-task diagnose {} --full",
                source.run_id
            )]),
        ));
    }

    if let Some(existing) = retry.plan.metadata["cook_timeout_overrides"]
        .as_array()
        .and_then(|overrides| {
            overrides
                .iter()
                .rev()
                .find(|entry| entry["source_run_id"] == source.run_id)
        })
    {
        if existing["timeout_ms"].as_u64() == Some(timeout_ms) {
            return Ok(());
        }
        return Err(Error::validation_invalid_argument(
            "timeout-ms",
            "this Cook retry already has a different durable timeout override",
            Some(existing["timeout_ms"].to_string()),
            None,
        ));
    }

    let diagnostic_timeout_ms = timeout_outcome
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.data["timeout_ms"].as_u64())
        .max();
    let previous_timeout_ms = retry
        .plan
        .tasks
        .iter()
        .map(|task| {
            crate::agent_task_timeout::effective_provider_timeout_ms(
                task.limits.timeout_ms.or(retry.plan.options.timeout_ms),
                task.limits.max_runtime_ms,
            )
        })
        .chain(diagnostic_timeout_ms)
        .max()
        .unwrap_or(crate::agent_task_timeout::DEFAULT_PROVIDER_TIMEOUT_MS);
    if timeout_ms <= previous_timeout_ms {
        return Err(Error::validation_invalid_argument(
            timeout_field,
            if review_form_only {
                format!(
                    "Cook review-form timeout override must increase the prior review-form timeout of {previous_timeout_ms}ms"
                )
            } else {
                format!(
                    "Cook timeout override must increase the prior provider timeout of {previous_timeout_ms}ms"
                )
            },
            Some(timeout_ms.to_string()),
            None,
        ));
    }
    if review_form_only && timeout_ms > crate::agent_task_cook_loop::MAX_REVIEW_FORM_TIMEOUT_MS {
        return Err(Error::validation_invalid_argument(
            timeout_field,
            format!(
                "Cook review-form timeout cannot exceed {}ms",
                crate::agent_task_cook_loop::MAX_REVIEW_FORM_TIMEOUT_MS
            ),
            Some(timeout_ms.to_string()),
            None,
        ));
    }

    let recipe = super::load_recipe(&retry.cook_id)?;
    let budget: AgentTaskExecutionBudget = serde_json::from_value(
        recipe.retry_budget["execution_budget"].clone(),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "cook_recipe.retry_budget.execution_budget",
            format!("durable Cook execution budget is invalid: {error}"),
            Some(retry.cook_id.clone()),
            None,
        )
    })?;
    if budget.remaining_deadline_ms(crate::agent_task_timeout::now_unix_ms()) == Some(0) {
        return Err(Error::validation_invalid_argument(
            "timeout-ms",
            "Cook timeout override cannot extend an expired durable execution deadline",
            Some(source.run_id.clone()),
            None,
        ));
    }
    let mut executions_used = 0u32;
    let mut scheduler_retries_used = 0u32;
    let mut provider_rotations_used = 0u32;
    let mut semantic_attempts = std::collections::BTreeSet::new();
    for attempt in &recipe.attempts {
        semantic_attempts.insert(attempt.attempt);
        if let Ok(record) = lifecycle_store.read_record(&attempt.run_id) {
            executions_used = executions_used.saturating_add(
                record.metadata["provider_executions_consumed"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
            );
        }
        if let Ok(aggregate) = lifecycle_store.read_aggregate(&attempt.run_id) {
            scheduler_retries_used = scheduler_retries_used.saturating_add(
                aggregate
                    .outcomes
                    .iter()
                    .flat_map(|outcome| &outcome.diagnostics)
                    .filter(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
                    .count()
                    .try_into()
                    .unwrap_or(u32::MAX),
            );
            provider_rotations_used = provider_rotations_used.saturating_add(
                aggregate
                    .outcomes
                    .iter()
                    .filter_map(super::cook_pre_execution::provider_rotation_attempts)
                    .map(|attempts| attempts.len().saturating_sub(1) as u32)
                    .fold(0, u32::saturating_add),
            );
        }
    }
    let semantic_retries_used = semantic_attempts.len().saturating_sub(1) as u32;
    let same_provider_retries_used = scheduler_retries_used.saturating_add(semantic_retries_used);
    let remaining_executions = budget
        .max_provider_executions
        .saturating_sub(executions_used);
    let remaining_retries = budget
        .max_same_provider_retries
        .saturating_sub(same_provider_retries_used);
    let remaining_rotations = budget
        .max_provider_rotations
        .saturating_sub(provider_rotations_used);
    if remaining_executions == 0 || remaining_retries == 0 {
        return Err(Error::validation_invalid_argument(
            "timeout-ms",
            "Cook timeout override cannot exceed the durable provider retry budget",
            Some(source.run_id.clone()),
            None,
        ));
    }

    apply_timeout_override_to_plan(
        &mut retry.plan,
        &source.run_id,
        previous_timeout_ms,
        timeout_ms,
        remaining_executions,
        remaining_retries,
        remaining_rotations,
        review_form_only,
    )?;
    Ok(())
}

fn apply_timeout_override_to_plan(
    plan: &mut AgentTaskPlan,
    source_run_id: &str,
    previous_timeout_ms: u64,
    timeout_ms: u64,
    remaining_executions: u32,
    remaining_retries: u32,
    remaining_rotations: u32,
    review_form_only: bool,
) -> Result<()> {
    if !plan.metadata.is_null() && !plan.metadata.is_object() {
        return Err(Error::validation_invalid_argument(
            "cook_plan.metadata",
            "durable Cook plan metadata must be an object before recording a timeout override",
            Some(source_run_id.to_string()),
            None,
        ));
    }
    if plan.metadata["cook_timeout_overrides"] != Value::Null
        && !plan.metadata["cook_timeout_overrides"].is_array()
    {
        return Err(Error::validation_invalid_argument(
            "cook_plan.metadata.cook_timeout_overrides",
            "durable Cook timeout override history must be an array",
            Some(source_run_id.to_string()),
            None,
        ));
    }
    plan.options.timeout_ms = Some(timeout_ms);
    plan.options.execution_budget.max_provider_executions = remaining_executions;
    plan.options.execution_budget.max_same_provider_retries = remaining_retries.saturating_sub(1);
    plan.options.execution_budget.max_provider_rotations = remaining_rotations;
    for task in &mut plan.tasks {
        task.limits.timeout_ms = Some(timeout_ms);
        if review_form_only {
            if !task.metadata.is_object() {
                task.metadata = json!({});
            }
            if !task.metadata["cook_loop"].is_object() {
                task.metadata["cook_loop"] = json!({});
            }
            task.metadata["cook_loop"]["review_form_timeout_ms"] = json!(timeout_ms);
        }
    }
    let timeout_override = json!({
        "schema": "homeboy/agent-task-cook-timeout-override/v1",
        "source_run_id": source_run_id,
        "previous_timeout_ms": previous_timeout_ms,
        "timeout_ms": timeout_ms,
        "authority": if review_form_only {
            "operator --review-form-timeout-ms"
        } else {
            "operator --timeout-ms"
        },
        "remaining_provider_executions": remaining_executions,
        "remaining_same_provider_retries_after_reservation": remaining_retries.saturating_sub(1),
        "remaining_provider_rotations": remaining_rotations,
    });
    if plan.metadata.is_null() {
        plan.metadata = json!({});
    }
    if review_form_only {
        if !plan.metadata["cook_loop"].is_object() {
            plan.metadata["cook_loop"] = json!({});
        }
        plan.metadata["cook_loop"]["review_form_timeout_ms"] = json!(timeout_ms);
    }
    plan.metadata
        .as_object_mut()
        .expect("plan metadata is an object")
        .entry("cook_timeout_overrides")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("cook_timeout_overrides is an array")
        .push(timeout_override);
    plan.rebuild_homeboy_plan();
    Ok(())
}

struct CookRetryReservation {
    run_id: String,
    created: bool,
}

fn local_cook_retry_reservation_metadata(
    cook_id: &str,
    retry_run_id: &str,
    lease_started_at: chrono::DateTime<chrono::Utc>,
    launcher_pid: u32,
    launcher_start_identity: homeboy_core::process::ProcessStartIdentity,
) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        ("cook_id".to_string(), json!(cook_id)),
        (
            "local_cook_supervisor".to_string(),
            json!({
                "state": "pending",
                "pinned_run_id": retry_run_id,
                "lease_started_at": lease_started_at.to_rfc3339(),
                "lease_expires_at": (lease_started_at + chrono::Duration::seconds(agent_task_lifecycle::LOCAL_COOK_SUPERVISOR_LEASE_SECONDS)).to_rfc3339(),
                "launcher_pid": launcher_pid,
                "launcher_process_start_identity": launcher_start_identity,
            }),
        ),
    ])
}

fn reserve_cook_retry_lifecycle(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    retry: &CookRetryAttempt,
    retry_run_id: &str,
    force: bool,
    preflight: &dyn Fn(&AgentTaskPlan) -> Result<()>,
) -> Result<CookRetryReservation> {
    let operation_key = format!("retry:{}:{}", retry.cook_id, retry.attempt);
    match agent_task_lifecycle::claim_cook_operation_in_store(
        lifecycle_store,
        &source.run_id,
        &operation_key,
        Duration::from_secs(30),
    )? {
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            // The Cook operation claim coordinates recipe/index binding; the
            // lifecycle retry lock also makes the first durable successor
            // idempotent across concurrent controllers and processes.
            let existed_before_claim =
                agent_task_lifecycle::run_record_exists_in_store(lifecycle_store, retry_run_id)?;
            let lease_started_at = chrono::Utc::now();
            let launcher_pid = std::process::id();
            let launcher_start_identity =
                homeboy_core::process::process_start_identity(launcher_pid)
                    .map_err(Error::internal_unexpected)?
                    .ok_or_else(|| {
                        Error::internal_unexpected(
                            "local Cook retry launcher exited before its reservation was persisted",
                        )
                    })?;
            let reserved =
                agent_task_lifecycle::retry_with_force_and_metadata_and_preflight_in_store(
                    lifecycle_store,
                    &source.run_id,
                    Some(retry_run_id),
                    force,
                    local_cook_retry_reservation_metadata(
                        &retry.cook_id,
                        retry_run_id,
                        lease_started_at,
                        launcher_pid,
                        launcher_start_identity,
                    ),
                    preflight,
                )?;
            let result = json!({ "run_id": retry_run_id });
            if let Err(error) = agent_task_lifecycle::complete_cook_operation_in_store(
                lifecycle_store,
                &source.run_id,
                &operation_key,
                result.clone(),
            ) {
                let recovered = agent_task_lifecycle::find_unbound_cook_retry_successor_in_store(
                    lifecycle_store,
                    &source.run_id,
                    &retry.cook_id,
                    retry.attempt,
                    &retry.plan,
                )?;
                if recovered.as_ref().map(|record| record.run_id.as_str()) != Some(retry_run_id) {
                    return Err(error);
                }
                agent_task_lifecycle::recover_completed_cook_operation_in_store(
                    lifecycle_store,
                    &source.run_id,
                    &operation_key,
                    result,
                )?;
            }
            Ok(CookRetryReservation {
                run_id: retry_run_id.to_string(),
                created: !existed_before_claim && reserved.run_id == retry_run_id,
            })
        }
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            let recorded_run_id = result["run_id"].as_str().ok_or_else(|| {
                Error::internal_unexpected("completed Cook retry claim has no run id")
            })?;
            Ok(CookRetryReservation {
                run_id: recorded_run_id.to_string(),
                created: false,
            })
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            // The winner writes its lifecycle reservation before completing the
            // claim. Re-read through the indexed successor path on the next
            // retry rather than allocating a competing run id.
            // Controller admission can take up to the normal local lease
            // handoff window. Bound the observer wait above that window so a
            // crashed winner remains recoverable rather than waiting forever.
            for _ in 0..2_000 {
                if let Some(record) =
                    agent_task_lifecycle::find_unbound_cook_retry_successor_in_store(
                        lifecycle_store,
                        &source.run_id,
                        &retry.cook_id,
                        retry.attempt,
                        &retry.plan,
                    )?
                {
                    return Ok(CookRetryReservation {
                        run_id: record.run_id,
                        created: false,
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(Error::validation_invalid_argument(
                "run_id",
                "Cook retry reservation is still being finalized",
                Some(source.run_id.clone()),
                None,
            ))
        }
    }
}

/// A manually approved retry of a Cook-owned failure with no candidate evidence
/// remains an append-only Cook attempt. The durable recipe authenticates source
/// ownership; the Cook index prevents a failed provider retry from superseding
/// an already-promotable sibling candidate.
fn retryable_cook_attempt(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<CookRetryAttempt>> {
    let Some(cook_id) = source.metadata["cook_id"].as_str() else {
        return Ok(None);
    };
    let Some(source_attempt) = source.metadata["cook_attempt"].as_u64() else {
        return Ok(None);
    };
    let attempt = u32::try_from(source_attempt).map_err(|_| {
        Error::validation_invalid_argument(
            "cook_attempt",
            "durable Cook attempt number exceeds the supported range",
            Some(source.run_id.clone()),
            None,
        )
    })?;
    if !super::recipe_exists(cook_id)? {
        // Legacy runs predate durable recipes. Retain their established generic
        // lifecycle retry behavior rather than inventing Cook ownership.
        return Ok(None);
    }
    let recipe = super::load_recipe(cook_id)?;
    let source_recipe_attempt = recipe.attempts.iter().find(|recipe_attempt| {
        recipe_attempt.attempt == attempt && recipe_attempt.run_id == source.run_id
    });
    let Some(source_recipe_attempt) = source_recipe_attempt else {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "retryable pre-provider failure is not owned by its durable Cook recipe",
            Some(source.run_id.clone()),
            Some(vec![format!(
                "Continue the owning Cook with: {}",
                super::cook_continue_command(None, &source.run_id, false, None)
            )]),
        ));
    };
    agent_task_lifecycle::admit_lab_pre_execution_replay(source, &source_recipe_attempt.plan)?;
    let retryable_pre_execution_failure =
        source.metadata["pre_execution_failure"]["retryable"] == serde_json::Value::Bool(true);
    if source.metadata["pre_execution_failure"].is_object() && !retryable_pre_execution_failure {
        return Ok(None);
    }
    let acceptance_repair = source.acceptance.as_ref().is_some_and(|acceptance| {
        acceptance.verdict == agent_task_lifecycle::AgentTaskAcceptanceVerdict::Rejected
            && acceptance.repair_attempts == 1
            && source.metadata["acceptance_repair"]["feedback"]
                .as_str()
                .is_some_and(|feedback| !feedback.is_empty())
    });
    let failed_provider_without_candidate = matches!(
        source.state,
        agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::Cancelled
    ) && !retryable_pre_execution_failure
        && agent_task_lifecycle::select_cook_candidate_in_store(lifecycle_store, cook_id)
            .ok()
            .is_some_and(|selection| {
                selection.run_id == source.run_id && selection.selected_artifact_id.is_none()
            });
    let source_is_retryable =
        retryable_pre_execution_failure || failed_provider_without_candidate || acceptance_repair;
    let replaces_source_attempt = source.metadata["pre_execution_failure"].is_object()
        && source.metadata["provider_executions_consumed"]
            .as_u64()
            .unwrap_or_default()
            == 0;
    if replaces_source_attempt
        && !recipe
            .attempts
            .iter()
            .any(|recipe_attempt| recipe_attempt.attempt > attempt)
    {
        return Ok(Some(CookRetryAttempt {
            cook_id: cook_id.to_string(),
            attempt,
            pending_run_id: None,
            plan: source_recipe_attempt.plan.clone(),
            recipe_replacement: false,
            replaces_source_attempt: true,
        }));
    }
    let mut pending_attempt = None;
    let mut materialized_attempt_seen = false;
    for recipe_attempt in recipe
        .attempts
        .iter()
        .filter(|recipe_attempt| recipe_attempt.attempt == attempt.saturating_add(1))
    {
        if !agent_task_lifecycle::run_record_exists_in_store(
            lifecycle_store,
            &recipe_attempt.run_id,
        )? {
            if materialized_attempt_seen {
                pending_attempt = Some(recipe_attempt);
                break;
            }
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry recipe entry has no durable lifecycle reservation",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        let mut record =
            agent_task_lifecycle::exact_record_in_store(lifecycle_store, &recipe_attempt.run_id)?;
        let mut owned_replacement = materialized_attempt_seen
            && record.metadata["cook_id"] == cook_id
            && record.metadata["cook_attempt"] == recipe_attempt.attempt;
        if materialized_attempt_seen
            && !owned_replacement
            && record.state == agent_task_lifecycle::AgentTaskRunState::Queued
        {
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(10));
                record = agent_task_lifecycle::exact_record_in_store(
                    lifecycle_store,
                    &recipe_attempt.run_id,
                )?;
                owned_replacement = record.metadata["cook_id"] == cook_id
                    && record.metadata["cook_attempt"] == recipe_attempt.attempt;
                if owned_replacement {
                    break;
                }
            }
        }
        if !owned_replacement && record.metadata["retry_of"] != source.run_id {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run is not the durable retry of its source attempt",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        if !cook_retry_plans_match(
            &recipe_attempt.plan,
            &agent_task_lifecycle::load_plan_in_store(lifecycle_store, &recipe_attempt.run_id)?,
        ) {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run does not match its durable plan",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        if record.state.is_terminal() {
            if record.state == agent_task_lifecycle::AgentTaskRunState::Succeeded {
                return Ok(Some(CookRetryAttempt {
                    cook_id: cook_id.to_string(),
                    attempt: recipe_attempt.attempt,
                    pending_run_id: Some(recipe_attempt.run_id.clone()),
                    plan: recipe_attempt.plan.clone(),
                    recipe_replacement: materialized_attempt_seen,
                    replaces_source_attempt: false,
                }));
            }
            materialized_attempt_seen = true;
            // A terminal failed successor is authoritative evidence. It cannot
            // be resumed; a later attempt may be allocated below if the durable
            // retry budget permits it.
            continue;
        }
        if record.state != agent_task_lifecycle::AgentTaskRunState::Queued {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "pending Cook retry run is already active and must be resumed through its lifecycle",
                Some(recipe_attempt.run_id.clone()),
                None,
            ));
        }
        pending_attempt = Some(recipe_attempt);
        break;
    }
    if let Some(pending_attempt) = pending_attempt {
        return Ok(Some(CookRetryAttempt {
            cook_id: cook_id.to_string(),
            attempt: pending_attempt.attempt,
            pending_run_id: Some(pending_attempt.run_id.clone()),
            plan: pending_attempt.plan.clone(),
            recipe_replacement: materialized_attempt_seen,
            replaces_source_attempt: false,
        }));
    }
    if !source_is_retryable {
        return Ok(None);
    }
    let max_attempts = recipe
        .retry_budget
        .get("max_attempts")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.retry_budget.max_attempts",
                "durable Cook recipe is missing a valid max_attempts budget",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    let next_attempt = recipe
        .attempts
        .iter()
        .map(|recipe_attempt| recipe_attempt.attempt)
        .max()
        .unwrap_or(attempt)
        .checked_add(1)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable Cook attempt sequence is exhausted",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    if next_attempt > max_attempts {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.retry_budget.max_attempts",
            "manual retry would exceed the durable Cook attempt budget",
            Some(cook_id.to_string()),
            None,
        ));
    }
    let mut plan = source_recipe_attempt.plan.clone();
    if acceptance_repair {
        let feedback = source.metadata["acceptance_repair"]["feedback"]
            .as_str()
            .expect("acceptance repair feedback was checked above");
        for task in &mut plan.tasks {
            task.instructions.push_str(&format!(
                "\n\nAddress this reviewer remediation feedback, then preserve the Cook's normal verification and review-form contract:\n{feedback}"
            ));
            task.inputs["cook_loop"]["reviewer_remediation"] = serde_json::json!({
                "source_run_id": source.run_id,
                "feedback": feedback,
                "max_attempts": 1,
            });
        }
    }
    Ok(Some(CookRetryAttempt {
        cook_id: cook_id.to_string(),
        attempt: next_attempt,
        pending_run_id: None,
        plan,
        recipe_replacement: false,
        replaces_source_attempt: false,
    }))
}

fn is_exact_retry_reservation(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    run_id: &str,
) -> Result<bool> {
    let record = agent_task_lifecycle::exact_record_in_store(lifecycle_store, run_id)?;
    Ok(record.metadata["retry_of"] == source.run_id
        && cook_retry_plans_match(
            plan,
            &agent_task_lifecycle::load_plan_in_store(lifecycle_store, run_id)?,
        ))
}

/// A pending local retry has already passed recipe binding and has not yet
/// materialized a child. Its pinned launcher lease is durable retry proof even
/// when a restarted caller rebuilds a plan with non-execution metadata changed.
fn is_pending_local_cook_retry_reservation(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    run_id: &str,
) -> Result<bool> {
    let record = agent_task_lifecycle::exact_record_in_store(lifecycle_store, run_id)?;
    let supervisor = &record.metadata["local_cook_supervisor"];
    Ok(
        record.state == agent_task_lifecycle::AgentTaskRunState::Queued
            && record.metadata["retry_of"] == source.run_id
            && matches!(
                supervisor["state"].as_str(),
                Some("pending") | Some("child_spawned")
            )
            && supervisor["pinned_run_id"] == run_id
            && supervisor["launcher_pid"].as_u64().is_some()
            && !supervisor["launcher_process_start_identity"].is_null(),
    )
}

pub(super) fn cook_retry_plans_match(expected: &AgentTaskPlan, observed: &AgentTaskPlan) -> bool {
    if expected == observed {
        return true;
    }
    if expected.tasks.len() != observed.tasks.len() {
        return false;
    }

    let mut normalized = observed.clone();
    for (expected_task, observed_task) in expected.tasks.iter().zip(&mut normalized.tasks) {
        let expected_identity = expected_task.metadata.get("cook_workspace_identity");
        if observed_task.metadata.get("cook_workspace_identity") == expected_identity {
            continue;
        }
        if observed_task
            .metadata
            .get("cook_workspace_identity_predecessor")
            != expected_identity
        {
            return false;
        }
        let Some(expected_identity) = expected_identity else {
            return false;
        };
        let Some(metadata) = observed_task.metadata.as_object_mut() else {
            return false;
        };
        metadata.insert(
            "cook_workspace_identity".to_string(),
            expected_identity.clone(),
        );
        metadata.remove("cook_workspace_identity_predecessor");
    }
    normalized == *expected
}

struct CookRetryAttempt {
    cook_id: String,
    attempt: u32,
    pending_run_id: Option<String>,
    plan: AgentTaskPlan,
    recipe_replacement: bool,
    replaces_source_attempt: bool,
}

/// Verify retry admission before advertising a retry as executable.
///
/// Recipe-backed Cook runs cannot fall back to generic lifecycle retry because
/// that would lose the Cook's authenticated lineage.
pub fn retry_admission(run_id: &str) -> Result<()> {
    retry_admission_with_preflight(run_id, |plan| {
        if plan.metadata.get("generic_lab_command_replay").is_some() {
            return Err(Error::validation_invalid_argument(
                "generic_lab_command_replay",
                "generic Lab replay requires controller workspace preflight",
                Some(plan.plan_id.clone()),
                None,
            ));
        }
        Ok(())
    })
}

/// Verify retry admission using the caller's execution-specific preflight.
pub fn retry_admission_with_preflight<F>(run_id: &str, preflight: F) -> Result<()>
where
    F: Fn(&AgentTaskPlan) -> Result<()>,
{
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let source = agent_task_lifecycle::normalize_local_execution_placement_in_store(
        &lifecycle_store,
        run_id,
    )?;
    let retry = retry_admission_in_store(&lifecycle_store, &source, true)?;
    if let Some(retry) = retry {
        preflight(&retry.plan)?;
    }
    Ok(())
}

fn retry_admission_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    source: &agent_task_lifecycle::AgentTaskRunRecord,
    require_latest_attempt: bool,
) -> Result<Option<CookRetryAttempt>> {
    let has_cook_ownership = source
        .metadata
        .get("cook_id")
        .and_then(serde_json::Value::as_str)
        .map(super::recipe_exists)
        .transpose()?
        .unwrap_or(false);
    let retry = retryable_cook_attempt(lifecycle_store, source)?;
    if require_latest_attempt && has_cook_ownership {
        let cook_id = source.metadata["cook_id"]
            .as_str()
            .expect("recipe-backed Cook ownership has a cook id");
        if super::load_recipe(cook_id)?
            .attempts
            .last()
            .map(|attempt| attempt.run_id.as_str())
            != Some(source.run_id.as_str())
        {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "only the latest durable Cook attempt can be retried",
                Some(source.run_id.clone()),
                None,
            ));
        }
    }
    if has_cook_ownership && retry.is_none() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "Cook-owned run is not eligible for a durable Cook retry",
            Some(source.run_id.clone()),
            None,
        ));
    }
    Ok(retry)
}

#[derive(Debug, Clone)]
pub struct AgentTaskRetryServiceResult {
    pub record: AgentTaskRunRecord,
    pub run: bool,
    /// True only for the caller that durably reserved this Cook successor.
    pub created: bool,
}

/// Reconcile status with explicit control over whether the operation may reach
/// the runner.
///
/// Read-only inspection must stay answerable while the Lab is wedged (#10418).
pub fn reconcile_status_with_options(
    run_id: &str,
    options: agent_task_lifecycle::AgentTaskStatusOptions,
) -> Result<agent_task_lifecycle::AgentTaskStatusOutcome> {
    agent_task_lifecycle::reconcile_status_with_options(run_id, options)
}

/// Return the canonical non-reconciling control-plane run resource.
pub fn control_plane_run(run_id: &str) -> Result<homeboy_control_plane_contract::ControlPlaneRun> {
    crate::orchestration::run_from_current_environment(run_id)
}

/// Resolve the durable substantive candidate for a logical Cook reader.
pub fn select_cook_candidate(
    cook_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskCookCandidateSelection> {
    agent_task_lifecycle::select_cook_candidate(cook_id)
}

pub fn logs(run_id: &str) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    agent_task_lifecycle::logs(run_id)
}

pub fn logs_from_cursor(
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    agent_task_lifecycle::logs_from_cursor(run_id, cursor)
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    agent_task_lifecycle::artifacts(run_id)
}

pub fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn run_claimed(
    run_id: String,
    executor: SharedAgentTaskExecutor,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    let mut plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
    if let Err(error) = prepare_plan_for_execution(&mut plan, Some(&run_id)) {
        agent_task_lifecycle::record_pre_execution_failure(
            &run_id,
            &plan,
            "prepare_plan_for_execution",
            &error,
        )?;
        return Err(error);
    }
    let harvest_context =
        match crate::agent_task_scheduler::HarvestExecutionContext::from_current_process() {
            Ok(context) => context,
            Err(error) => {
                agent_task_lifecycle::record_pre_execution_failure(
                    &run_id,
                    &plan,
                    "validate_harvest_transport",
                    &error,
                )?;
                return Err(error);
            }
        };
    run_prepared_claimed(run_id, plan, executor, harvest_context)
}

fn run_prepared_claimed(
    run_id: String,
    plan: AgentTaskPlan,
    executor: SharedAgentTaskExecutor,
    harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>> {
    let aggregate = run_plan_with_scheduler(
        None,
        plan.clone(),
        Some(&run_id),
        executor,
        None,
        harvest_context,
    )?;
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

fn prepare_plan_for_execution(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    prepare_plan_workspaces(plan, run_id)?;
    apply_provider_runner_secret_env_contracts(plan);
    preflight_plan_secret_env(plan)
}

fn prepare_plan_workspaces(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    for request in &mut plan.tasks {
        prepare_component_worktree_workspace(request, run_id)?;
    }

    Ok(())
}

pub(crate) fn preflight_plan_secret_env(plan: &AgentTaskPlan) -> Result<()> {
    let mut secret_env_plan = SecretEnvPlan::from_secret_env_names(
        plan.tasks
            .iter()
            .flat_map(|task| task.executor.secret_env.iter().cloned()),
    );
    // Service secrets are execution-host requirements too. Validate them before
    // any Lab handoff or local supervisor spawn, never after a listener/lease
    // has been admitted.
    for service in &plan.services {
        if let Some(service_plan) = &service.secret_env_plan {
            secret_env_plan.merge_from(service_plan.clone());
        } else {
            secret_env_plan.extend_secret_env_names(service.secret_env.clone());
        }
    }

    validate_secret_env_with_fallbacks(
        &secret_env_plan.secret_env_names(),
        &provider_secret_sources_for_plan(plan),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "secret_env",
            error.message,
            None,
            Some(vec![
                "Agent-task executor provider manifests can declare runner-required secret env contracts; Homeboy validates those contracts before task execution.".to_string(),
                "For local execution, configure provider credentials with `homeboy agent-task auth map-env`, `set-keychain`, or `set-keychain-bundle`.".to_string(),
                "For delegated runner execution, configure the selected runner's secret_env references so the runner receives these names without printing values.".to_string(),
            ]),
        )
    })
}

/// Queue admission validates provider eligibility and credential provenance
/// before a record is claimed Running. Workspace preparation remains after the
/// claim because it creates controller-owned filesystem state.
fn preflight_queued_plan_provider_eligibility(_plan: &AgentTaskPlan) -> Result<()> {
    // Queue admission must not turn a short-lived negative probe into a durable
    // exclusion or skip. The scheduler evaluates the full chain after claim and
    // can therefore emit a terminal aggregate with zero execution usage.
    Ok(())
}

fn run_plan_with_scheduler(
    lifecycle_store: Option<&agent_task_lifecycle::AgentTaskLifecycleStore>,
    plan: AgentTaskPlan,
    run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
    derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
) -> Result<AgentTaskAggregate> {
    let scheduler =
        AgentTaskScheduler::new_controller(executor).with_harvest_context(harvest_context);
    let scheduler = match lifecycle_store {
        Some(store) => scheduler.with_lifecycle_store(store.clone()),
        None => scheduler,
    };
    match run_id {
        Some(run_id) => Ok(scheduler
            .with_run_id(run_id.to_string())
            .run_with_derived_cook_baseline(plan, derived_cook_baseline)),
        None => Ok(scheduler.run_with_derived_cook_baseline(plan, derived_cook_baseline)),
    }
}

pub fn aggregate_exit_code(aggregate: &AgentTaskAggregate) -> i32 {
    if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    }
}

fn normalize_component_worktree_workspace(request: &mut AgentTaskRequest) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(Error::validation_invalid_argument(
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
        return Ok(());
    };

    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
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

fn prepare_component_worktree_workspace(
    request: &mut AgentTaskRequest,
    run_id: Option<&str>,
) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }
    if request.workspace.root.is_some()
        || materialization_string(&request.workspace.materialization, "root").is_some()
        || materialization_string(&request.workspace.materialization, "resolved_root").is_some()
    {
        return normalize_component_worktree_workspace(request);
    }

    let component_id = request.workspace.component_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        )
    })?;
    let branch = request.workspace.branch.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.branch",
            format!(
                "agent-task task '{}' component-worktree workspace for component '{}' requires branch",
                request.task_id, component_id
            ),
            None,
            None,
        )
    })?;
    let cleanup_policy = cleanup_policy_for_workspace(request.workspace.cleanup.as_deref());
    let task_url = request.workspace.task_url.clone().or_else(|| {
        request
            .source_refs
            .iter()
            .find(|source| source.kind == "task")
            .or_else(|| request.source_refs.first())
            .map(source_uri)
    });
    let created = worktree_provider::create_worktree(worktree::WorktreeCreateOptions {
        component_id: component_id.clone(),
        branch,
        from: request.workspace.base_ref.clone(),
        task_url,
        run_id: run_id.map(str::to_string),
        cleanup_policy: cleanup_policy.clone(),
        require_handoff_freshness: false,
    })?;
    let (root, cleanup, materialization) = match created {
        worktree_provider::WorktreeProviderCreateOutput::Native(created) => {
            let record = created.record;
            let cleanup = cleanup_lifecycle_policy(&record.cleanup_policy).to_string();
            let root = record.worktree_path.clone();
            let materialization = serde_json::json!({
                "kind": "homeboy-worktree",
                "id": record.id,
                "component_id": record.component_id,
                "branch": record.branch,
                "base_ref": record.base_ref,
                "root": record.worktree_path,
                "source_checkout": record.source_checkout,
                "task_url": record.task_url,
                "run_id": record.run_id,
                "cleanup_policy": cleanup.clone(),
            });
            (root, cleanup, materialization)
        }
        worktree_provider::WorktreeProviderCreateOutput::Configured(provision) => {
            let cleanup_policy =
                cleanup_policy.unwrap_or(worktree::CleanupPolicy::PreserveOnFailure);
            let cleanup = cleanup_lifecycle_policy(&cleanup_policy).to_string();
            let evidence = worktree_provider::ConfiguredWorktreeCreateEvidence::from(provision);
            let root = evidence.path.clone();
            let materialization = serde_json::json!({
                "kind": "worktree-provider",
                "provider": evidence.provider,
                "id": evidence.handle,
                "component_id": component_id.clone(),
                "branch": evidence.branch,
                "root": evidence.path,
                "task_url": evidence.task_url,
                "run_id": run_id,
                "cleanup_policy": cleanup.clone(),
                "provision_action": evidence.provision_action,
                "idempotency_key": evidence.idempotency_key,
            });
            (root, cleanup, materialization)
        }
    };
    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(root);
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = Some(cleanup);
    request.workspace.materialization = materialization;

    Ok(())
}

fn cleanup_policy_for_workspace(value: Option<&str>) -> Option<worktree::CleanupPolicy> {
    match value {
        Some("remove_when_safe") | Some("remove-when-safe") | Some("cleanup") => {
            Some(worktree::CleanupPolicy::RemoveWhenSafe)
        }
        Some("preserve") | Some("preserve_on_failure") | Some("preserve-on-failure") => {
            Some(worktree::CleanupPolicy::PreserveOnFailure)
        }
        _ => None,
    }
}

fn cleanup_lifecycle_policy(policy: &worktree::CleanupPolicy) -> &'static str {
    match policy {
        worktree::CleanupPolicy::RemoveWhenSafe => "remove_when_safe",
        worktree::CleanupPolicy::PreserveOnFailure => "preserve",
    }
}

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AgentTaskWorkspaceMode,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_scheduler::{
        AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskManagedService,
        AgentTaskManagedServiceLifecycle, AgentTaskPlan, HarvestExecutionContext,
    };

    #[derive(Clone)]
    struct SuccessfulExecutor;

    impl AgentTaskExecutorAdapter for SuccessfulExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if let Some(root) = request.workspace.root.as_deref() {
                std::fs::write(
                    Path::new(root).join("rooted-change.txt"),
                    format!("change from {}\n", request.task_id),
                )
                .expect("write provider change");
            }
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("succeeded".to_string()),
                metadata: serde_json::json!({}),
                ..Default::default()
            }
        }
    }

    fn one_task_plan(plan_id: &str, workspace: &Path) -> AgentTaskPlan {
        AgentTaskPlan::new(
            plan_id,
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "same-task".to_string(),
                group_key: None,
                parent_plan_id: Some(plan_id.to_string()),
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "execute the rooted task".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace {
                    mode: AgentTaskWorkspaceMode::Existing,
                    root: Some(workspace.display().to_string()),
                    ..Default::default()
                },
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                runtime_tools: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    #[test]
    fn validate_plan_rejects_unsupported_plan_schema_with_structured_failure() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut plan = one_task_plan("schema-plan", workspace.path());
        plan.schema = "homeboy/agent-task-plan/v999".to_string();

        let report = validate_plan_spec(&serde_json::to_string(&plan).expect("plan JSON"));

        assert!(!report.valid);
        assert_eq!(report.plan_id.as_deref(), Some("schema-plan"));
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            report.failures[0].kind,
            AgentTaskPlanValidationKind::InvalidInput
        );
        assert_eq!(report.failures[0].code, "validation.invalid_argument");
    }

    #[test]
    fn timeout_override_changes_only_effective_timeouts_and_appends_history() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut plan = one_task_plan("timeout-override", workspace.path());
        plan.options.timeout_ms = Some(100);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(4, 3, 2);
        let mut second = plan.tasks[0].clone();
        second.task_id = "second-task".to_string();
        second.limits.timeout_ms = Some(200);
        plan.tasks.push(second);
        plan.metadata["cook_timeout_overrides"] = json!([{
            "schema": "homeboy/agent-task-cook-timeout-override/v1",
            "source_run_id": "older-run",
            "previous_timeout_ms": 50,
            "timeout_ms": 100,
            "authority": "operator --timeout-ms"
        }]);
        let original_budget = plan.options.execution_budget.clone();

        apply_timeout_override_to_plan(&mut plan, "timed-out-run", 200, 400, 3, 2, 1, false)
            .expect("apply timeout override");

        assert_eq!(plan.options.timeout_ms, Some(400));
        assert!(plan
            .tasks
            .iter()
            .all(|task| task.limits.timeout_ms == Some(400)));
        assert_ne!(plan.options.execution_budget, original_budget);
        assert_eq!(
            plan.options.execution_budget,
            AgentTaskExecutionBudget::new(3, 1, 1)
        );
        let history = plan.metadata["cook_timeout_overrides"]
            .as_array()
            .expect("timeout history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["source_run_id"], "older-run");
        assert_eq!(history[1]["source_run_id"], "timed-out-run");
        assert_eq!(history[1]["previous_timeout_ms"], 200);
        assert_eq!(history[1]["timeout_ms"], 400);
        assert_eq!(history[1]["remaining_provider_executions"], 3);
        assert_eq!(
            history[1]["remaining_same_provider_retries_after_reservation"],
            1
        );
        assert_eq!(history[1]["remaining_provider_rotations"], 1);
        assert_eq!(
            plan.homeboy_plan,
            plan.clone().canonicalize().homeboy_plan,
            "the portable plan projection is rebuilt with the override"
        );
    }

    #[test]
    fn review_form_timeout_override_updates_the_distinct_deadline_and_authority() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut plan = one_task_plan("review-form-timeout-override", workspace.path());
        plan.tasks[0].metadata = json!({
            "cook_loop": {
                "kind": "review_form_only",
                "review_form_timeout_ms": 300_000,
            }
        });

        apply_timeout_override_to_plan(
            &mut plan,
            "timed-out-review-form",
            300_000,
            600_000,
            1,
            1,
            0,
            true,
        )
        .expect("apply review-form timeout override");

        assert_eq!(plan.options.timeout_ms, Some(600_000));
        assert_eq!(plan.tasks[0].limits.timeout_ms, Some(600_000));
        assert_eq!(
            plan.tasks[0].metadata["cook_loop"]["review_form_timeout_ms"],
            600_000
        );
        assert_eq!(
            plan.metadata["cook_loop"]["review_form_timeout_ms"],
            600_000
        );
        let override_record = plan.metadata["cook_timeout_overrides"]
            .as_array()
            .and_then(|history| history.last())
            .expect("timeout override history");
        assert_eq!(
            override_record["authority"],
            "operator --review-form-timeout-ms"
        );
    }

    #[test]
    fn validate_plan_structure_rejects_duplicate_tasks_and_unknown_dependencies() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut duplicate = one_task_plan("duplicate-plan", workspace.path());
        duplicate.tasks.push(duplicate.tasks[0].clone());
        assert!(validate_plan_structure(&duplicate)
            .expect_err("duplicate task")
            .message
            .contains("unique"));

        let mut dependency = one_task_plan("dependency-plan", workspace.path());
        dependency.output_dependencies.insert(
            "same-task".to_string(),
            crate::agent_task_scheduler::AgentTaskOutputDependencies {
                depends_on: vec!["missing-task".to_string()],
                bindings: HashMap::new(),
            },
        );
        assert!(validate_plan_structure(&dependency)
            .expect_err("unknown dependency")
            .message
            .contains("unknown"));
    }

    #[test]
    fn validate_plan_rejects_missing_provider_and_capability_before_readiness() {
        let workspace = tempfile::tempdir().expect("workspace");
        let plan = one_task_plan("provider-plan", workspace.path());
        let empty_catalog = crate::agent_task_provider::AgentTaskProviderCatalog::default();
        let missing_provider = validate_plan_provider_capabilities(&plan, &empty_catalog)
            .expect_err("missing provider");
        assert_eq!(
            missing_provider.code,
            homeboy_core::ErrorCode::RunnerCapabilityMissing
        );

        let mut provider: crate::agent_task_provider::AgentTaskExecutorProvider =
            serde_json::from_value(serde_json::json!({
                "id": "test-provider",
                "backend": "test",
                "capabilities": []
            }))
            .expect("provider");
        provider.capabilities.clear();
        let catalog = crate::agent_task_provider::AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        };
        let mut plan = plan;
        plan.tasks[0]
            .executor
            .required_capabilities
            .push("workspace_write".to_string());
        let missing_capability =
            validate_plan_provider_capabilities(&plan, &catalog).expect_err("missing capability");
        assert_eq!(
            missing_capability.code,
            homeboy_core::ErrorCode::RunnerCapabilityMissing
        );
        assert_eq!(
            missing_capability.details["missing_capabilities"][0],
            "workspace_write"
        );
    }

    fn initialize_workspace(path: &Path) {
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("run git fixture command");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--initial-branch=main"]);
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(path.join("base.txt"), "base\n").expect("write base fixture");
        git(&["add", "base.txt"]);
        git(&["commit", "-m", "base"]);
    }

    fn invalid_service() -> AgentTaskManagedService {
        AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "invalid".to_string(),
            command: vec!["fixture".to_string()],
            cwd: None,
            env: HashMap::new(),
            env_allowlist: Vec::new(),
            secret_env: Vec::new(),
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: None,
            port_env: None,
            socket_handoff: false,
            readiness: None,
            cleanup_deadline_ms: 0,
            public_url: None,
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        }
    }

    #[test]
    fn direct_run_plan_read_rejects_an_invalid_managed_service() {
        let mut plan = AgentTaskPlan::new("invalid-service", Vec::new());
        plan.services.push(invalid_service());
        plan.rebuild_homeboy_plan();
        let file = tempfile::NamedTempFile::new().expect("plan file");
        std::fs::write(
            file.path(),
            serde_json::to_vec(&plan).expect("serialize plan"),
        )
        .expect("write plan");

        let spec = format!("@{}", file.path().display());
        let error = read_plan(&spec).expect_err("invalid plan");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert!(error.message.contains("cleanup_deadline_ms"));
    }

    #[test]
    fn local_retry_reservation_is_live_before_recipe_binding() {
        let run_id = "local-retry-reservation";
        let started_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("lease timestamp")
            .with_timezone(&chrono::Utc);
        let launcher_pid = std::process::id();
        let launcher_start_identity = homeboy_core::process::process_start_identity(launcher_pid)
            .expect("inspect launcher identity")
            .expect("launcher is live");
        let metadata = local_cook_retry_reservation_metadata(
            "local-retry-cook",
            run_id,
            started_at,
            launcher_pid,
            launcher_start_identity,
        );
        let record: agent_task_lifecycle::AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": "local-retry-plan",
            "state": "queued",
            "submitted_at": started_at.to_rfc3339(),
            "plan_path": "plan.json",
            "metadata": metadata,
        }))
        .expect("reservation record");

        assert_eq!(record.metadata["cook_id"], "local-retry-cook");
        assert!(record
            .has_live_pending_local_cook_supervisor(started_at + chrono::Duration::seconds(1)));
    }

    /// A supervisor that has begun supervising still owns its run. Admitting
    /// only `pending` reconciled every detached local Cook as ownerless the
    /// moment its lease advanced, cancelling a healthy attempt before it had
    /// published an owner pid (#13692). The lease window remains the bound that
    /// keeps an abandoned lease from protecting a dead run.
    #[test]
    fn supervising_local_cook_lease_still_owns_its_run() {
        let run_id = "local-supervising-reservation";
        let started_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("lease timestamp")
            .with_timezone(&chrono::Utc);
        let launcher_pid = std::process::id();
        let launcher_start_identity = homeboy_core::process::process_start_identity(launcher_pid)
            .expect("inspect launcher identity")
            .expect("launcher is live");
        let mut metadata = local_cook_retry_reservation_metadata(
            "local-supervising-cook",
            run_id,
            started_at,
            launcher_pid,
            launcher_start_identity,
        );
        metadata["local_cook_supervisor"]["state"] = json!("supervising");
        let record: agent_task_lifecycle::AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": "local-supervising-plan",
            "state": "running",
            "submitted_at": started_at.to_rfc3339(),
            "plan_path": "plan.json",
            "metadata": metadata,
        }))
        .expect("supervising record");

        assert!(record
            .has_live_pending_local_cook_supervisor(started_at + chrono::Duration::seconds(1)));

        // The lease window still bounds ownership: once it expires, a
        // supervising state no longer shields the run from reconciliation.
        assert!(!record.has_live_pending_local_cook_supervisor(
            started_at
                + chrono::Duration::seconds(
                    agent_task_lifecycle::LOCAL_COOK_SUPERVISOR_LEASE_SECONDS + 1,
                )
        ));
    }

    #[test]
    fn local_execution_isolates_identical_run_ids_in_explicit_lifecycle_stores() {
        let first = homeboy_core::test_support::HermeticTestContext::new();
        let second = homeboy_core::test_support::HermeticTestContext::new();
        let first_store = agent_task_lifecycle::AgentTaskLifecycleStore::new(first.path_roots());
        let second_store = agent_task_lifecycle::AgentTaskLifecycleStore::new(second.path_roots());
        let first_workspace = tempfile::tempdir().expect("first workspace");
        let second_workspace = tempfile::tempdir().expect("second workspace");
        initialize_workspace(first_workspace.path());
        initialize_workspace(second_workspace.path());
        let run_id = "same-local-follow-up-run";

        for (store, plan_id, workspace) in [
            (
                &first_store,
                "first-local-follow-up",
                first_workspace.path(),
            ),
            (
                &second_store,
                "second-local-follow-up",
                second_workspace.path(),
            ),
        ] {
            let result = run_loaded_plan_with_derived_cook_baseline_in_store(
                store,
                one_task_plan(plan_id, workspace),
                Some(run_id),
                Arc::new(SuccessfulExecutor),
                None,
                Some(HarvestExecutionContext::default()),
            )
            .expect("execute through the explicit lifecycle store");
            assert_eq!(result.exit_code, 0);

            let record = store.read_record(run_id).expect("rooted terminal record");
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
            );
            assert_eq!(
                record.metadata["provider_executions"][0]["state"],
                "succeeded"
            );
            assert!(
                record.metadata["provider_executions"][0]["post_provider_cleanup_finished_at"]
                    .is_string()
            );
            let aggregate = store.read_aggregate(run_id).expect("rooted aggregate");
            let patch_path = aggregate.outcomes[0].artifacts[0]
                .path
                .as_deref()
                .map(Path::new)
                .expect("harvested patch path");
            assert!(patch_path.starts_with(store.artifact_root()));
            assert!(patch_path.exists());
            let scratch_index = store.data_root().join("controller-scratch/resources.json");
            let scratch = std::fs::read_to_string(scratch_index).expect("rooted scratch index");
            assert!(scratch.contains(run_id));
        }

        assert_ne!(first_store.run_dir(run_id), second_store.run_dir(run_id));
    }
}
