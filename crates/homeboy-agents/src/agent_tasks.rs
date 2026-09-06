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
//! - [`loop_controller`]: durable agent-task loop controller state contracts.
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

pub use super::agent_task::AgentCommandPolicy;

pub use super::agent_task::AgentSupervisionPolicy;

pub use super::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskMatrixAggregate,
    AgentTaskMatrixAggregateCell, AgentTaskMatrixAxis, AgentTaskMatrixExecutionState,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AgentToolExecutionLocation, AgentToolPolicy,
    AgentToolRequest, AgentToolResultStatus, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_MATRIX_AGGREGATE_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA,
};

pub use super::agent_task_aggregate::{
    AgentTaskAggregateReport, AgentTaskAggregateSummary, AgentTaskDecisionRef,
    AgentTaskReconciliationDecision, AGENT_TASK_AGGREGATE_SCHEMA,
};

pub use super::agent_task_contract::{
    agent_task_core_contract, AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
    AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA, AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
};

pub use super::agent_task_batch::{
    AgentTaskBatchRecord, AgentTaskBatchState, AGENT_TASK_BATCH_ARTIFACTS_SCHEMA,
};

/// Durable portfolio reconciliation for independent fanout children. Dependency
/// topology is supplied through `FanoutDependencyResolver` by #10946.
pub mod fanout_supervisor {
    pub use super::super::agent_task_fanout_supervisor::*;
}

// Plan/scheduler/execution context types are widely consumed and stay at the
// facade root for ergonomics.
pub use super::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyPolicy, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactOutputDeclaration,
    AgentTaskArtifactPostprocessStep, AgentTaskExecutionContext, AgentTaskOutputBinding,
    AgentTaskOutputDependencies, AgentTaskPlan, AgentTaskResourceBudget, AgentTaskResourcePressure,
    AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
};

// `AgentTaskProgressEvent` is defined in both `agent_task` and
// `agent_task_schedule`. Historically the wildcard facade picked whichever the
// glob resolved last; the canonical type for orchestration callers is the
// schedule-side variant, so name it explicitly here.
pub use super::agent_task_schedule::AgentTaskProgressEvent;

pub use super::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskScheduler, SharedAgentTaskExecutor,
};

pub use super::agent_tool_control_plane::dispatch_agent_tool_request;

// Matrix expansion is `pub(crate)` on the implementation module; expose it
// through the facade for callers inside the crate that need to expand a plan
// matrix without depending on the implementation path.
pub use super::agent_task::expand_agent_task_matrix;

// Convenience re-exports of the loop-controller state enum and lineage record
// that appear on the loop-controller surface and the durable run surface.
pub use super::agent_task_loop_controller::AgentTaskLoopControllerState;
pub use super::agent_task_loop_definition::{
    compile_loop_definition, AGENT_TASK_LOOP_DEFINITION_SCHEMA,
};

// Secret-env status type is referenced from review/dispatch commands.
pub use super::agent_task_secrets::{
    secret_env_status, secret_env_status_with_fallbacks, AgentTaskSecretEnvStatus,
};
pub use homeboy_core::secret_env_plan::{
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
        apply_event, apply_spec_dispatch_defaults, apply_spec_dispatch_defaults_with_cwd,
        controller_request_dispatch_command, init, init_from_spec, list,
        load_materialize_spec_source, mark_human_ready, plan_from_spec, prepare_controller_proof,
        resolve_proof_profile, resume, resume_with_options, run_action, run_next,
        AgentTaskRepoLoopSpec, AgentTaskRepoLoopSpecWorkflow, ControllerApplyEventRequest,
        ControllerDispatchHook, ControllerDispatchOverrides, ControllerFromSpecRequest,
        ControllerInitRequest, ControllerMarkHumanReadyRequest, ControllerPlanRequest,
        ControllerResumeOptions, APPLY_EVENT_RESULT_SCHEMA,
    };
}

/// Cook-loop evaluation contracts and entry points.
pub mod cook_loop {
    pub use super::super::agent_task_cook_loop::{evaluate_cook_loop, AgentTaskCookLoopOptions};
}

