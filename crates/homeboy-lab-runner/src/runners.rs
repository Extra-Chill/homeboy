//! Stable facade for runner configuration, connection, execution, and lab
//! offload APIs.
//!
//! The runner module tree only exposes a hand-picked surface (most submodules
//! are private). Routing consumers through this facade keeps that contract
//! explicit and lets the underlying module layout evolve without touching
//! external callers. Some in-tree callers still import from the crate root
//! directly for names this facade does not re-export.
//!
//! Exports are a single flat list covering stable identity, registry,
//! connection, execution, workspace, evidence, capability, and lab offload
//! contracts.

// ----------------------------------------------------------------------------
// Stable top-level contracts
// ----------------------------------------------------------------------------

pub use crate::runner_capability_inventory;
pub use crate::{
    apply_change_artifact, apply_workspace_patch, broker_auth_store_path,
    broker_submit_token_for_runner, broker_token_from_env, close_reconnected_job_log_owner,
    connect, connect_with_live_lease_adoption, connect_with_orphan_adoption,
    reconcile_terminal_jobs, reconnect_job_log_owner, reverse_broker_artifact,
    reverse_broker_artifact_content, reverse_broker_reconcile, runner_artifact_content,
    BrokerAuthGrant, BrokerAuthStore, BrokerCredential, BrokerScope, MintedCredential,
    BROKER_TOKEN_ENV, BROKER_TOKEN_HEADER,
};
pub use crate::{
    connect_reverse, disconnect, disconnect_local_recovery, download_remote_artifact,
    evaluate_lab_runner_capabilities_for_runner, exec, execute_lab_offload,
    hydrate_prepared_workspace_source_snapshot, is_remote_runner_artifact_path,
    is_reportable_artifact_evidence_path, is_retrievable_runner_artifact,
    lab_offload_changed_since_ref, lab_offload_metadata,
    lab_offload_metadata_with_workspace_mapping, lab_runner_readiness, list_workspaces,
    mirror_connected_runner_run, mirrored_runner_job_identity, persisted_status,
    persisted_statuses, plan_homeboy_binary_refresh, plan_managed_runner_source_sync,
    plan_managed_runner_source_syncs, plan_workspace_pull, preflight_lab_offload_changed_since,
    preflight_remote_argv_path_translation, prepare_explicit_lab_runner_for_offload,
    prepare_git_lab_offload_changed_since, prepare_lab_runner_capability,
    promote_runner_exec_artifact_dirs, promote_runner_exec_artifacts,
    promote_runner_exec_summaries, promoted_output, prune_homeboy_binary_cache, prune_workspaces,
    pull_workspace, refresh_homeboy_binary, refresh_mirrored_daemon_evidence,
    reportable_artifact_evidence_path, resolve_default_lab_runner, run_reverse_worker,
    runner_artifact_store_token, runner_dev_sync, runner_exec_failure_error,
    runner_exec_structured_summary, runner_generation_inventory,
    runner_generation_inventory_for_session, runner_homeboy_path_for_command, runner_job_cancel,
    runner_job_cancel_for_session, runner_job_log_snapshot, runner_job_log_snapshot_for_session,
    status, statuses, statuses_indexed, sync_workspace, update_workspace, workspace_snapshots,
    HomeboyBinaryRefreshMode, HomeboyBinaryRefreshOptions, HomeboyBinaryRefreshOutput,
    HomeboyBinaryRefreshPlan, LabJobOverrides, LabOffloadCommand, LabOffloadOutcome,
    LabOffloadRequest, LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy,
    LabRunnerCapabilityContract, LabRunnerGateDecision, LabRunnerGateMode, LabRunnerHandoff,
    LabRunnerReadiness, LabRunnerReadinessState, LabRunnerSelectionSource,
    ManagedRunnerSourceSyncPlan, PreparedLabRunnerCapability, RemoteArtifactDownload,
    ReverseRunnerConnectOptions, ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput, Runner,
    RunnerActiveJobSource, RunnerActiveJobState, RunnerActiveJobsSnapshot, RunnerAdmissionSummary,
    RunnerArtifactRef, RunnerAvailability, RunnerBinaryCachePruneEntry,
    RunnerBinaryCachePruneOptions, RunnerBinaryCachePruneOutput, RunnerBinarySource,
    RunnerCapabilityInventory, RunnerCapabilityPreflight, RunnerChangedRuntimePath,
    RunnerConnectReport, RunnerDaemonGenerationStatus, RunnerDevSyncOptions, RunnerDevSyncOutput,
    RunnerDisconnectReport, RunnerExecDiagnostics, RunnerExecMode, RunnerExecOptions,
    RunnerExecOutput, RunnerExecPromotedOutput, RunnerExecStructuredSummary, RunnerFailureKind,
    RunnerJob, RunnerKind, RunnerLifecycleOwner, RunnerMutationArtifacts,
    RunnerNamedWorkspaceLease, RunnerRecoveryState, RunnerRequiredTool, RunnerResourceMetrics,
    RunnerResult, RunnerSecretEnvMigrationPlan, RunnerSession, RunnerSessionRole,
    RunnerSessionState, RunnerSpec, RunnerStaleDaemonWarning, RunnerStaleRuntimePath,
    RunnerStatusReport, RunnerToolRegistry, RunnerToolSpec, RunnerTunnelMode,
    RunnerWorkspaceApplyOptions, RunnerWorkspaceApplyOutput, RunnerWorkspaceApplyStatus,
    RunnerWorkspaceLease, RunnerWorkspaceLeaseSet, RunnerWorkspaceListEntry,
    RunnerWorkspaceListOutput, RunnerWorkspaceMaterializationPlan, RunnerWorkspacePruneEntry,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry,
    RunnerWorkspacePullOptions, RunnerWorkspacePullOutput, RunnerWorkspacePullPlan,
    RunnerWorkspaceSnapshotAppliedFilters, RunnerWorkspaceSnapshotEntry,
    RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSnapshotsOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput, RunnerWorkspaceUpdateOptions,
    RunnerWorkspaceUpdateOutput, RuntimeMaterializationStatus,
};

// Registry CRUD entry points.
pub use crate::{
    apply_secret_env_migration, create, delete_safe, effective_env, enable_server_runner, exists,
    list, load, merge, secret_env_migration_plan,
};

// Crate-internal helpers that historically flowed through the wildcard
// `pub use runner::*`. Keep them available so existing in-tree callers
// (currently `commands::runs::remote`) compile, but do not expose them as
// public API.
pub use crate::daemon_api_get;
