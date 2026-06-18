//! Stable facade for agent task orchestration APIs.
//!
//! New command and integration code MUST import agent task contracts from this
//! module instead of reaching into the underlying implementation files
//! (`core::agent_task`, `core::agent_task_lifecycle`, `core::agent_task_service`,
//! etc.). The implementation modules remain public for backward compatibility
//! with existing external callers (see `core/mod.rs`), but new code should
//! depend on the explicit API groups defined here so that internal layout can
//! evolve without becoming accidental public contract.
//!
//! The exports are organised into nested API groups by operation:
//!
//! - top-level: stable request/outcome/workflow contracts that callers reach
//!   for most often.
//! - [`aggregate`]: aggregate reports and matrix/reconciliation types.
//! - [`cook_loop`]: cook-loop evaluation contracts.
//! - [`fanout`]: matrix/fanout scheduling primitives.
//! - [`finalization`]: PR finalization contracts and backends.
//! - [`gate`]: gate report contracts and visibility/reveal policies.
//! - [`lifecycle`]: durable run record lifecycle helpers.
//! - [`loop_controller`]: cook/agent-task loop controller state contracts.
//! - [`promotion`]: promotion-report contracts and entry points.
//! - [`provider`]: executor provider contracts used by extensions.
//! - [`scheduler`]: scheduling/plan/concurrency primitives.
//! - [`secrets`]: secret-env mapping helpers.
//! - [`service`]: high-level service entry points combining lifecycle and
//!   scheduling.

// ----------------------------------------------------------------------------
// Stable top-level contracts
// ----------------------------------------------------------------------------
//
// These names are intentionally re-exported at the facade root because they
// form the most common surface for callers (request envelopes, outcomes, the
// workspace contract, schema identifiers, matrix expansion, fanout aggregates).
// Adding a new name here is an intentional API decision.

pub use super::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutionHandle,
    AgentTaskExecutionState, AgentTaskExecutor, AgentTaskExecutorCapabilities,
    AgentTaskFailureClassification, AgentTaskFollowUp, AgentTaskLimits, AgentTaskMatrixAggregate,
    AgentTaskMatrixAggregateCell, AgentTaskMatrixAxis, AgentTaskMatrixCell, AgentTaskMatrixError,
    AgentTaskMatrixPlan, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy,
    AgentTaskPreparedWorkspace, AgentTaskProgress, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskStart, AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence,
    AgentTaskWorkflowStepStatus, AgentTaskWorkflowStepSuggestion, AgentTaskWorkspace,
    AgentTaskWorkspaceMode, AgentToolExecutionLocation, AgentToolPolicy, AgentToolPolicyRule,
    AgentToolRequest, AgentToolResult, AgentToolResultStatus, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_MATRIX_AGGREGATE_SCHEMA, AGENT_TASK_MATRIX_PLAN_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA, AGENT_TOOL_POLICY_SCHEMA,
    AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};

pub use super::agent_task_aggregate::{
    AgentTaskAggregateReport, AgentTaskAggregateSummary, AgentTaskArtifactInventoryItem,
    AgentTaskDecisionRef, AgentTaskMatrixRow, AgentTaskReconciliationDecision,
    AgentTaskReconciliationItem, AGENT_TASK_AGGREGATE_SCHEMA,
};

pub use super::agent_task_contract::{
    agent_task_core_contract, AgentTaskCoreContract, AgentTaskCoreContractEnums,
    AgentTaskCoreContractSchemas, AgentTaskCoreProviderCapabilityContract,
    AgentTaskCoreRedactionDefaults, AGENT_TASK_CORE_CONTRACT_SCHEMA,
};

pub use super::agent_task_fanout::{
    AgentTaskFanoutAggregate, AgentTaskFanoutPlan, AgentTaskFanoutPlane, AgentTaskFanoutScheduler,
    AGENT_TASK_FANOUT_AGGREGATE_SCHEMA, AGENT_TASK_FANOUT_PLAN_SCHEMA,
};

// Plan/scheduler/execution context types are widely consumed and stay at the
// facade root for ergonomics.
pub use super::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
    AgentTaskArtifactOutputDeclaration, AgentTaskBackpressureStatus, AgentTaskCancellationToken,
    AgentTaskExecutionContext, AgentTaskOutputBinding, AgentTaskOutputDependencies, AgentTaskPlan,
    AgentTaskQueueStatus, AgentTaskResourceBudget, AgentTaskResourceBudgetStatus,
    AgentTaskResourcePressure, AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
    AGENT_TASK_PLAN_SCHEMA,
};