/// Durable batch/fanout lifecycle records built from independent child runs.
pub mod batch {
    pub use super::super::agent_task_batch::status;
    pub use super::super::agent_task_batch::{
        artifacts, claim_fanout_run_batch, fanout_dependency_graph_with_finalization_statuses,
        finalize_provider_worktree_for_child, heartbeat_fanout_run_batch, owned_child_run_ids,
        persist_fanout_run_batch, read_batch_record, record_fanout_run_batch_failure,
        record_provider_worktree_finalization_deferred,
        record_provider_worktree_finalization_preflight_error, start_fanout_run_batch,
        BatchProviderWorktreeFinalization,
    };
    pub use super::super::agent_task_batch::{
        fanout_aggregate_state, record_fanout_run_batch_failed_admissions, submit_plan_batch,
        AgentTaskBatchRecord, AgentTaskBatchState, FanoutRunBatchChild,
        AGENT_TASK_BATCH_ARTIFACTS_SCHEMA,
    };
}

/// Generic dependency graph validation and readiness projection for fanout and
/// supervisor orchestration.
pub mod dependency_graph {
    pub use super::super::agent_task_dependency_graph::{
        dependency_graph_readiness, AgentTaskDependencyNode, AgentTaskDependencyReadiness,
        AgentTaskDependencyState,
    };
}

/// Durable Git/PR actions released by a merged fanout dependency.
pub mod dependency_actions {
    pub use super::super::agent_task_dependency_actions::{
        execute_resolved_dependency_actions, DependencyAction, DependencyActionExecutor,
        DependencyResolution,
    };
}

/// Durable dispatch request, plan construction, and execution service.
pub mod dispatch_service {
    pub use super::super::agent_task_dispatch_plan::{
        build_dispatch_plan, build_dispatch_plan_with_provider_requirements,
        preflight_dispatch_provider_secrets, validate_single_cook_prompt_source,
    };
    pub use super::super::agent_task_dispatch_service::{
        build_controller_dispatch_plan, controller_resolved_execution_policy, dispatch,
        preflight_dispatch_provider_admission, resolve_cook_initial_provider_route_with_catalog,
        resolve_dispatch_request, resolve_dispatch_request_with_default,
        resolve_dispatch_request_with_default_and_catalog, run_dispatch_command,
        AgentTaskDispatchCommand, AgentTaskDispatchRequest, DispatchCoreInputs,
        DISPATCH_RESULT_SCHEMA,
    };
}

/// PR finalization contracts and backends.
pub mod finalization {
    pub use super::super::agent_task_finalization::{
        finalize_pr, hydrate_manual_verification_dependencies, preflight_pr, AgentTaskGateResult,
        AgentTaskGateSetupEvidence, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions,
        AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship, AgentTaskPrVerification,
    };
}

/// Typed reviewer dossier contracts and deterministic rendering.
pub mod review_dossier {
    pub use super::super::agent_task_review_dossier::{
        homeboy_tool_disclosure, resolve_review_profile, validate_issue_reference,
        AgentTaskExternalUsageEvidence, AgentTaskExternalUsageStatus, AgentTaskPublicContract,
        AgentTaskPublicContractEvidence, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
        AgentTaskReviewIssueRelationship, AgentTaskReviewIssueRelationshipKind,
        AgentTaskReviewOverride, AgentTaskReviewOverrideTarget, AgentTaskReviewProfile,
        AgentTaskReviewTestStep, AGENT_TASK_REVIEW_DOSSIER_SCHEMA,
    };
}

/// Gate report contracts, visibility, and reveal policies.
pub mod gate {
    pub use super::super::agent_task_gate::{
        AgentTaskGateEnvironmentMode, AgentTaskGateEnvironmentPolicy, AgentTaskGateExecutionPolicy,
        AgentTaskGateExtensionInput, AgentTaskGateInputSource,
        AgentTaskGatePackageArtifactRequirement, AgentTaskGateRevealPolicy,
        AgentTaskGateToolchainRequirement, AgentTaskGateVisibility, VerifyGateOptions,
    };
}

