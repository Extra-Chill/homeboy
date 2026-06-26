//! Stable facade for runner configuration, connection, execution, and lab
//! offload APIs.
//!
//! New command and integration code MUST import runner APIs from this module
//! instead of depending directly on `core::runner::*`. The runner module tree
//! itself only exposes a hand-picked surface (most submodules are private),
//! but routing every consumer through this facade keeps the contract explicit
//! and lets the underlying module layout evolve without touching external
//! callers.
//!
//! The exports are organised into nested API groups by operation:
//!
//! - top-level: stable identity, registry, capability, and session contracts
//!   that callers reach for most often.
//! - [`registry`]: CRUD helpers over the runner config registry.
//! - [`connection`]: connect/disconnect/status helpers for runner sessions.
//! - [`execution`]: exec entry points and option/output contracts.
//! - [`workspace`]: workspace sync and patch application contracts.
//! - [`evidence`]: artifact evidence/mirroring helpers.
//! - [`capabilities`]: lab runner capability evaluation contracts.
//! - [`lab_offload`]: lab offload entry points and contracts.

// ----------------------------------------------------------------------------
// Stable top-level contracts
// ----------------------------------------------------------------------------

pub use super::runner::{
    apply_change_artifact, apply_workspace_patch, broker_auth_store_path, broker_token_from_env,
    capture_lab_offload_subprocess_metadata, connect, reverse_broker_artifact,
    reverse_broker_reconcile, BrokerAuthGrant, BrokerAuthStore, BrokerCredential, BrokerScope,
    MintedCredential, BROKER_TOKEN_ENV, BROKER_TOKEN_HEADER,
};
pub use super::runner::{
    connect_reverse, disconnect, download_remote_artifact,
    evaluate_lab_runner_capabilities_for_runner, exec, execute_lab_offload,
    is_remote_runner_artifact_path, is_reportable_artifact_evidence_path,
    is_retrievable_runner_artifact, lab_offload_changed_since_ref, lab_offload_metadata,
    lab_offload_metadata_with_workspace_mapping, list_workspaces, mirror_connected_runner_run,
    mirrored_runner_job_identity, plan_homeboy_binary_refresh, plan_managed_runner_source_sync,
    plan_managed_runner_source_syncs, preflight_lab_offload_changed_since,
    preflight_remote_argv_path_translation, prepare_git_lab_offload_changed_since,
    prepare_lab_runner_capability, prune_workspaces, refresh_homeboy_binary,
    refresh_mirrored_daemon_evidence, reportable_artifact_evidence_path,
    resolve_default_lab_runner, run_reverse_worker, runner_artifact_store_token,
    runner_exec_failure_error, runner_job_cancel, runner_job_log_snapshot, status, statuses,
    sync_workspace, HomeboyBinaryRefreshMode, HomeboyBinaryRefreshOptions,
    HomeboyBinaryRefreshOutput, HomeboyBinaryRefreshPlan, LabJobOverrides, LabLocalExecutionPolicy,
    LabOffloadCommand, LabOffloadOutcome, LabOffloadRequest, LabOffloadSourcePathMode,
    LabOffloadWorkspaceModePolicy, LabRunnerCapabilityContract, LabRunnerGateDecision,
    LabRunnerGateMode, LabRunnerSelectionSource, ManagedRunnerSourceSyncPlan,
    PreparedLabRunnerCapability, RemoteArtifactDownload, ReverseRunnerConnectOptions,
    ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput, Runner, RunnerActiveJobSource,
    RunnerActiveJobState, RunnerArtifactRef, RunnerCapabilityPreflight, RunnerConnectReport,
    RunnerDisconnectReport, RunnerExecDiagnostics, RunnerExecMode, RunnerExecOptions,
    RunnerExecOutput, RunnerFailureKind, RunnerHandoff, RunnerJob, RunnerKind,
    RunnerLifecycleOwner, RunnerMutationArtifacts, RunnerNamedWorkspaceLease, RunnerRequiredTool,
    RunnerResourceMetrics, RunnerResult, RunnerSession, RunnerSessionRole, RunnerSessionState,
    RunnerSpec, RunnerStaleDaemonWarning, RunnerStatusReport, RunnerToolRegistry, RunnerToolSpec,
    RunnerTunnelMode, RunnerWorkspaceApplyOptions, RunnerWorkspaceApplyOutput,
    RunnerWorkspaceApplyStatus, RunnerWorkspaceLease, RunnerWorkspaceLeaseSet,
    RunnerWorkspaceListEntry, RunnerWorkspaceListOutput, RunnerWorkspacePruneEntry,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

// Registry CRUD entry points (re-exported at the root for ergonomics; also
// available via the explicit `registry` group below).
pub use super::runner::{
    create, delete_safe, effective_env, enable_server_runner, exists, list, load, merge,
};

// Crate-internal helpers that historically flowed through the wildcard
// `pub use runner::*`. Keep them available so existing in-tree callers
// (currently `commands::runs::remote`) compile, but do not expose them as
// public API.
pub(crate) use super::runner::daemon_api_get;

// ----------------------------------------------------------------------------
// Explicit API groups
// ----------------------------------------------------------------------------

/// CRUD helpers over the runner config registry.
pub mod registry {
    pub use super::super::runner::{
        create, delete_safe, effective_env, enable_server_runner, exists, list, load, merge,
        resolve_default_lab_runner, Runner, RunnerKind, RunnerSpec,
    };
}

/// Connect/disconnect/status helpers for runner sessions.
pub mod connection {
    pub use super::super::runner::{
        connect, connect_reverse, disconnect, run_reverse_worker, status, statuses,
        ReverseRunnerConnectOptions, ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput,
        RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind, RunnerSession,
        RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
        RunnerTunnelMode,
    };
}

/// Exec entry points and option/output contracts.
pub mod execution {
    pub use super::super::runner::{
        exec, runner_exec_failure_error, RunnerExecDiagnostics, RunnerExecMode, RunnerExecOptions,
        RunnerExecOutput, RunnerResourceMetrics,
    };
}

/// Workspace sync and patch application contracts.
pub mod workspace {
    pub use super::super::runner::{
        apply_change_artifact, apply_workspace_patch, list_workspaces,
        plan_managed_runner_source_sync, plan_managed_runner_source_syncs, prune_workspaces,
        sync_workspace, ManagedRunnerSourceSyncPlan, RunnerWorkspaceApplyOptions,
        RunnerWorkspaceApplyOutput, RunnerWorkspaceApplyStatus, RunnerWorkspaceListEntry,
        RunnerWorkspaceListOutput, RunnerWorkspacePruneEntry, RunnerWorkspacePruneOptions,
        RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry, RunnerWorkspaceSyncMode,
        RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
    };
}

/// Artifact evidence and mirroring helpers.
pub mod evidence {
    pub use super::super::runner::{
        download_remote_artifact, is_remote_runner_artifact_path,
        is_reportable_artifact_evidence_path, is_retrievable_runner_artifact,
        mirror_connected_runner_run, mirrored_runner_job_identity,
        refresh_mirrored_daemon_evidence, reportable_artifact_evidence_path,
        runner_artifact_store_token, runner_job_log_snapshot, RemoteArtifactDownload,
    };
}

/// Lab runner capability evaluation contracts.
pub mod capabilities {
    pub use super::super::runner::{
        evaluate_lab_runner_capabilities_for_runner, prepare_lab_runner_capability,
        LabRunnerCapabilityContract, LabRunnerGateDecision, LabRunnerGateMode,
        PreparedLabRunnerCapability, RunnerCapabilityPreflight, RunnerRequiredTool,
    };
}

/// Lab offload entry points and contracts.
pub mod lab_offload {
    pub use super::super::runner::{
        capture_lab_offload_subprocess_metadata, execute_lab_offload,
        lab_offload_changed_since_ref, lab_offload_metadata,
        lab_offload_metadata_with_workspace_mapping, preflight_lab_offload_changed_since,
        prepare_git_lab_offload_changed_since, LabJobOverrides, LabOffloadCommand,
        LabOffloadOutcome, LabOffloadRequest, LabOffloadSourcePathMode,
        LabOffloadWorkspaceModePolicy, LabRunnerSelectionSource,
    };
}