// `AgentTaskProgressEvent` is defined in both `agent_task` and
// `agent_task_schedule`. Historically the wildcard facade picked whichever the
// glob resolved last; the canonical type for orchestration callers is the
// schedule-side variant, so name it explicitly here.
pub use super::agent_task_schedule::AgentTaskProgressEvent;

pub use super::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskScheduler};

pub use super::agent_tool_control_plane::{
    dispatch_agent_tool_request, AgentToolControlPlaneDispatcher, AgentToolDispatchEvidence,
    AgentToolDispatchOutcome, UnsupportedAgentToolControlPlaneDispatcher,
    AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA,
};

// Matrix expansion is `pub(crate)` on the implementation module; expose it
// through the facade for callers inside the crate that need to expand a plan
// matrix without depending on the implementation path.
pub(crate) use super::agent_task::expand_agent_task_matrix;

// Convenience re-exports of the loop-controller state enum and lineage record
// that appear on the loop-controller surface and the durable run surface.
pub use super::agent_task_loop_controller::{
    AgentTaskLoopControllerState, AgentTaskLoopTaskLineage,
};
pub use super::agent_task_loop_definition::{
    compile_loop_definition, AgentTaskLoopDefinition, AgentTaskLoopDefinitionTask,
    AGENT_TASK_LOOP_DEFINITION_SCHEMA,
};

// Secret-env status type is referenced from review/dispatch commands.
pub use super::agent_task_secrets::{
    resolve_secret_env_plan, secret_env_plan_status, secret_env_status,
    secret_env_status_with_fallbacks, AgentTaskSecretEnvStatus,
};
pub use super::secret_env_plan::{
    SecretEnvCredentialSource, SecretEnvPlan, SecretEnvProviderCredentialMapping,
    SecretEnvRedactionPolicy, SECRET_ENV_PLAN_SCHEMA,
};

// Provider helpers used directly from the facade root for common callers.
pub use super::agent_task_provider::{
    provider_secret_sources_for_discovered_providers, required_extension_ids_for_plan,
};

// ----------------------------------------------------------------------------
// Explicit API groups
// ----------------------------------------------------------------------------
//
// Each submodule below exposes the intentional surface of one implementation
// area. Callers can import either the top-level names above or use the group
// modules to disambiguate where contracts overlap (e.g. `lifecycle::status`
// vs `service::status`).

/// Cook-loop evaluation contracts and entry points.
/// Durable controller execution service entry points and report contracts.
pub mod controller_service {
    pub use super::super::agent_task_controller_service::{
        apply_event, init, init_from_spec, list, mark_human_ready, optional_bool, optional_string,
        optional_string_array, optional_u32, optional_usize, plan_from_controller_request,
        plan_from_spec, resume, run_action, run_next, status, AgentTaskRepoLoopSpec,
        AgentTaskRepoLoopSpecAbility, AgentTaskRepoLoopSpecAgent, AgentTaskRepoLoopSpecArtifact,
        AgentTaskRepoLoopSpecDependency, AgentTaskRepoLoopSpecEntity, AgentTaskRepoLoopSpecEvent,
        AgentTaskRepoLoopSpecGate, AgentTaskRepoLoopSpecMetric, AgentTaskRepoLoopSpecPhase,
        AgentTaskRepoLoopSpecTool, AgentTaskRepoLoopSpecWorkflow, ControllerActionReport,
        ControllerApplyEventRequest, ControllerDispatchHook, ControllerEventReport,
        ControllerFromSpecReport, ControllerFromSpecRequest, ControllerInitRequest,
        ControllerListReport, ControllerMarkHumanReadyRequest, ControllerPlanReport,
        ControllerPlanRequest, ControllerResumeReport, NoopDispatchHook, ACTION_RESULT_SCHEMA,
        APPLY_EVENT_RESULT_SCHEMA, FROM_SPEC_RESULT_SCHEMA, LIST_RESULT_SCHEMA, PLAN_RESULT_SCHEMA,
        RESUME_RESULT_SCHEMA,
    };
}

/// Cook-loop evaluation contracts and entry points.
pub mod cook_loop {
    pub use super::super::agent_task_cook_loop::{
        evaluate_cook_loop, AgentTaskCookLoopGateFailure, AgentTaskCookLoopOptions,
        AgentTaskCookLoopReport, AgentTaskCookLoopStatus, AGENT_TASK_COOK_LOOP_REPORT_SCHEMA,
    };
}