/// Durable run lifecycle: submit, run-record state, log/artifact loaders.
pub mod lifecycle {
    pub use super::super::agent_task_lifecycle::{
        aggregate_source, artifacts, cancel_run, cook_index, durable_local_read,
        fail_detached_cook_handoff_parent, list_records, load_plan, logs,
        reconcile_terminal_artifact_projection, record_cook_finalization,
        record_execution_placement_outcome, recover_unmaterialized_cook_input_publication, retry,
        run_id_for_aggregate_path, run_record_exists, run_record_exists_readonly, submit_plan,
    };
    pub use super::super::agent_task_lifecycle::{
        cancel, claim_cook_operation_in_store, claim_local_cook_retry_launch_in_store,
        complete_cook_operation_in_store, consume_unmaterialized_cook_replay_claim,
        cook_attempt_run_id, cook_index_exists_in_store, fail_cook_operation_in_store,
        has_accepted_runner_handoff, is_unmaterialized_cook_admission, load_plan_in_store,
        materialize_recovered_patch_artifact, pin_current_controller_runtime,
        pinned_runtime_for_mutation, precheck_unmaterialized_cook_admission,
        prepare_unmaterialized_cook_admission, prune_controller_runtime_pins,
        quarantine_queued_run_exact_in_store, rearm_quarantined_run_in_store,
        reconcile_record_health_in_store, reconcile_status, reconcile_status_in_store,
        reconcile_status_with_options, record_acceptance_verdict_with_feedback_in_store,
        record_cook_attempt_in_store, record_cook_finalization_in_store,
        record_cook_force_with_lease_receipt_in_store, record_detached_cook_handoff_child_in_store,
        record_detached_cook_handoff_parent_in_store, record_detached_cook_supervisor_in_store,
        record_local_cook_retry_child_in_store, record_local_cook_retry_supervisor_in_store,
        record_manual_finalization_failure, record_pre_execution_failure_in_store,
        record_unmaterialized_cook_admission_in_store, register_acceptance_verifier,
        register_acceptance_verifier_from_config,
        release_unmaterialized_cook_replay_claim_after_worker_exit,
        renew_unmaterialized_cook_replay_claim,
        reserve_detached_cook_handoff_materialization_in_store,
        resolve_detached_cook_materializing_attempt_in_store, resolve_promotion_patch_artifact_id,
        run_record_exists_resolved_in_store, runner_diagnostic_probe,
        runner_pinned_runtime_for_mutation,
        transition_execution_placement_for_continuation_in_store, AgentTaskAcceptanceVerdict,
        AgentTaskArtifactRef, AgentTaskDurableReadUnavailable, AgentTaskLifecycleStore,
        AgentTaskPreDispatchFailure, AgentTaskRemoteDispatchFailure, AgentTaskRunRecord,
        AgentTaskRunState, AgentTaskRunnerDiagnosticProbe, AgentTaskRunnerProbe,
        AgentTaskStatusOptions, ClaimOutcome, ControllerRuntimePruneResult, DetachedLabRunRecord,
        LabOffloadProxyPlan, LocalCookRetryLaunchClaim, RunnerPinnedRuntime,
    };
    pub use super::super::agent_task_lifecycle::{
        cook_index_exists, mark_running, record_run_aggregate, record_runner_job_identity,
        verified_controller_artifact_projection_path,
    };
    pub use super::super::agent_task_lifecycle::{
        exact_record, load_controller_plan, record_detached_lab_run, record_lab_offload_phase,
        record_lab_offload_planned, record_pre_dispatch_failure, record_pre_execution_failure,
        record_remote_dispatch_failure, status,
    };
    #[cfg(feature = "test-support")]
    pub use super::super::agent_task_lifecycle::{
        fail_next_record_write_for_test, rewrite_record_for_test,
    };
    pub use super::super::agent_task_lifecycle::{record_completed_run, record_promotion};
}

/// Durable agent-task loop controller state, events, and policy.
pub mod loop_controller {
    pub use crate::agent_task_loop_controller::{
        controller_status_report, create_controller, load_controller, write_controller,
        AgentTaskLoopActionStatus, AgentTaskLoopControllerRecord, AgentTaskLoopControllerState,
        AgentTaskLoopPolicyAction, AgentTaskLoopRunnerAvailability, AgentTaskLoopWait,
        AgentTaskLoopWaitStatus, AGENT_TASK_LOOP_CONTROLLER_SCHEMA,
        AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA,
    };
}

/// Declarative loop definitions compiled into scheduler plans.
pub mod loop_definition {
    pub use super::super::agent_task_loop_definition::{
        compile_loop_definition, AGENT_TASK_LOOP_DEFINITION_SCHEMA,
    };
}

/// Promotion reports and entry point.
pub mod promotion {
    pub use super::super::agent_task_promotion::{
        canonical_recoverable_patch_artifacts, promote, AgentTaskPromotionArtifactRef,
        AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
        AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
        AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase, PromotionProgressCallback,
    };
}

/// Executor provider contracts used by extensions and routing.
pub mod provider {
    /// Compile-time gate for the `fixture` test double. Always `false` in a
    /// production build: `fixture` is not a registered agent runtime, so no
    /// caller may branch on the name outside a `test-support` build (#11118).
    pub use crate::agent_task_provider::is_fixture_backend;
    pub use crate::agent_task_provider::{
        default_backend, default_backend_for_component, dependency_failure_patterns,
        provider_capability_contract, provider_requires_cwd_git_checkout,
        provider_runner_readiness_contracts, provider_runner_source_contracts,
        provider_secret_env_scopes, provider_secret_sources_for_providers,
        required_extension_ids_for_plan, resolve_provider_for_backend,
        validate_provider_runner_readiness_for_backend_with_catalog, AgentTaskExecutorProvider,
        AgentTaskProviderCatalog, AgentTaskProviderDependencyFailurePattern,
        AgentTaskProviderEnvPathReadiness, AgentTaskProviderRunnerReadiness,
        AgentTaskProviderRunnerReadinessContract, AgentTaskProviderRunnerSource,
        ExtensionProviderAgentTaskExecutor, ProviderReadinessInvocationResult, ProviderResolution,
        ProviderRuntimeReadinessCache, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
        AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA, PROVIDER_READINESS_RESULT_SCHEMA,
    };
    /// Credential readiness: whether a *declared* provider is actually
    /// *dispatchable* here, and the pre-dispatch preflight that enforces it
    /// before a workspace or a provider execution is spent (#11479).
    pub use crate::agent_task_provider::{
        evaluate_provider_dispatchability, evaluate_provider_dispatchability_with_config,
        preflight_provider_credentials_for_backend, preflight_provider_dispatchability_with_config,
        provider_credential_readiness, AgentTaskProviderDispatchability,
    };
    pub use crate::agent_task_provider::{
        probe_provider_executor_resolves, provider_runner_secret_env_for_plan_with_providers,
        provider_secret_sources_for_plan_with_providers, ProviderExecutorResolution,
    };
}

/// Scheduling primitives: plans, scheduler, execution context, retry/concurrency.
///
/// Some types here (such as `AgentTaskPlan`) are also re-exported at the
/// facade root for ergonomics. The `scheduler` group provides a stable named
/// import location for callers that prefer the explicit grouping.
pub mod scheduler {
    pub use super::super::agent_task_schedule::{
        AgentTaskAdaptiveConcurrencyPolicy, AgentTaskAggregate, AgentTaskAggregateStatus,
        AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactOutputDeclaration,
        AgentTaskExecutionContext, AgentTaskOutputBinding, AgentTaskOutputDependencies,
        AgentTaskPlan, AgentTaskResourceBudget, AgentTaskResourcePressure, AgentTaskRetryPolicy,
        AgentTaskScheduleOptions, AgentTaskState,
    };
    pub use super::super::agent_task_scheduler::{
        AgentTaskExecutorAdapter, AgentTaskScheduler, SharedAgentTaskExecutor,
    };
}