/// Durable dispatch request, plan construction, and execution service.
pub mod dispatch_service {
    pub use super::super::agent_task_dispatch_service::{
        build_dispatch_plan, build_dispatch_plan_with_provider_requirements, dispatch,
        dispatch_with_provider_requirements, resolve_dispatch_request,
        resolve_dispatch_request_with_default, run_cook_command, run_dispatch_command,
        AgentTaskDispatchCommand, AgentTaskDispatchReport, AgentTaskDispatchRequest,
        DISPATCH_RESULT_SCHEMA,
    };
}

/// PR finalization contracts and backends.
pub mod finalization {
    pub use super::super::agent_task_finalization::{
        finalize_pr, finalize_pr_with_backend, AgentTaskGateResult, AgentTaskPrEvidence,
        AgentTaskPrFinalizationBackend, AgentTaskPrFinalizationOptions,
        AgentTaskPrFinalizationReport, AgentTaskPrRef, AgentTaskPrRuntimeGuardrails,
        AgentTaskPrSourceRelationship, AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
        AGENT_TASK_PR_FINALIZATION_SCHEMA,
    };
}

/// Gate report contracts, visibility, and reveal policies.
pub mod gate {
    pub use super::super::agent_task_gate::{
        AgentTaskGateEnvironment, AgentTaskGateEnvironmentVariable, AgentTaskGateFailureEvidence,
        AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateStatus,
        AgentTaskGateVisibility, VerifyGateOptions, AGENT_TASK_GATE_REPORT_SCHEMA,
    };
}

/// Durable run lifecycle: submit, run-record state, log/artifact loaders.
pub mod lifecycle {
    pub use super::super::agent_task_lifecycle::{
        aggregate_source, artifacts, cancel, cancel_run, claim_next_queued_run, list_records,
        load_plan, logs, mark_resuming, mark_running, record_completed_run,
        record_pre_dispatch_failure, record_promotion, record_remote_dispatch_failure,
        record_run_aggregate, retry, run_record_exists, status, submit_plan, AgentTaskArtifactRef,
        AgentTaskPreDispatchFailure, AgentTaskRemoteDispatchFailure, AgentTaskRunArtifacts,
        AgentTaskRunLog, AgentTaskRunProviderHandle, AgentTaskRunRecord, AgentTaskRunState,
        AgentTaskRunTask,
    };
}

/// Cook/agent-task loop controller state, events, and policy.
pub mod loop_controller {
    pub use super::super::agent_task_loop_controller::{
        apply_external_event, controller_status, controller_status_diagnostics,
        controller_status_report, create_controller, list_controllers, load_controller,
        write_controller, AgentTaskGateBundle, AgentTaskGateBundleCheck,
        AgentTaskGateBundleCheckKind, AgentTaskGateBundleResult, AgentTaskGateBundleStatus,
        AgentTaskGateCheckResult, AgentTaskLoopActionDiagnostic, AgentTaskLoopActionStatus,
        AgentTaskLoopArtifactRef, AgentTaskLoopCandidateLoopLimits, AgentTaskLoopCandidatePatch,
        AgentTaskLoopCandidateValidation, AgentTaskLoopCandidateValidationStatus,
        AgentTaskLoopControllerDiagnosticSummary, AgentTaskLoopControllerDiagnostics,
        AgentTaskLoopControllerRecord, AgentTaskLoopControllerState,
        AgentTaskLoopControllerStatusReport, AgentTaskLoopDedupeRecord, AgentTaskLoopEntity,
        AgentTaskLoopExternalEvent, AgentTaskLoopFeedbackArtifact, AgentTaskLoopFeedbackStatus,
        AgentTaskLoopFindingPacket, AgentTaskLoopHistoryEvent, AgentTaskLoopLocalFallbackPolicy,
        AgentTaskLoopPendingActionDiagnostic, AgentTaskLoopPolicy, AgentTaskLoopPolicyAction,
        AgentTaskLoopPolicyActionRecord, AgentTaskLoopProvenanceRef, AgentTaskLoopReviewFinding,
        AgentTaskLoopRunRef, AgentTaskLoopRunnerAvailability, AgentTaskLoopRunnerExecutionTarget,
        AgentTaskLoopRunnerPolicy, AgentTaskLoopRunnerPolicyDecision,
        AgentTaskLoopSubcontrollerRef, AgentTaskLoopTaskLineage, AgentTaskLoopTransition,
        AgentTaskLoopWait, AgentTaskLoopWaitStatus, AGENT_TASK_LOOP_CONTROLLER_SCHEMA,
        AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA,
    };
}