/// Secret-env mapping and resolution helpers.
pub mod secrets {
    pub use super::super::agent_task_secrets::{
        legacy_secrets_file, map_secret_to_env, map_secret_to_keychain_bundle,
        remove_secret_mapping, resolve_secret_env, resolve_secret_env_with_fallbacks,
        secret_env_status, secret_env_status_for_scopes, secret_env_status_with_fallbacks,
        set_config_secret, set_keychain_bundle, set_keychain_secret, AgentTaskSecretEnvStatus,
    };
}

/// High-level service entry points combining lifecycle and scheduling.
pub mod service {
    pub use super::super::agent_task_service::{
        adopt_cook_candidate_with_options_dispatcher_and_executor_for_attempt,
        attempt_primary_failure_diagnostic, authorize_cook_continue_route,
        authorize_cook_continue_route_with_artifact, claim_continuation_for,
        claim_continuation_for_recovery_and_clear_failure_in_store,
        compile_cook_attempt_with_catalog_and_readiness_cache,
        compile_cook_attempt_with_readiness_cache, consume_claimed_terminal_with_dispatcher,
        consume_claimed_with_dispatcher, continuation_state_in_store, control_plane_run,
        cook_batch_job_submission, cook_continuation_replays_provider,
        cook_continuation_requires_model_provenance, cook_request_is_review_form_only,
        detached_batch_coordinator_control, enqueue_terminal_continuation, evidence_ref_task_id,
        execute_promotion_with_progress, hydrate_evidence_ref, hydrate_evidence_summary,
        liveness_for_record, load_recipe, load_recipe_for_attempt,
        local_pre_execution_runtime_recovery_is_eligible, persist_manual_finalization_intent,
        persist_manual_finalization_receipt, persist_manual_finalization_retry_intent,
        persist_provider_boundary_replay_evidence, preflight_continuation_claim,
        preflight_cook_continuation_admission_for_observation,
        preflight_cook_promotion_for_observation_in_store,
        preflight_recipe_attempt_for_continuation_in_store, prepare_cook_workspace_base,
        prepare_manual_finalization_identity, promotion_is_resumable, read_plan,
        reconcile_recipe_attempt_for_continuation, reconstruct_adoption_options_with_dispatcher,
        reconstruct_options_for_pre_execution_recovery, reconstruct_options_with_dispatcher,
        reconstruct_options_with_local_placement_override, record_replacement_gate_proof,
        recover_cook_pr, recover_missing_promotion_aggregate, register_cook_batch_work_handler,
        register_cook_work_handler, register_promotion_job_driver, resolve_cook_budget, resume,
        resume_cook, resume_cook_batch, retry_with_provider_route_override,
        retry_with_timeout_override, review_form_timeout_ms, run_cook_batch_with_control,
        run_loaded_plan, run_next, run_next_with_cook_dispatcher, run_submitted,
        run_submitted_with_timeout, source_worktree_path, submit_plan_spec,
        terminal_review_form_continuation_is_eligible,
        terminal_review_form_continuation_is_eligible_for_observation_readonly,
        terminal_transport_recovery_required, validate_recipe_attempt_record,
        verify_replacement_gates, AgentTaskCandidateAdoptionOptions, AgentTaskCookBatchCellReport,
        AgentTaskCookBatchControl, AgentTaskCookBatchOptions, AgentTaskCookBatchReport,
        AgentTaskCookCellError, AgentTaskCookReport, AgentTaskDiscoveryFilter,
        AgentTaskDiscoveryReport, AgentTaskHydratedEvidence, AgentTaskLiveness,
        AgentTaskPromotionRequest, AgentTaskRunResult, CookContinuationState, CookMode,
        CookProgressEvent, CookProviderRouteOverride, CookRecipeStore, CookRequest, CookRuntime,
        CookService, DEFAULT_REVIEW_FORM_TIMEOUT_MS, DETACHED_BATCH_COORDINATOR_ENV,
        MAX_REVIEW_FORM_TIMEOUT_MS,
    };
    pub use super::super::agent_task_service::{
        artifacts, logs, persist_initial_recipe, promotion_source,
        reconcile_terminal_artifact_projection, resolve_cook_continuation_run_id, retry,
        validate_initial_recipe_compatibility,
    };
}