/// Declarative loop definitions compiled into scheduler plans.
pub mod loop_definition {
    pub use super::super::agent_task_loop_definition::{
        compile_loop_definition, AgentTaskLoopDefinition, AgentTaskLoopDefinitionTask,
        AGENT_TASK_LOOP_DEFINITION_SCHEMA,
    };
}

/// Promotion reports and entry point.
pub mod promotion {
    pub use super::super::agent_task_promotion::{
        promote, AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport,
        AgentTaskPromotionNotification, AgentTaskPromotionOptions, AgentTaskPromotionReport,
        AgentTaskPromotionSource, AgentTaskPromotionStatus, AgentTaskPromotionTarget,
        AGENT_TASK_PROMOTION_REPORT_SCHEMA,
    };
}

/// Executor provider contracts used by extensions and routing.
pub mod provider {
    pub use super::super::agent_task_provider::{
        default_backend, default_backend_for_component, dependency_failure_patterns,
        provider_capability_contract, provider_requires_cwd_git_checkout,
        provider_runner_readiness_contracts, provider_runner_secret_env_for_plan,
        provider_runner_source_contracts, provider_secret_sources_for_plan,
        provider_secret_sources_for_providers, required_extension_ids_for_plan,
        AgentTaskExecutorProvider, AgentTaskProviderCapabilityContract,
        AgentTaskProviderDependencyFailurePattern, AgentTaskProviderEnvPathReadiness,
        AgentTaskProviderRoleAliases, AgentTaskProviderRunnerReadiness,
        AgentTaskProviderRunnerSource, AgentTaskProviderWorkspaceMaterialization,
        AgentTaskRuntimeContract, AgentTaskRuntimeLifecycleStates, AgentTaskRuntimeNormalization,
        AgentTaskRuntimeOutputArtifactMapping, ExtensionProviderAgentTaskExecutor,
        WorkspaceMaterializationSpec, WorkspaceMountSpec, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
        AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA,
    };
    pub(crate) use super::super::agent_task_provider::{
        provider_runner_secret_env_for_plan_with_providers,
        provider_secret_sources_for_plan_with_providers,
    };
}

/// Scheduling primitives: plans, scheduler, execution context, retry/concurrency.
///
/// Some types here (such as `AgentTaskPlan`) are also re-exported at the
/// facade root for ergonomics. The `scheduler` group provides a stable named
/// import location for callers that prefer the explicit grouping.
pub mod scheduler {
    pub use super::super::agent_task_schedule::{
        AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
        AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
        AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
        AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
        AgentTaskArtifactOutputDeclaration, AgentTaskBackpressureStatus,
        AgentTaskCancellationToken, AgentTaskExecutionContext, AgentTaskOutputBinding,
        AgentTaskOutputDependencies, AgentTaskPlan, AgentTaskProgressEvent, AgentTaskQueueStatus,
        AgentTaskResourceBudget, AgentTaskResourceBudgetStatus, AgentTaskResourcePressure,
        AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState, AGENT_TASK_PLAN_SCHEMA,
    };
    pub use super::super::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskScheduler};
}

/// Secret-env mapping and resolution helpers.
pub mod secrets {
    pub use super::super::agent_task_secrets::{
        map_secret_to_env, map_secret_to_keychain_bundle, remove_secret_mapping,
        resolve_secret_env, resolve_secret_env_with_fallbacks, secret_env_status,
        secret_env_status_with_fallbacks, set_config_secret, set_keychain_bundle,
        set_keychain_secret, validate_secret_env, AgentTaskSecretEnvStatus,
        AgentTaskSecretResolutionError,
    };
}

/// High-level service entry points combining lifecycle and scheduling.
pub mod service {
    pub use super::super::agent_task_service::{
        aggregate_exit_code, artifacts, cancel, discover_runs, logs, normalize_plan_workspaces,
        promotion_source, read_plan, resume, retry, run_cook_loop, run_loaded_plan, run_next,
        run_submitted, status, submit_plan_spec, AgentTaskDiscoveryCommands,
        AgentTaskDiscoveryCounts, AgentTaskDiscoveryFilter, AgentTaskDiscoveryReport,
        AgentTaskDiscoveryRun, AgentTaskLoopAttemptReport, AgentTaskLoopReport,
        AgentTaskLoopServiceOptions, AgentTaskRetryServiceResult, AgentTaskRunResult,
    };
}
