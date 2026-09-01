use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_agents::agent_task_secrets;
use homeboy_core::config::{self, ConfigEntity};
use homeboy_core::defaults;
use homeboy_core::error::{Error, Result};
use homeboy_core::output::{BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult};
use homeboy_core::server::{self, RunnerPolicy, RunnerSecretEnvRef, RunnerSettings, ServerRunner};

// agent_task_lifecycle_event is pure core logic (job-events -> lifecycle events)
// that was mis-filed under runner. It now lives in agent_task_lifecycle; re-
// exported here so runner-internal `super::agent_task_lifecycle_event` /
// `crate::agent_task_lifecycle_event` call sites resolve unchanged.
pub(crate) use homeboy_agents::agent_task_lifecycle::agent_task_lifecycle_event;
// The cook/dispatch counterpart of `agent_task_lifecycle_event`: turns a
// runner terminal result into a typed, contract-keyed handoff event so the
// controller stops depending on the offloaded command's output format (#7530).
pub(crate) use homeboy_agents::agent_task_lifecycle::agent_task_handoff_event;
mod apply;
pub mod artifact_attach;
mod availability_provider;
mod broker_http;
mod cancellable_sleep;
pub mod dev_run;
pub use homeboy_core::broker_auth::{
    broker_submit_token_for_runner, broker_token_from_env, extract_bearer_token,
    store_path as broker_auth_store_path, BrokerAuthGrant, BrokerAuthStore, BrokerCredential,
    BrokerScope, MintedCredential, BROKER_TOKEN_ENV, BROKER_TOKEN_HEADER,
};
mod capabilities;
mod cli_resolver;
pub use cli_resolver::{
    resolve_agent_task_dispatch, resolve_command_label, resolve_lab_runner_hint,
    set_agent_task_dispatch_resolver, set_command_label_resolver, set_lab_runner_hint_provider,
    LabRunnerHint,
};
mod command_path;
mod connection;
mod continuation_provider;
pub mod controller_fallback_projection;
mod daemon_exec_driver;
mod daemon_health;
mod daemon_http_get;
mod daemon_repair;
pub use daemon_repair::codes as daemon_repair_codes;
mod discovery;
pub use discovery::RunnerDiscoveryService;
pub mod direct_lab_handoff;
mod evidence;
mod execution;
mod execution_bundle;
mod extension_materialization;
mod generation_store;
pub mod lab_staging_controller;
pub mod runner_staging_operation;
pub mod runner_staging_store;
pub fn runner_generation_inventory(runner_id: &str) -> Result<Vec<RunnerDaemonGenerationStatus>> {
    let report = connection::status(runner_id)?;
    runner_generation_inventory_for_session(runner_id, report.session.as_ref())
}

/// Project persisted generation state from an already-observed status session.
/// This keeps `runner status` from re-running its remote observation path just
/// to render the generation ledger.
pub fn runner_generation_inventory_for_session(
    runner_id: &str,
    session: Option<&RunnerSession>,
) -> Result<Vec<RunnerDaemonGenerationStatus>> {
    generation_store::status_projection(runner_id, session)
}

/// Durable job identities for an already-observed generation view.
pub fn runner_generation_job_owners_for_session(
    runner_id: &str,
    session: Option<&RunnerSession>,
) -> Result<Vec<RunnerGenerationJobOwners>> {
    generation_store::status_job_owners(runner_id, session)
}

/// The complete read-only admission projection for a runner. Command surfaces
/// enrich this snapshot with their own diagnostics but must not recalculate
/// compatibility or rotation safety from a partial observation.
#[derive(Debug)]
pub struct RunnerAdmissionSnapshot {
    pub status: RunnerStatusReport,
    pub generation_inventory: Vec<RunnerDaemonGenerationStatus>,
    pub generation_owners: Vec<RunnerGenerationJobOwners>,
    pub summary: RunnerAdmissionSummary,
}

impl RunnerAdmissionSnapshot {
    fn from_status_and_generations(
        status: RunnerStatusReport,
        generation_inventory: Vec<RunnerDaemonGenerationStatus>,
        generation_owners: Vec<RunnerGenerationJobOwners>,
    ) -> Self {
        let summary = status.admission_summary_with_generations(
            &generation_inventory,
            &generation_owners,
            generation_inventory
                .iter()
                .filter(|generation| !generation.admission_owner)
                .count(),
        );

        Self {
            status,
            generation_inventory,
            generation_owners,
            summary,
        }
    }
}

/// Observe a runner once, then derive every admission fact from that status and
/// its matching generation ledger.
pub fn runner_admission_snapshot(runner_id: &str) -> Result<RunnerAdmissionSnapshot> {
    runner_admission_snapshot_until(
        runner_id,
        std::time::Instant::now() + readonly_probe::readonly_probe_timeout(),
    )
}

/// Observe admission under one caller-owned deadline. Every remote observation
/// in the status projection receives only the budget that remains.
pub fn runner_admission_snapshot_until(
    runner_id: &str,
    deadline: std::time::Instant,
) -> Result<RunnerAdmissionSnapshot> {
    let (status, generation_inventory, generation_owners) =
        connection::status_with_admission_projection_until(runner_id, deadline)?;
    Ok(RunnerAdmissionSnapshot::from_status_and_generations(
        status,
        generation_inventory,
        generation_owners,
    ))
}

/// Build the admission projection from an already-observed status report.
/// This avoids repeating remote inspection when a caller also needs status
/// details.
pub fn runner_admission_snapshot_for_status(
    status: RunnerStatusReport,
) -> Result<RunnerAdmissionSnapshot> {
    let (generation_inventory, generation_owners) =
        generation_store::status_admission_projection(&status.runner_id, status.session.as_ref())?;
    Ok(RunnerAdmissionSnapshot::from_status_and_generations(
        status,
        generation_inventory,
        generation_owners,
    ))
}

mod git_dependency_materialization;
mod homeboy_refresh;
mod job_preparation;
mod lab;
mod lab_apply;
mod lab_args;
mod lab_capabilities;
mod lab_command;
mod lab_env;
mod lab_offload_provider;
pub(crate) mod lab_plan;
mod lab_selection;
pub use lab_selection::{
    compile_lab_admission_plan, placement_readiness, LabAdmissionPlan, PlacementReadiness,
    PlacementReadinessInvocation, PlacementReadinessPredicate, PlacementReadinessRequest,
    PlacementReadinessState,
};
mod lab_workspace_provenance_provider;
mod lab_workspaces;
mod lab_workspaces_deps;
mod managed_source;
mod workspace_root_provider;
pub use managed_source::{
    plan_managed_runner_source_sync, plan_managed_runner_source_syncs, ManagedRunnerSourceSyncPlan,
};
mod offload_changed_since;
mod offload_metadata;
mod origin_refs;
mod progress;
pub mod readonly_probe;
mod resource_metrics;
mod rig_materialization;
mod rolling_generation;
mod runner_cache;
pub mod runner_probe_gate;
mod runtime_materialization_status;
pub mod runtime_materializer;
mod runtime_overlay_freshness;
mod session;
mod shell_quote;
mod source_materialization;
mod tool_registry;
mod transport;
mod validation_dependencies;
pub use runner_cache::{
    prune_homeboy_binary_cache, RunnerBinaryCachePruneEntry, RunnerBinaryCachePruneOptions,
    RunnerBinaryCachePruneOutput,
};
pub use runner_probe_gate::{
    invalidate_runner_probes, reset_runner_probe_gate, runner_probe_metrics, RunnerProbeMetrics,
    PROBE_CACHE_TTL_ENV, PROBE_CONCURRENCY_ENV, PROBE_WAIT_ENV,
};
pub use validation_dependencies::RunnerValidationDependencySyncOutput;
pub mod runners;
mod worker;
pub(crate) mod workload;
mod workspace;
pub(crate) use extension_materialization::materialize_lab_job_extension_overlays;
pub(crate) use workspace::copy_snapshot_to_directory;
pub use workspace::register_workspace_snapshot_provider;
#[cfg(test)]
pub(crate) use workspace::verify_lab_workspace_from_env;

/// Compute the same controller workspace identity used by Lab snapshot
/// materialization before a detached command is admitted.
pub fn controller_workspace_materialization_identity(
    path: &std::path::Path,
) -> homeboy_core::error::Result<String> {
    workspace::snapshot_identity(path, &[], &[])
}

/// The generic Lab replay contract is an artifact digest, not a mutable
/// controller-workspace observation. Callers which only have the historical
/// `snapshot:` identity must recover through a fresh run.
pub fn generic_lab_replay_artifact_identity(
    path: &std::path::Path,
) -> homeboy_core::error::Result<String> {
    let mut excludes = workspace::DEFAULT_EXCLUDES
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    for exclude in homeboy_core::source_snapshot::policy_for_path(path).sync_excludes {
        if !excludes.contains(&exclude) {
            excludes.push(exclude);
        }
    }
    workspace::replay_artifact_identity(path, &excludes)
}

pub fn generic_lab_replay_artifact_identity_for_runner(
    runner_id: &str,
    path: &std::path::Path,
) -> homeboy_core::error::Result<String> {
    let runner = load(runner_id)?;
    workspace::replay_artifact_identity(path, &generic_lab_replay_transfer_excludes(&runner, path))
}

pub fn generic_lab_replay_transfer_excludes(
    runner: &Runner,
    path: &std::path::Path,
) -> Vec<String> {
    let mut excludes = workspace::DEFAULT_EXCLUDES
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    for exclude in runner.policy.snapshot_excludes.iter().chain(
        homeboy_core::source_snapshot::policy_for_path(path)
            .sync_excludes
            .iter(),
    ) {
        if !excludes.contains(exclude) {
            excludes.push(exclude.clone());
        }
    }
    let mut excludes =
        workspace::effective_snapshot_excludes(excludes, &runner.policy.snapshot_includes);
    excludes.sort();
    excludes.dedup();
    excludes
}

pub fn generic_lab_replay_identity_excludes(
    identity: &str,
) -> homeboy_core::error::Result<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(identity).map_err(|_| {
        homeboy_core::error::Error::validation_invalid_argument(
            "generic_lab_command_replay",
            "Lab replay uses a legacy artifact identity that does not persist its transfer exclusion policy",
            None,
            Some(vec!["Reissue the command as a new Lab run to create a policy-bound immutable replay artifact.".to_string()]),
        )
    })?;
    if value["schema"] != "homeboy/lab-replay-artifact/v2"
        || !value["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    {
        return Err(homeboy_core::error::Error::validation_invalid_argument(
            "generic_lab_command_replay",
            "Lab replay uses a legacy artifact identity that does not persist its transfer exclusion policy",
            None,
            Some(vec!["Reissue the command as a new Lab run to create a policy-bound immutable replay artifact.".to_string()]),
        ));
    }
    let excludes: Vec<String> =
        serde_json::from_value(value["excludes"].clone()).map_err(|error| {
            homeboy_core::error::Error::internal_json(
                error.to_string(),
                Some("parse replay artifact exclusions".to_string()),
            )
        })?;
    let mut canonical = excludes.clone();
    canonical.sort();
    canonical.dedup();
    if excludes.is_empty()
        || excludes != canonical
        || excludes.iter().any(|exclude| exclude.trim().is_empty())
    {
        return Err(homeboy_core::error::Error::validation_invalid_argument(
            "generic_lab_command_replay",
            "Lab replay artifact has an invalid persisted transfer exclusion policy",
            None,
            Some(vec!["Reissue the command as a new Lab run to create a canonical policy-bound immutable replay artifact.".to_string()]),
        ));
    }
    Ok(excludes)
}
pub(crate) use workspace::update_workspace_resource_lifecycle;
#[cfg(test)]
pub(crate) use workspace::workspace_resource_lifecycle;

pub(crate) use workspace::{
    MaterializedWorkspace, WorkspaceCleanupPolicy, WorkspaceTerminalOutcome,
};

pub use apply::{
    apply_change_artifact, apply_workspace_patch, RunnerWorkspaceApplyOptions,
    RunnerWorkspaceApplyOutput, RunnerWorkspaceApplyStatus,
};
pub use capabilities::{
    evaluate_lab_runner_capabilities_for_inventory, evaluate_lab_runner_capabilities_for_runner,
    prepare_lab_runner_capability, runner_capability_inventory, runner_capability_inventory_until,
    LabRunnerCapabilityContract, LabRunnerGateDecision, LabRunnerGateMode,
    PreparedLabRunnerCapability, RunnerCapabilityInventory, RunnerCapabilityPreflight,
    RunnerRequiredTool, RunnerToolCapabilityRequirement, RunnerToolchainReadinessProbe,
};
pub(crate) use command_path::normalize_runner_command_env_for_homeboy_path;
pub use command_path::preflight_remote_argv_path_translation;
pub(crate) use connection::daemon_endpoint_identity;
pub use connection::{
    close_reconnected_job_log_owner, connect, connect_reverse, connect_with_live_lease_adoption,
    connect_with_orphan_adoption, connect_with_unleased_candidate_reconciliation,
    diagnostic_status, disconnect, disconnect_local_recovery, peer_session_maintenance,
    persisted_status, persisted_status_until, persisted_statuses, reconcile_status,
    reconcile_status_with_outcome, reconcile_terminal_jobs, reconnect_job_log_owner,
    reverse_broker_artifact, reverse_broker_artifact_content, reverse_broker_reconcile,
    runner_artifact_content, status, statuses, statuses_indexed, submit_runner_api_request,
    PeerSessionMaintenanceReport,
};
pub(crate) use connection::{
    configured_runner_homeboy_build_identity, configured_runner_homeboy_handshake_evidence,
    daemon_lab_handoff_capabilities, status_for_admission,
};
pub use runner_probe_gate::observe_runner_capabilities;
mod upgrade_runners;
pub use availability_provider::register as register_runner_availability_provider;
pub use continuation_provider::register as register_runner_continuation_provider;
pub use daemon_exec_driver::register as register_runner_daemon_exec_driver;
pub use evidence::register_runner_evidence_provider;
pub use evidence::runner_artifact_store_token;
pub use evidence::runner_job_log_snapshot_for_session;
pub use evidence::{
    download_remote_artifact, download_remote_artifact_with_intent, is_remote_runner_artifact_path,
    is_reportable_artifact_evidence_path, is_retrievable_runner_artifact,
    mirror_connected_runner_run, mirrored_runner_job_identity, refresh_mirrored_daemon_evidence,
    reportable_artifact_evidence_path, runner_job_log_snapshot, RemoteArtifactDownload,
    RunnerJobLogSnapshot,
};
pub(crate) use execution::exec_with_status_snapshot;
pub use execution::{
    daemon_api_get, daemon_api_post, exec, finish_scheduled_terminal_runner_exec_recovery,
    promote_runner_exec_artifact_dirs, promote_runner_exec_artifact_dirs_in_store,
    promote_runner_exec_artifacts_in_store, promote_runner_exec_summaries_in_store,
    promoted_output, reconcile_runner_generation_after_evidence,
    record_scheduled_terminal_runner_exec_recovery_child_spawn_failure,
    record_scheduled_terminal_runner_exec_recovery_spawn_failure,
    run_scheduled_terminal_runner_exec_recovery, run_scheduled_terminal_runner_exec_recovery_child,
    runner_exec_failure_error, runner_exec_orchestration_provenance,
    runner_exec_structured_summary, runner_job_cancel, runner_job_cancel_for_session,
    runner_job_cancel_projection, schedule_terminal_runner_exec_recovery, RunnerExecDiagnostics,
    RunnerExecMode, RunnerExecOptions, RunnerExecOutput, RunnerExecPromotedOutput,
    RunnerExecRecoveryChildSchedule, RunnerExecRecoveryDiagnostic, RunnerExecStructuredSummary,
};
pub use execution::{RUNNER_HOSTED_EXEC_ENV, RUNNER_ID_ENV, RUNNER_PLACEMENT_RESOLVED_ENV};
pub(crate) use extension_materialization::extension_source_content_hash;
pub(crate) use extension_materialization::{
    materialize_runner_extension_with_env, materialize_runner_extension_with_exec,
    plan_controller_snapshot_extension, RunnerExtensionMaterializationRequest,
    RunnerExtensionMaterializationSource,
};
pub(crate) use git_dependency_materialization::{
    dependency_cache_save, dependency_cache_save_request, materialize_git_dependency,
    RunnerDependencyCacheSaveOutput, RunnerDependencyCacheSaveRequest,
    RunnerGitDependencyMaterializationOptions, RunnerGitDependencyMaterializationOutput,
};
pub use homeboy_refresh::{
    plan_homeboy_binary_refresh, refresh_homeboy_binary, runner_dev_sync,
    HomeboyBinaryRefreshArtifacts, HomeboyBinaryRefreshFailure, HomeboyBinaryRefreshMode,
    HomeboyBinaryRefreshOptions, HomeboyBinaryRefreshOutput, HomeboyBinaryRefreshPlan,
    HomeboyControllerContinuationAction, HomeboyRefreshPhase, HomeboyRefreshReadiness,
    HomeboyRefreshReadinessState, RunnerDevSyncExtensionProvenance, RunnerDevSyncOptions,
    RunnerDevSyncOutput, RunnerDevSyncPlan,
};
pub use job_preparation::register as register_runner_job_preparation_provider;
pub use lab::offload::hydrate_runner_workspace_dependencies;
pub use lab::{
    execute_lab_offload, LabJobOverrides, LabOffloadCommand, LabOffloadOutcome, LabOffloadRequest,
    LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy, LabRunnerSelectionSource,
};
pub use lab_offload_provider::register as register_runner_lab_offload_provider;
pub use lab_selection::prepare_explicit_lab_runner_for_offload;
pub use lab_staging_controller::enable_production_routing as enable_production_lab_staging;
pub use lab_staging_controller::register as register_lab_staging_controller_driver;
pub use lab_staging_controller::{
    load_lab_staging_recipe, persist_lab_staging_recipe, LabStagingRecipe, LabStagingRecipeRef,
    LabStagingRequest,
};
pub use lab_workspace_provenance_provider::register as register_lab_workspace_provenance_provider;
pub use offload_changed_since::{
    lab_offload_changed_since_ref, preflight_lab_offload_changed_since,
    prepare_git_lab_offload_changed_since,
};
pub use offload_metadata::{
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping,
    lab_offload_metadata_with_workspace_mapping_and_lab_runner_workload,
};
pub(crate) use resource_metrics::RunnerCommandProgressSink;
pub use resource_metrics::{
    RunnerResourceGuardLimits, RunnerResourceGuardViolation, RunnerResourceMetrics,
};
pub use rolling_generation::{
    RollingDrainState, RollingGeneration, RollingGenerations, RollingStart,
};
pub use runner_staging_store::{
    register_runner_staging_provider, resolve_runner_staging_transport,
};
pub use runtime_materialization_status::{RunnerBinarySource, RuntimeMaterializationStatus};
pub use session::{
    LabRunnerHandoff, ReverseRunnerConnectOptions, RunnerActiveJobError, RunnerActiveJobSource,
    RunnerActiveJobState, RunnerActiveJobsSnapshot, RunnerAdmissionSummary, RunnerArtifactRef,
    RunnerAvailability, RunnerChangedRuntimePath, RunnerConnectReport,
    RunnerDaemonGenerationStatus, RunnerDaemonVerification, RunnerDisconnectReport,
    RunnerFailureKind, RunnerGenerationJobOwners, RunnerJob, RunnerLeaselessRecoveryContract,
    RunnerLeaselessRecoveryEvidence, RunnerLifecycleOwner, RunnerMutationArtifacts,
    RunnerNamedWorkspaceLease, RunnerRecoveryState, RunnerResult, RunnerSession, RunnerSessionRole,
    RunnerSessionState, RunnerStaleDaemonWarning, RunnerStaleRuntimePath, RunnerStatusReport,
    RunnerTunnelMode, RunnerTunnelProcessStartIdentity, RunnerUnresolvedJobOwner,
    RunnerWorkspaceLease, RunnerWorkspaceLeaseSet,
};
pub use tool_registry::{RunnerToolRegistry, RunnerToolSpec};
pub(crate) use transport::{select_runner_transport, RunnerFileTransfer, RunnerTransport};
pub use upgrade_runners::register as register_runner_upgrade;
pub use worker::{run_reverse_worker, ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput};
pub use workspace::reap_run_workspace;
pub use workspace::{
    hydrate_prepared_workspace_source_snapshot, list_workspaces, plan_workspace_pull,
    prune_workspaces, pull_workspace, resolve_workspace_ref, reuse_compatible_snapshot_workspace,
    sync_workspace, update_workspace, verify_workspace_ref_hydration_source, workspace_snapshots,
    ByteFileCounts, RunnerWorkspaceCurrentSummary, RunnerWorkspaceListEntry,
    RunnerWorkspaceListOutput, RunnerWorkspaceMaterializationContract,
    RunnerWorkspaceMaterializationPlan, RunnerWorkspaceOutputPaths, RunnerWorkspacePruneEntry,
    RunnerWorkspacePruneOptions, RunnerWorkspacePruneOutput, RunnerWorkspacePruneSkippedEntry,
    RunnerWorkspacePullOptions, RunnerWorkspacePullOutput, RunnerWorkspacePullPlan,
    RunnerWorkspaceRefResolution, RunnerWorkspaceSnapshotAppliedFilters,
    RunnerWorkspaceSnapshotEntry, RunnerWorkspaceSnapshotFilters, RunnerWorkspaceSnapshotsOutput,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
    RunnerWorkspaceUpdateOptions, RunnerWorkspaceUpdateOutput, WorkspaceContentManifest,
    WorkspaceContentManifestEntry,
};
pub(crate) use workspace::{
    workspace_content_hash, workspace_content_hash_algorithm,
    workspace_content_manifest_for_policy, WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
};
pub use workspace_root_provider::register as register_runner_workspace_root_provider;

use homeboy_runner_contract::RunnerKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    #[serde(skip_deserializing, default)]
    pub id: String,
    pub kind: RunnerKind,
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(flatten)]
    pub settings: RunnerSettings,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secret_env: HashMap<String, RunnerSecretEnvRef>,
    #[serde(default)]
    pub resources: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "RunnerPolicy::is_empty")]
    pub policy: RunnerPolicy,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunnerSpec {
    pub workspace_root: Option<String>,
    pub settings: RunnerSettings,
    pub env: HashMap<String, String>,
    pub resources: HashMap<String, Value>,
    pub security: server::RunnerSecurityConfig,
}

/// A value-free migration plan for legacy credential-shaped runner env entries.
/// It is safe to show in diagnostics and records the durable secret reference
/// that an apply operation will create.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerSecretEnvMigrationPlan {
    pub runner_id: String,
    pub entries: Vec<RunnerSecretEnvMigrationEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerSecretEnvMigrationEntry {
    pub key: String,
    pub location: String,
    pub secret: String,
}

impl RunnerSecretEnvMigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl RunnerSpec {
    pub fn from_runner(runner: &Runner) -> Self {
        Self {
            workspace_root: runner.workspace_root.clone(),
            settings: runner.settings.clone(),
            env: runner.env.clone(),
            resources: runner.resources.clone(),
            security: server::RunnerSecurityConfig {
                secret_env: runner.secret_env.clone(),
                policy: runner.policy.clone(),
            },
        }
    }

    pub fn into_runner(self, id: String, kind: RunnerKind, server_id: Option<String>) -> Runner {
        Runner {
            id,
            kind,
            server_id,
            workspace_root: self.workspace_root,
            settings: self.settings,
            env: self.env,
            secret_env: self.security.secret_env,
            resources: self.resources,
            policy: self.security.policy,
        }
    }

    pub fn into_server_runner(self) -> ServerRunner {
        ServerRunner {
            workspace_root: self.workspace_root,
            settings: self.settings,
            env: self.env,
            resources: self.resources,
            security: self.security,
        }
    }

    pub fn effective_env(&self) -> HashMap<String, String> {
        let mut env = self.env.clone();
        normalize_runner_command_env_for_homeboy_path(
            &mut env,
            self.settings.homeboy_path.as_deref(),
        );
        env
    }
}

impl From<ServerRunner> for RunnerSpec {
    fn from(runner: ServerRunner) -> Self {
        Self {
            workspace_root: runner.workspace_root,
            settings: runner.settings,
            env: runner.env,
            resources: runner.resources,
            security: runner.security,
        }
    }
}

pub(crate) fn remote_runner_homeboy_path<'a>(runner: &'a Runner, context: &str) -> Result<&'a str> {
    match runner.kind {
        RunnerKind::Local => Ok(runner.settings.homeboy_path.as_deref().unwrap_or("homeboy")),
        RunnerKind::Ssh => runner
            .settings
            .homeboy_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| missing_remote_homeboy_path_error(runner, context)),
    }
}

pub fn runner_homeboy_path_for_command(id: &str, context: &str) -> Result<String> {
    let runner = load(id)?;
    remote_runner_homeboy_path(&runner, context).map(str::to_string)
}

fn missing_remote_homeboy_path_error(runner: &Runner, context: &str) -> Error {
    Error::validation_invalid_argument(
        "homeboy_path",
        format!(
            "{context} requires runner `{}` to configure runner.homeboy_path; refusing to use bare `homeboy` on a remote runner because PATH may select a stale binary",
            runner.id
        ),
        Some(runner.id.clone()),
        Some(vec![
            format!(
                "Configure an explicit remote Homeboy binary path for runner `{}` with `homeboy runner merge {}` or `homeboy runner refresh-homeboy {}`.",
                runner.id, runner.id, runner.id
            ),
            "Use an absolute path to the runner-side Homeboy binary, then reconnect the runner daemon.".to_string(),
        ]),
    )
}

impl ConfigEntity for Runner {
    const ENTITY_TYPE: &'static str = "runner";
    const DIR_NAME: &'static str = "runners";

    fn id(&self) -> &str {
        &self.id
    }

    fn set_id(&mut self, id: String) {
        self.id = id;
    }

    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::runner_not_found(id, suggestions)
    }

    /// The backing server is loaded from the *same* root the runner is being
    /// written to. Loading it from the process root would accept an SSH runner
    /// into one installation on the strength of a server that only exists in
    /// another.
    fn validate_in_root(&self, config_root: &std::path::Path) -> Result<()> {
        if matches!(self.kind, RunnerKind::Ssh) {
            let server_id = self.server_id.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "server_id",
                    "SSH runners require server_id",
                    None,
                    None,
                )
            })?;
            server::load_in_root(config_root, server_id)?;
        }

        server::validate_runner_settings(&self.settings, "concurrency_limit", None)?;
        server::validate_runner_env(&self.env, "env")?;

        Ok(())
    }

    fn dependents_in_root(_config_root: &std::path::Path, _id: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

/// Register `Runner` as a config entity so it participates in config-id/alias
/// collision detection. Called once at startup, mirroring how feature crates
/// (e.g. the tunnel crate) register their own entities; when the runner crate
/// is extracted this call moves into that crate's startup registration.
pub fn register_runner_config_entity() {
    config::register_config_entity::<Runner>();
}

pub fn load(id: &str) -> Result<Runner> {
    load_in_roots(&homeboy_core::paths::PathRoots::from_environment()?, id)
}

/// [`load`] against an explicitly injected root.
///
/// The registry probe, the server-backed fallback, and the alias retry all have
/// to describe the same installation: a runner found in one home and a server
/// record read from another is not a runner definition, it is two (#7505).
pub fn load_in_roots(roots: &homeboy_core::paths::PathRoots, id: &str) -> Result<Runner> {
    if id == "local" {
        return Ok(builtin_local_runner());
    }

    if let Ok(runner) = config::load_in_root::<Runner>(roots.config(), id) {
        if runner.kind == RunnerKind::Local {
            return Ok(runner);
        }
    }

    if let Ok(runner) = load_server_runner_in_root(roots.config(), id) {
        return Ok(runner);
    }

    if let Some(runner_id) = resolve_reserved_runner_alias(id)? {
        return load_server_runner_in_root(roots.config(), &runner_id);
    }

    if let Some(runner) = execution_context_runner(id) {
        return Ok(runner);
    }

    Err(Error::runner_not_found(
        id.to_string(),
        runner_suggestions(id),
    ))
}

fn resolve_reserved_runner_alias(id: &str) -> Result<Option<String>> {
    if !id.eq_ignore_ascii_case("lab") {
        return Ok(None);
    }

    let lab_runner_ids = configured_lab_runner_ids()?;
    if lab_runner_ids.is_empty() {
        return Ok(None);
    }

    if let Some(runner_id) = resolve_default_lab_runner()? {
        return Ok(Some(runner_id));
    }

    if let Some(preferred) = defaults::load_config().lab.preferred_runner {
        if lab_runner_ids
            .iter()
            .any(|runner_id| runner_id == &preferred)
        {
            return Ok(Some(preferred));
        }
    }

    if lab_runner_ids.len() == 1 {
        return Ok(Some(lab_runner_ids[0].clone()));
    }

    Err(Error::runner_not_found(id.to_string(), lab_runner_ids))
}

fn configured_lab_runner_ids() -> Result<Vec<String>> {
    let mut ids: Vec<String> = list()?
        .into_iter()
        .filter(|runner| runner.kind == RunnerKind::Ssh)
        .map(|runner| runner.id)
        .collect();
    ids.sort();
    Ok(ids)
}

fn runner_suggestions(id: &str) -> Vec<String> {
    list()
        .map(|runners| {
            let id_lower = id.to_lowercase();
            let mut matches: Vec<String> = runners
                .into_iter()
                .filter_map(|runner| {
                    let runner_id_lower = runner.id.to_lowercase();
                    (runner_id_lower.starts_with(&id_lower)
                        || runner_id_lower.ends_with(&id_lower)
                        || runner_id_lower.contains(&format!("-{id_lower}"))
                        || (id_lower.starts_with("lab") && runner_id_lower.contains("lab")))
                    .then_some(runner.id)
                })
                .collect();
            matches.sort();
            matches.dedup();
            matches.truncate(3);
            matches
        })
        .unwrap_or_default()
}

fn builtin_local_runner() -> Runner {
    Runner {
        id: "local".to_string(),
        kind: RunnerKind::Local,
        server_id: None,
        workspace_root: std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string()),
        settings: server::RunnerSettings::default(),
        env: HashMap::new(),
        secret_env: HashMap::new(),
        resources: HashMap::new(),
        policy: server::RunnerPolicy::default(),
    }
}

/// Materialize the current Lab execution host as a local runner only when a
/// nested command asks for the runner that selected this process. The configured
/// registry always wins, so controller-side and ordinary local lookup retain
/// their existing behavior.
fn execution_context_runner(id: &str) -> Option<Runner> {
    let runner_id = homeboy_core::resource_policy_context::lab_execution_runner_id()?;
    (runner_id == id).then(|| Runner {
        id: runner_id,
        ..builtin_local_runner()
    })
}

pub fn effective_env(id: &str) -> Result<HashMap<String, String>> {
    let runner = load(id)?;
    Ok(RunnerSpec::from_runner(&runner).effective_env())
}

pub fn list() -> Result<Vec<Runner>> {
    let mut runners: Vec<Runner> = vec![builtin_local_runner()];
    runners.extend(
        config::list::<Runner>()?
            .into_iter()
            .filter(|runner| runner.kind == RunnerKind::Local)
            .filter(|runner| runner.id != "local"),
    );
    runners.extend(
        server::list()?
            .into_iter()
            .filter(|server| server.runner.is_some())
            .map(|server| runner_from_server(&server.id, server.runner.expect("checked above"))),
    );
    if let Some(runner) = homeboy_core::resource_policy_context::lab_execution_runner_id()
        .and_then(|runner_id| execution_context_runner(&runner_id))
        .filter(|runner| !runners.iter().any(|configured| configured.id == runner.id))
    {
        runners.push(runner);
    }
    runners.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(runners)
}

pub fn resolve_default_lab_runner() -> Result<Option<String>> {
    Ok(lab_runner_readiness()?.selected_runner_id)
}

/// A generic, live inventory of Lab-capable runners. Consumers use this rather
/// than reducing runner state to an optional default ID, which loses the reason
/// a connected runner was not selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerReadiness {
    pub state: LabRunnerReadinessState,
    pub selected_runner_id: Option<String>,
    pub available_runner_ids: Vec<String>,
    pub reasons: Vec<String>,
    pub remediation_commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerReadinessState {
    Absent,
    ConnectedReady,
    ConnectedIneligible,
    Stale,
    CapacityBlocked,
    Disconnected,
}

// A detached admission retry must not turn an operator command into an
// unbounded sweep across every configured remote runner.
const DETACHED_QUEUE_REFRESH_LIMIT: usize = 3;

impl LabRunnerReadinessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::ConnectedReady => "connected_ready",
            Self::ConnectedIneligible => "connected_ineligible",
            Self::Stale => "stale",
            Self::CapacityBlocked => "capacity_blocked",
            Self::Disconnected => "disconnected",
        }
    }
}

pub fn lab_runner_readiness() -> Result<LabRunnerReadiness> {
    let preferred = defaults::load_config().lab.preferred_runner;
    let candidates: Vec<_> = list()?
        .into_iter()
        .filter(|runner| runner.kind == RunnerKind::Ssh)
        .filter_map(|runner| {
            let status = status(&runner.id).ok()?;
            let capabilities_ready = runner_capability_inventory(&runner.id)
                .is_ok_and(|inventory| !inventory.runtime_ids.is_empty());
            let mode = status
                .session
                .as_ref()
                .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
            Some(lab_runner_admission_candidate(
                &runner.id,
                mode,
                runner.settings.concurrency_limit,
                &status,
                capabilities_ready,
                lab::offload::metadata::require_exact_runner_version(&runner.settings),
            ))
        })
        .collect();
    Ok(lab_runner_readiness_from_candidates(
        preferred.as_deref(),
        candidates,
    ))
}

/// Admit one connected runner for a bounded readiness repair without treating
/// its missing capability as workload readiness. Transport freshness, idle
/// ownership, and active-job evidence remain mandatory.
pub fn runner_readiness_repair_admitted(runner_id: &str) -> Result<bool> {
    let runner = load(runner_id)?;
    let status = status(runner_id)?;
    let mode = status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
    let candidate = lab_runner_admission_candidate(
        runner_id,
        mode,
        runner.settings.concurrency_limit,
        &status,
        false,
        lab::offload::metadata::require_exact_runner_version(&runner.settings),
    );
    Ok(candidate.connected
        && !candidate.stale_daemon
        && candidate.admission_fresh
        && candidate.active_jobs_available
        && candidate.active_jobs == 0)
}

/// Refresh a bounded set of connected-runner admission observations before a
/// hot controller rejects a portable workload. This is read-only: it neither
/// reconnects sessions nor settles jobs, so it is a targeted alternative to a
/// full runner doctor while still exposing the current admission predicate.
pub fn refresh_lab_runner_readiness_for_admission() -> Result<LabRunnerReadiness> {
    let preferred = defaults::load_config().lab.preferred_runner;
    let mut runner_ids = configured_lab_runner_ids()?;
    runner_ids.sort_by_key(|runner_id| {
        (
            Some(runner_id.as_str()) != preferred.as_deref(),
            runner_id.clone(),
        )
    });
    let deadline = std::time::Instant::now() + readonly_probe::readonly_probe_timeout();
    let mut observations = Vec::new();
    for runner_id in runner_ids.into_iter().take(DETACHED_QUEUE_REFRESH_LIMIT) {
        observations.push(observe_lab_runner_admission_candidate(&runner_id, deadline));
    }
    lab_runner_readiness_from_refresh_observations(preferred.as_deref(), observations)
}

fn observe_lab_runner_admission_candidate(
    runner_id: &str,
    deadline: std::time::Instant,
) -> Result<DefaultLabRunnerCandidate> {
    let runner = load(runner_id)?;
    runner_probe_gate::invalidate_runner_probes(runner_id);
    let status = runner_admission_snapshot_until(runner_id, deadline)?.status;
    let capabilities_ready = runner_capability_inventory_until(runner_id, deadline)?
        .runtime_ids
        .contains("homeboy");
    let mode = status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
    Ok(lab_runner_admission_candidate(
        runner_id,
        mode,
        runner.settings.concurrency_limit,
        &status,
        capabilities_ready,
        lab::offload::metadata::require_exact_runner_version(&runner.settings),
    ))
}

fn lab_runner_admission_candidate(
    runner_id: &str,
    mode: RunnerTunnelMode,
    capacity: Option<usize>,
    status: &RunnerStatusReport,
    capabilities_ready: bool,
    exact_version: bool,
) -> DefaultLabRunnerCandidate {
    let admission_warning = status.admission_blocking_stale_daemon().filter(|_| {
        lab::offload::metadata::lab_runner_homeboy_has_blocking_status_drift(status, exact_version)
    });
    DefaultLabRunnerCandidate {
        id: runner_id.to_string(),
        mode,
        connected: status.connected,
        capacity,
        stale_daemon: admission_warning.is_some(),
        unverified_daemon: status.unverified_daemon().is_some(),
        admission_fresh: lab::offload::metadata::lab_runner_daemon_fresh_for_admission(
            status,
            exact_version,
        ),
        admission_failure_reason: admission_warning
            .map(|warning| warning.mismatch_predicate.to_string()),
        admission_remediation: admission_warning
            .and_then(|_| status.admission_action())
            .map(|action| action.render_command()),
        active_jobs: status.active_job_count.max(status.active_jobs.len()),
        active_jobs_available: status.active_job_state == RunnerActiveJobState::Available,
        capabilities_ready,
    }
}

fn lab_runner_readiness_from_refresh_observations(
    preferred: Option<&str>,
    observations: Vec<Result<DefaultLabRunnerCandidate>>,
) -> Result<LabRunnerReadiness> {
    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for observation in observations {
        match observation {
            Ok(candidate) => candidates.push(candidate),
            Err(error) => failures.push(serde_json::json!({
                "code": error.code.as_str(),
                "message": error.message,
                "details": error.details,
            })),
        }
    }
    let observed_candidate = !candidates.is_empty();
    let readiness = lab_runner_readiness_from_candidates(preferred, candidates);
    if observed_candidate || failures.is_empty() {
        return Ok(readiness);
    }
    let timeout = failures.iter().all(|failure| {
        failure.get("code").and_then(serde_json::Value::as_str)
            == Some(homeboy_core::error::ErrorCode::RemoteCommandTimeout.as_str())
    });
    Err(homeboy_core::error::Error::new(
        if timeout {
            homeboy_core::error::ErrorCode::RemoteCommandTimeout
        } else {
            homeboy_core::error::ErrorCode::RunnerLabTransportFailure
        },
        "No Lab runner completed the bounded admission refresh",
        serde_json::json!({
            "runner_failures": failures,
            "observed_readiness_state": readiness.state.as_str(),
            "available_runner_ids": readiness.available_runner_ids,
        }),
    ))
}

/// Reconcile the configured Lab inventory before admitting a detached Cook to a
/// reverse runner's durable capacity queue. This is deliberately narrower than
/// default selection: only a connected, healthy reverse runner at capacity can
/// accept a broker-queued job without choosing controller-local execution.
///
/// `reconcile_status` owns bounded remote status reads and terminal-job
/// settlement. Refreshing here prevents a stale controller projection from
/// refusing work that a just-freed runner can accept.
pub fn refresh_detached_queue_runner() -> Result<Option<String>> {
    let mut runner_ids = configured_lab_runner_ids()?;
    runner_ids.sort();

    for runner_id in runner_ids.into_iter().take(DETACHED_QUEUE_REFRESH_LIMIT) {
        let runner = load(&runner_id)?;
        let status = connection::reconcile_status(&runner_id)?;
        let mode = status
            .session
            .as_ref()
            .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
        let capabilities_ready = runner_capability_inventory(&runner_id)
            .is_ok_and(|inventory| !inventory.runtime_ids.is_empty());
        let candidate = lab_runner_admission_candidate(
            &runner_id,
            mode,
            runner.settings.concurrency_limit,
            &status,
            capabilities_ready,
            lab::offload::metadata::require_exact_runner_version(&runner.settings),
        );
        if let Some(runner_id) = detached_queue_runner_from_candidates([candidate]) {
            return Ok(Some(runner_id));
        }
    }

    Ok(None)
}

/// Reconcile and classify one explicitly pinned reverse runner for detached
/// capacity admission. This never substitutes another configured runner.
pub fn refresh_explicit_detached_queue_runner(runner_id: &str) -> Result<bool> {
    let runner = load(runner_id)?;
    let status = connection::reconcile_status(runner_id)?;
    let mode = status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
    let capabilities_ready = runner_capability_inventory(runner_id)
        .is_ok_and(|inventory| !inventory.runtime_ids.is_empty());
    Ok(
        detached_queue_runner_from_candidates([lab_runner_admission_candidate(
            runner_id,
            mode,
            runner.settings.concurrency_limit,
            &status,
            capabilities_ready,
            lab::offload::metadata::require_exact_runner_version(&runner.settings),
        )])
        .as_deref()
            == Some(runner_id),
    )
}

fn detached_queue_runner_from_candidates(
    candidates: impl IntoIterator<Item = DefaultLabRunnerCandidate>,
) -> Option<String> {
    candidates.into_iter().find_map(|candidate| {
        let at_capacity = candidate
            .capacity
            .is_some_and(|capacity| candidate.active_jobs >= capacity);
        (candidate.mode == RunnerTunnelMode::Reverse
            && candidate.connected
            && !candidate.stale_daemon
            && candidate.admission_fresh
            && candidate.active_jobs_available
            && candidate.capabilities_ready
            && at_capacity)
            .then_some(candidate.id)
    })
}

fn lab_runner_readiness_from_candidates(
    preferred: Option<&str>,
    candidates: Vec<DefaultLabRunnerCandidate>,
) -> LabRunnerReadiness {
    let selected_runner_id =
        resolve_default_lab_runner_from_candidates(preferred, candidates.clone());
    let availability: Vec<_> = candidates
        .iter()
        .map(DefaultLabRunnerCandidate::availability)
        .collect();
    let available_runner_ids: Vec<_> = availability
        .iter()
        .filter(|runner| runner.accepts_jobs)
        .map(|runner| runner.runner_id.clone())
        .collect();
    let reasons: Vec<_> = availability
        .iter()
        .flat_map(|runner| runner.reasons.iter().cloned())
        .collect();
    let state = if candidates.is_empty() {
        LabRunnerReadinessState::Absent
    } else if !available_runner_ids.is_empty() {
        LabRunnerReadinessState::ConnectedReady
    } else if candidates
        .iter()
        .any(|candidate| candidate.stale_daemon || !candidate.admission_fresh)
    {
        LabRunnerReadinessState::Stale
    } else if reasons.iter().any(|reason| reason == "capacity_reached") {
        LabRunnerReadinessState::CapacityBlocked
    } else if candidates.iter().any(|candidate| candidate.connected) {
        LabRunnerReadinessState::ConnectedIneligible
    } else {
        LabRunnerReadinessState::Disconnected
    };
    let remediation_commands = match state {
        LabRunnerReadinessState::Absent => vec!["homeboy runner connect <runner-id>".to_string()],
        LabRunnerReadinessState::ConnectedReady => Vec::new(),
        LabRunnerReadinessState::ConnectedIneligible | LabRunnerReadinessState::CapacityBlocked => {
            candidates
                .iter()
                .map(|candidate| format!("homeboy runner status {}", candidate.id))
                .collect()
        }
        LabRunnerReadinessState::Stale => candidates
            .iter()
            .filter(|candidate| candidate.stale_daemon || !candidate.admission_fresh)
            .map(|candidate| {
                candidate.admission_remediation.clone().unwrap_or_else(|| {
                    format!("homeboy runner doctor {} --scope lab-offload", candidate.id)
                })
            })
            .collect(),
        LabRunnerReadinessState::Disconnected => candidates
            .iter()
            .map(|candidate| format!("homeboy runner connect {}", candidate.id))
            .collect(),
    };
    LabRunnerReadiness {
        state,
        selected_runner_id,
        available_runner_ids,
        reasons,
        remediation_commands,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultLabRunnerCandidate {
    id: String,
    mode: RunnerTunnelMode,
    connected: bool,
    capacity: Option<usize>,
    /// A *proven* compatibility mismatch, or a probe that failed on a runner
    /// that has one. Hard-fences selection.
    stale_daemon: bool,
    /// The runner has no controller-side verification path at all, so its
    /// freshness was never established. Deliberately not a fence — see
    /// `DefaultLabRunnerCandidate::readiness`.
    unverified_daemon: bool,
    admission_fresh: bool,
    admission_failure_reason: Option<String>,
    admission_remediation: Option<String>,
    active_jobs: usize,
    active_jobs_available: bool,
    capabilities_ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DefaultLabRunnerReadiness {
    eligible: bool,
    score: i32,
}

/// How far an unverified runner ranks below an otherwise identical verified
/// one. Large enough that any verified candidate wins, small enough that the
/// score stays positive so the runner remains eligible.
const UNVERIFIED_DAEMON_SELECTION_PENALTY: i32 = 50;

impl DefaultLabRunnerCandidate {
    fn availability(&self) -> RunnerAvailability {
        let mut availability = RunnerAvailability::from_status_parts(
            self.id.clone(),
            self.connected,
            self.stale_daemon,
            self.active_jobs,
            if self.active_jobs_available {
                &RunnerActiveJobState::Available
            } else {
                &RunnerActiveJobState::Unavailable
            },
            self.capacity,
        );
        if !self.admission_fresh && !self.stale_daemon {
            availability
                .reasons
                .push("daemon_freshness_unavailable".to_string());
            availability.accepts_jobs = false;
        }
        if let Some(reason) = &self.admission_failure_reason {
            availability.reasons.push(reason.clone());
        }
        if !self.active_jobs_available
            && !availability
                .reasons
                .iter()
                .any(|reason| reason == "active_jobs_unavailable")
        {
            availability
                .reasons
                .push("active_jobs_unavailable".to_string());
            availability.accepts_jobs = false;
        }
        if !self.capabilities_ready {
            availability
                .reasons
                .push("required_capabilities_unavailable".to_string());
            availability.accepts_jobs = false;
        }
        // Named, not fenced: an unverifiable runner keeps accepting work, but
        // an operator reading availability can see that nothing checked it.
        if self.unverified_daemon {
            availability.reasons.push("daemon_unverified".to_string());
        }
        availability
    }

    fn readiness(&self) -> DefaultLabRunnerReadiness {
        // Eligibility for *default* selection is looser than
        // `availability().accepts_jobs`: a disconnected direct-SSH runner is
        // still a valid default target because auto-offload connects it on
        // demand. So gate on the hard, non-connectivity reasons only
        // (capabilities, a failed/absent active-job poll, and capacity), and
        // score connectivity below rather than excluding it. The one
        // connectivity gate that IS hard is a disconnected reverse tunnel,
        // which cannot be woken on demand — handled explicitly below.
        let at_capacity = matches!(self.capacity, Some(capacity) if self.active_jobs >= capacity);
        let capacity_unknown = self.capacity.is_none() && self.active_jobs > 0;
        if !self.capabilities_ready
            || !self.active_jobs_available
            || self.stale_daemon
            || !self.admission_fresh
            || at_capacity
            || capacity_unknown
        {
            return DefaultLabRunnerReadiness {
                eligible: false,
                score: 0,
            };
        }

        if self.mode == RunnerTunnelMode::Reverse && !self.connected {
            return DefaultLabRunnerReadiness {
                eligible: false,
                score: 0,
            };
        }

        let mut score = 10;
        if self.connected {
            score += 100;
        }
        if self.mode == RunnerTunnelMode::DirectSsh {
            score += 5;
        }
        // A runner whose freshness was never established ranks below every
        // verified peer but stays selectable. Excluding it would take every
        // reverse-connected lab out of service the moment the gap is reported,
        // which trades this bug for #11101's — an unverified runner is neither
        // healthy nor stale, and the ordering is where that shows up.
        if self.unverified_daemon {
            score -= UNVERIFIED_DAEMON_SELECTION_PENALTY;
        }
        score -= self.active_jobs.min(50) as i32;

        DefaultLabRunnerReadiness {
            eligible: true,
            score,
        }
    }
}

pub(crate) fn default_lab_runner_availability() -> Result<Vec<RunnerAvailability>> {
    let mut availability: Vec<RunnerAvailability> = list()?
        .into_iter()
        .filter_map(|runner| {
            if runner.kind != RunnerKind::Ssh {
                return None;
            }
            let status = status(&runner.id).ok()?;
            let capabilities_ready = runner_capability_inventory(&runner.id)
                .is_ok_and(|inventory| !inventory.runtime_ids.is_empty());
            let mode = status
                .session
                .as_ref()
                .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone());
            let candidate = lab_runner_admission_candidate(
                &runner.id,
                mode,
                runner.settings.concurrency_limit,
                &status,
                capabilities_ready,
                lab::offload::metadata::require_exact_runner_version(&runner.settings),
            );
            Some(candidate.availability())
        })
        .collect();
    availability.sort_by(|a, b| a.runner_id.cmp(&b.runner_id));
    Ok(availability)
}

fn resolve_default_lab_runner_from_candidates(
    preferred: Option<&str>,
    candidates: impl IntoIterator<Item = DefaultLabRunnerCandidate>,
) -> Option<String> {
    let eligible: Vec<(DefaultLabRunnerCandidate, DefaultLabRunnerReadiness)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let readiness = candidate.readiness();
            readiness.eligible.then_some((candidate, readiness))
        })
        .collect();

    let best_score = eligible
        .iter()
        .map(|(_, readiness)| readiness.score)
        .max()?;
    let best: Vec<DefaultLabRunnerCandidate> = eligible
        .into_iter()
        .filter(|(_, readiness)| readiness.score == best_score)
        .map(|(candidate, _)| candidate)
        .collect();

    if let Some(preferred) = preferred {
        if let Some(candidate) = best.iter().find(|candidate| candidate.id == preferred) {
            return Some(candidate.id.clone());
        }
    }

    (best.len() == 1).then(|| best.into_iter().next().expect("checked len").id)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Runner>> {
    let raw = config::read_json_spec_to_string(json_spec)?;
    let value: Value = config::from_str(&raw)?;

    if let Some(items) = value.as_array() {
        let mut summary = BatchResult::new();
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            if skip_existing && load(&id).is_ok() {
                summary.record_skipped(id);
                continue;
            }

            match create_single_value(item.clone()) {
                Ok(result) => summary.record_created(result.id),
                Err(err) => summary.record_error(id, err.message),
            }
        }
        return Ok(CreateOutput::Bulk(summary));
    }

    Ok(CreateOutput::Single(create_single_value(value)?))
}

/// Inspect a legacy runner configuration without resolving or rendering values.
pub fn secret_env_migration_plan(id: &str) -> Result<RunnerSecretEnvMigrationPlan> {
    let runner = load(id)?;
    Ok(secret_env_migration_plan_for_runner(&runner))
}

fn secret_env_migration_plan_for_runner(runner: &Runner) -> RunnerSecretEnvMigrationPlan {
    let location = if runner.kind == RunnerKind::Ssh {
        "server.runner.env"
    } else {
        "runner.env"
    };
    let mut entries = runner
        .env
        .keys()
        .filter(|key| server::is_likely_secret_env_key(key))
        .map(|key| RunnerSecretEnvMigrationEntry {
            key: key.clone(),
            location: location.to_string(),
            secret: runner_secret_name(&runner.id, key),
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.key.cmp(&right.key));
    RunnerSecretEnvMigrationPlan {
        runner_id: runner.id.clone(),
        entries,
    }
}

/// Move legacy plaintext env values into the OS keychain and atomically replace
/// each persisted value with a `secret_env` reference. Existing mappings are
/// never overwritten; any newly created mappings are removed if config save
/// fails, leaving the legacy config unchanged.
pub fn apply_secret_env_migration(id: &str) -> Result<RunnerSecretEnvMigrationPlan> {
    let plan = secret_env_migration_plan(id)?;
    if plan.is_empty() {
        return Ok(plan);
    }
    let mut runner = load(id)?;
    let mut created: Vec<String> = Vec::new();
    for entry in &plan.entries {
        if homeboy_core::keychain::get("runner", &entry.secret)?.is_some() {
            return Err(Error::validation_invalid_argument(
                "secret_env",
                format!("migration secret reference `{}` already exists", entry.secret),
                Some(entry.key.clone()),
                Some(vec!["Choose a different secret reference or remove the existing mapping before applying this migration.".to_string()]),
            ));
        }
        let value = runner
            .env
            .get(&entry.key)
            .expect("plan key exists in runner env");
        if let Err(error) = agent_task_secrets::set_keychain_secret(
            &entry.secret,
            value,
            Some("runner"),
            Some(&entry.secret),
        ) {
            for secret in created {
                let _ = agent_task_secrets::remove_secret_mapping(&secret, true);
            }
            return Err(error);
        }
        created.push(entry.secret.clone());
    }

    for entry in &plan.entries {
        runner.env.remove(&entry.key);
        runner.secret_env.insert(
            entry.key.clone(),
            RunnerSecretEnvRef {
                env: None,
                file: None,
                secret: Some(entry.secret.clone()),
            },
        );
    }

    let saved = match runner.kind {
        RunnerKind::Local => config::save(&runner),
        RunnerKind::Ssh => {
            let mut server = server::load(&runner.id)?;
            server.runner = Some(RunnerSpec::from_runner(&runner).into_server_runner());
            server::save(&server)
        }
    };
    if let Err(error) = saved {
        for secret in created {
            let _ = agent_task_secrets::remove_secret_mapping(&secret, true);
        }
        return Err(error);
    }
    Ok(plan)
}

fn runner_secret_name(runner_id: &str, key: &str) -> String {
    format!("runner/{runner_id}/{key}")
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let raw = config::read_json_spec_to_string(json_spec)?;
    let parsed: Value = config::from_str(&raw)?;

    if parsed.is_array() {
        return Ok(MergeOutput::Bulk(config::merge_batch_from_json::<Runner>(
            &raw,
        )?));
    }

    let effective_id = id
        .map(String::from)
        .or_else(|| parsed.get("id").and_then(Value::as_str).map(String::from))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                "Provide runner ID as argument or in JSON body",
                None,
                None,
            )
        })?;

    if let Ok(runner) = config::load::<Runner>(&effective_id) {
        if runner.kind == RunnerKind::Local {
            return Ok(MergeOutput::Single(config::merge_from_json::<Runner>(
                Some(&effective_id),
                &raw,
                replace_fields,
            )?));
        }
    }

    Ok(MergeOutput::Single(merge_server_runner(
        &effective_id,
        parsed,
        replace_fields,
    )?))
}

pub fn delete_safe(id: &str) -> Result<()> {
    if let Ok(runner) = config::load::<Runner>(id) {
        if runner.kind == RunnerKind::Local {
            return config::delete_safe::<Runner>(id);
        }
    }

    let mut server = server::load(id)?;
    if server.runner.is_none() {
        return Err(Error::runner_not_found(id.to_string(), vec![]));
    }
    server.runner = None;
    server::save(&server)
}

pub fn exists(id: &str) -> bool {
    config::load::<Runner>(id)
        .map(|runner| runner.kind == RunnerKind::Local)
        .unwrap_or(false)
        || load_server_runner(id).is_ok()
}

pub fn enable_server_runner(server_id: &str, patch: Value) -> Result<Runner> {
    let mut server = server::load(server_id)?;
    let mut runner = server.runner.unwrap_or_default();
    let patch = strip_runner_identity_fields(patch);
    if !matches!(patch.as_object(), Some(obj) if obj.is_empty()) {
        config::merge_config(&mut runner, patch, &[])?;
    }
    validate_server_runner(server_id, &runner)?;
    let spec = RunnerSpec::from(runner);
    server.runner = Some(spec.clone().into_server_runner());
    server::save(&server)?;
    Ok(runner_from_spec(server_id, spec))
}

fn create_single_value(value: Value) -> Result<CreateResult<Runner>> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument("id", "Missing required field: id", None, None)
        })?
        .to_string();
    let mut runner: Runner = serde_json::from_value(value.clone())
        .map_err(|err| Error::validation_invalid_argument("json", err.to_string(), None, None))?;
    runner.set_id(id.clone());

    match runner.kind {
        RunnerKind::Local => {
            if config::exists::<Runner>(&id) {
                return Err(Error::validation_invalid_argument(
                    "runner.id",
                    format!("runner '{}' already exists", id),
                    Some(id),
                    None,
                ));
            }
            config::validate(&runner)?;
            config::save(&runner)?;
            Ok(CreateResult {
                id: runner.id.clone(),
                entity: runner,
            })
        }
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().unwrap_or(&id);
            if server_id != id {
                return Err(Error::validation_invalid_argument(
                    "server_id",
                    "SSH runner IDs are server IDs; use the server ID as the runner ID",
                    Some(server_id.to_string()),
                    Some(vec![format!(
                        "Run `homeboy runner enable {server_id}` to make server '{server_id}' runner-capable."
                    )]),
                ));
            }
            let entity = enable_server_runner(&id, value)?;
            Ok(CreateResult { id, entity })
        }
    }
}

fn load_server_runner(id: &str) -> Result<Runner> {
    load_server_runner_in_root(&homeboy_core::paths::homeboy()?, id)
}

/// [`load_server_runner`] against an already-resolved config root.
fn load_server_runner_in_root(config_root: &std::path::Path, id: &str) -> Result<Runner> {
    let server = server::load_in_root(config_root, id)?;
    let runner = server
        .runner
        .ok_or_else(|| Error::runner_not_found(id.to_string(), vec![]))?;
    Ok(runner_from_server(id, runner))
}

fn runner_from_server(server_id: &str, runner: ServerRunner) -> Runner {
    runner_from_spec(server_id, RunnerSpec::from(runner))
}

fn runner_from_spec(server_id: &str, spec: RunnerSpec) -> Runner {
    spec.into_runner(
        server_id.to_string(),
        RunnerKind::Ssh,
        Some(server_id.to_string()),
    )
}

pub(crate) fn resolve_runner_secret_env(
    secret_env: &HashMap<String, RunnerSecretEnvRef>,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();
    for (name, source) in secret_env {
        let has_env = source
            .env
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_file = source
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_secret = source
            .secret
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        match (has_env, has_file, has_secret) {
            (true, false, false) => {
                let env_name = source.env.as_deref().unwrap_or_default();
                let value = std::env::var(env_name).map_err(|err| {
                    Error::validation_invalid_argument(
                        "secret_env",
                        format!("failed to read secret env ref for {name}: {err}"),
                        Some(env_name.to_string()),
                        Some(vec![
                            "Set the referenced environment variable on the runner process."
                                .to_string(),
                        ]),
                    )
                })?;
                resolved.insert(name.clone(), value);
            }
            (false, true, false) => {
                let raw_path = source.file.as_deref().unwrap_or_default();
                let path = shellexpand::tilde(raw_path).to_string();
                let value = std::fs::read_to_string(&path).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("read secret env file {path}")),
                    )
                })?;
                resolved.insert(
                    name.clone(),
                    value.trim_end_matches(['\r', '\n']).to_string(),
                );
            }
            (false, false, true) => {
                let secret_name = source.secret.as_deref().unwrap_or_default();
                let values = agent_task_secrets::resolve_secret_env(&[secret_name.to_string()])
                    .map_err(|err| {
                        Error::validation_invalid_argument(
                            "secret_env",
                            format!(
                                "failed to resolve Homeboy secret ref for {name}: {}",
                                err.message
                            ),
                            Some(secret_name.to_string()),
                            Some(vec![
                                "Configure the named Homeboy secret before running this runner job."
                                    .to_string(),
                            ]),
                        )
                    })?;
                let value = values
                    .into_iter()
                    .next()
                    .map(|(_, value)| value)
                    .ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "secret_env",
                            format!("Homeboy secret ref for {name} resolved no value"),
                            Some(secret_name.to_string()),
                            None,
                        )
                    })?;
                resolved.insert(name.clone(), value);
            }
            (false, false, false) => {
                return Err(Error::validation_invalid_argument(
                    "secret_env",
                    format!("secret env ref for {name} requires env, file, or secret"),
                    Some(name.clone()),
                    None,
                ));
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "secret_env",
                    format!(
                        "secret env ref for {name} must use exactly one of env, file, or secret"
                    ),
                    Some(name.clone()),
                    None,
                ));
            }
        }
    }
    Ok(resolved)
}

fn merge_server_runner(
    id: &str,
    mut patch: Value,
    replace_fields: &[String],
) -> Result<MergeResult> {
    let mut server = server::load(id)?;
    let mut runner = server.runner.unwrap_or_default();
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
        obj.remove("kind");
        obj.remove("server_id");
    }
    let result = config::merge_config(&mut runner, patch, replace_fields)?;
    validate_server_runner(id, &runner)?;
    server.runner = Some(runner);
    server::save(&server)?;
    Ok(MergeResult {
        id: id.to_string(),
        updated_fields: result.updated_fields,
    })
}

fn strip_runner_identity_fields(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("id");
        obj.remove("kind");
        obj.remove("server_id");
    }
    value
}

fn validate_server_runner(server_id: &str, runner: &ServerRunner) -> Result<()> {
    server::validate_runner_settings(
        &runner.settings,
        "concurrency_limit",
        Some(server_id.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
    use homeboy_core::test_support;

    #[test]
    fn replay_policy_includes_override_and_persist_all_effective_exclusions() {
        let source = tempfile::tempdir().expect("source");
        std::fs::create_dir(source.path().join("source-output")).expect("source output");
        std::fs::write(source.path().join(".gitignore"), "source-output/\n")
            .expect("source policy");
        for path in [".env", "source-output/generated.txt", "runner-output"] {
            std::fs::write(source.path().join(path), path).expect("write included input");
        }
        let runner: Runner = serde_json::from_value(serde_json::json!({
            "kind": "local",
            "policy": {
                "snapshot_excludes": ["runner-output"],
                "snapshot_includes": [".env", "source-output/generated.txt", "runner-output"],
            },
        }))
        .expect("runner policy");

        let effective = generic_lab_replay_transfer_excludes(&runner, source.path());
        for overridden in [".env", "source-output", "source-output/**", "runner-output"] {
            assert!(!effective.contains(&overridden.to_string()));
        }
        let replay = workspace::immutable_replay_snapshot(source.path(), &effective)
            .expect("replay artifact");
        assert_eq!(
            generic_lab_replay_identity_excludes(&replay.identity).expect("persisted policy"),
            effective
        );
        for path in [".env", "source-output/generated.txt", "runner-output"] {
            assert!(
                replay.path().join(path).is_file(),
                "{path} must be included"
            );
        }
    }

    #[test]
    fn admission_snapshot_keeps_identity_and_rotation_decisions_on_one_ledger() {
        let status = RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: Some(RunnerStaleDaemonWarning::new(
                "homeboy-lab",
                "old".to_string(),
                "current".to_string(),
                None,
                Some("current".to_string()),
            )),
            configured_job_binary_build_identity: None,
            daemon_freshness: Some(DaemonFreshnessReport {
                fresh: false,
                stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
                restartable: true,
                lease_id: Some("lease-current".to_string()),
                pid: Some(1),
                recovery_evidence: None,
                ownership_evidence: None,
                adoption_command: None,
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }),
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        };
        let generations = vec![RunnerDaemonGenerationStatus {
            generation: "lease-current".to_string(),
            admission_owner: true,
            drain_state: RollingDrainState::Admitting,
            active_job_count: 0,
            observed_active_job_count: Some(0),
            active_job_count_authoritative: true,
            job_owner_count: 0,
            run_owner_count: 0,
            artifact_owner_count: 0,
            homeboy_build_identity: None,
            remote_daemon_lease_id: Some("lease-current".to_string()),
            remote_daemon_address: None,
            local_url: None,
        }];

        let snapshot =
            RunnerAdmissionSnapshot::from_status_and_generations(status, generations, Vec::new());

        assert!(!snapshot.summary.daemon_compatible);
        assert!(snapshot.summary.safe_to_rotate);
        assert_eq!(snapshot.summary.draining_generation_count, 0);
        assert_eq!(
            snapshot.summary,
            snapshot.status.admission_summary_with_generations(
                &snapshot.generation_inventory,
                &snapshot.generation_owners,
                snapshot
                    .generation_inventory
                    .iter()
                    .filter(|generation| !generation.admission_owner)
                    .count(),
            )
        );
    }

    #[test]
    fn admission_snapshot_healthy_connected_lab_runner_accepts_jobs() {
        let status = RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: None,
            configured_job_binary_build_identity: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        };

        let snapshot =
            RunnerAdmissionSnapshot::from_status_and_generations(status, Vec::new(), Vec::new());

        assert!(snapshot.summary.accepting_jobs);
    }

    #[test]
    fn admission_snapshot_stale_idle_lab_runner_is_safe_to_rotate_but_not_admitted() {
        let status = RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: Some(RunnerStaleDaemonWarning::new(
                "homeboy-lab",
                "old".to_string(),
                "current".to_string(),
                None,
                Some("current".to_string()),
            )),
            configured_job_binary_build_identity: None,
            daemon_freshness: Some(DaemonFreshnessReport {
                fresh: false,
                stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
                restartable: true,
                lease_id: Some("lease-current".to_string()),
                pid: Some(1),
                recovery_evidence: None,
                ownership_evidence: None,
                adoption_command: None,
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }),
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        };

        let snapshot =
            RunnerAdmissionSnapshot::from_status_and_generations(status, Vec::new(), Vec::new());

        assert!(!snapshot.summary.accepting_jobs);
        assert!(snapshot.summary.safe_to_rotate);
    }

    #[test]
    fn register_runner_config_entity_participates_in_collision_detection() {
        // Runner is no longer hard-coded into core's config-entity seed; it
        // self-registers. Guard the collision invariant: after registration the
        // runner entity type is present, and registration is idempotent.
        register_runner_config_entity();
        register_runner_config_entity();
        let registered = homeboy_core::config::registered_config_entity_types();
        assert_eq!(
            registered
                .iter()
                .filter(|entity_type| **entity_type == Runner::ENTITY_TYPE)
                .count(),
            1,
            "runner entity type must be registered exactly once, got: {registered:?}"
        );
    }

    fn default_lab_candidate(
        id: &str,
        mode: RunnerTunnelMode,
        connected: bool,
    ) -> DefaultLabRunnerCandidate {
        DefaultLabRunnerCandidate {
            id: id.to_string(),
            mode,
            connected,
            capacity: None,
            stale_daemon: false,
            unverified_daemon: false,
            admission_fresh: true,
            admission_failure_reason: None,
            admission_remediation: None,
            active_jobs: 0,
            active_jobs_available: true,
            capabilities_ready: true,
        }
    }

    fn skewed_runner_status(version: String) -> RunnerStatusReport {
        let controller_version = homeboy_product_identity::product_version();
        let controller_build_identity = format!("homeboy {controller_version}+0123456789ab");
        let runner_build_identity = format!("homeboy {version}+0123456789ab");
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            version.clone(),
            version.clone(),
            Some(runner_build_identity.clone()),
            Some(runner_build_identity),
        )
        .with_controller_compatibility(
            "homeboy-lab",
            controller_version.to_string(),
            controller_build_identity,
            false,
            true,
            false,
        );
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::Reverse,
                role: RunnerSessionRole::Runner,
                server_id: None,
                controller_id: Some("controller".to_string()),
                broker_url: Some("http://broker.invalid".to_string()),
                remote_daemon_address: None,
                local_port: None,
                local_url: None,
                tunnel_pid: None,
                tunnel_process_start_identity: None,
                proxy_forward: None,
                remote_daemon_pid: Some(42),
                remote_daemon_lease_id: Some("lease".to_string()),
                homeboy_version: version,
                homeboy_build_identity: None,
                connected_at: "2026-08-24T00:00:00Z".to_string(),
                worker_identity: Some("worker".to_string()),
                worker_pid: Some(43),
                last_seen_at: Some("2026-08-24T00:00:01Z".to_string()),
                leaseless_recovery_evidence: None,
            }),
            stale_daemon: Some(warning),
            configured_job_binary_build_identity: None,
            daemon_freshness: Some(homeboy_core::daemon::DaemonFreshnessReport {
                fresh: false,
                stale_reason_code: Some(
                    homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch,
                ),
                restartable: true,
                lease_id: Some("lease".to_string()),
                pid: Some(42),
                recovery_evidence: None,
                ownership_evidence: None,
                adoption_command: None,
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }),
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "fixture".to_string(),
        }
    }

    fn patch_drift_version() -> String {
        let controller = homeboy_product_identity::product_version();
        let mut parts = controller.split('.');
        let major = parts.next().expect("major");
        let minor = parts.next().expect("minor");
        let patch = parts
            .next()
            .and_then(|part| {
                part.chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or_default();
        format!("{major}.{minor}.{}", patch.wrapping_add(1))
    }

    #[test]
    fn refreshed_stale_inventory_selects_a_newly_ready_runner() {
        let mut stale = default_lab_candidate("lab-a", RunnerTunnelMode::Reverse, true);
        stale.stale_daemon = true;
        let cached = lab_runner_readiness_from_candidates(None, vec![stale]);
        assert_eq!(cached.state, LabRunnerReadinessState::Stale);
        assert!(cached.selected_runner_id.is_none());

        let refreshed = lab_runner_readiness_from_candidates(
            None,
            vec![default_lab_candidate(
                "lab-a",
                RunnerTunnelMode::Reverse,
                true,
            )],
        );
        assert_eq!(refreshed.state, LabRunnerReadinessState::ConnectedReady);
        assert_eq!(refreshed.selected_runner_id.as_deref(), Some("lab-a"));
    }

    #[test]
    fn current_release_refresh_selects_provider_ready_compatible_skewed_runner() {
        let status = skewed_runner_status(patch_drift_version());
        let candidate = lab_runner_admission_candidate(
            "homeboy-lab",
            RunnerTunnelMode::Reverse,
            Some(1),
            &status,
            true,
            false,
        );
        let readiness = lab_runner_readiness_from_candidates(None, vec![candidate]);

        assert_eq!(readiness.state, LabRunnerReadinessState::ConnectedReady);
        assert_eq!(readiness.selected_runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(readiness.available_runner_ids, ["homeboy-lab"]);
    }

    #[test]
    fn current_release_refresh_respects_exact_version_admission() {
        let status = skewed_runner_status(patch_drift_version());
        let candidate = lab_runner_admission_candidate(
            "homeboy-lab",
            RunnerTunnelMode::Reverse,
            Some(1),
            &status,
            true,
            true,
        );
        let readiness = lab_runner_readiness_from_candidates(None, vec![candidate]);

        assert_eq!(readiness.state, LabRunnerReadinessState::Stale);
        assert!(readiness.selected_runner_id.is_none());
        assert!(readiness
            .reasons
            .iter()
            .any(|reason| reason == "controller_version != job_command_binary_version"));
    }

    #[test]
    fn current_release_refresh_names_incompatible_skew_predicate_and_command() {
        let controller = homeboy_product_identity::product_version();
        let mut parts = controller.split('.');
        let major = parts.next().expect("major");
        let minor = parts
            .next()
            .and_then(|part| part.parse::<u64>().ok())
            .expect("minor");
        let mut status = skewed_runner_status(format!("{major}.{}.0", minor.wrapping_add(1)));
        let freshness = status.daemon_freshness.as_mut().expect("fixture freshness");
        freshness.fresh = true;
        freshness.stale_reason_code = None;
        let candidate = lab_runner_admission_candidate(
            "homeboy-lab",
            RunnerTunnelMode::Reverse,
            Some(1),
            &status,
            true,
            false,
        );
        let readiness = lab_runner_readiness_from_candidates(None, vec![candidate]);

        assert_eq!(readiness.state, LabRunnerReadinessState::Stale);
        assert_eq!(
            readiness.reasons,
            [
                "stale_daemon",
                "controller_version != job_command_binary_version"
            ]
        );
        assert_eq!(
            readiness.remediation_commands,
            ["homeboy runner refresh-homeboy homeboy-lab --ref 0123456789ab --reconnect"]
        );
    }

    #[test]
    fn admission_refresh_continues_past_a_failed_runner_to_ready_capacity() {
        let readiness = lab_runner_readiness_from_refresh_observations(
            None,
            vec![
                Err(homeboy_core::error::Error::new(
                    homeboy_core::error::ErrorCode::RemoteCommandTimeout,
                    "lab-a timed out",
                    serde_json::Value::Null,
                )),
                Ok(default_lab_candidate(
                    "lab-b",
                    RunnerTunnelMode::Reverse,
                    true,
                )),
            ],
        )
        .expect("a later ready runner must win admission");

        assert_eq!(readiness.selected_runner_id.as_deref(), Some("lab-b"));
        assert_eq!(readiness.available_runner_ids, ["lab-b"]);
    }

    #[test]
    fn admission_refresh_preserves_authoritative_blocked_observation() {
        let mut full = default_lab_candidate("lab-b", RunnerTunnelMode::Reverse, true);
        full.capacity = Some(1);
        full.active_jobs = 1;
        let readiness = lab_runner_readiness_from_refresh_observations(
            None,
            vec![
                Err(homeboy_core::error::Error::new(
                    homeboy_core::error::ErrorCode::RemoteCommandTimeout,
                    "lab-a timed out",
                    serde_json::Value::Null,
                )),
                Ok(full),
            ],
        )
        .expect("an authoritative observation must not be discarded");

        assert_eq!(readiness.state, LabRunnerReadinessState::CapacityBlocked);
        assert_eq!(readiness.reasons, ["capacity_reached"]);
    }

    #[test]
    fn admission_refresh_reports_timeout_when_every_observation_times_out() {
        let error = lab_runner_readiness_from_refresh_observations(
            None,
            vec![Err(homeboy_core::error::Error::new(
                homeboy_core::error::ErrorCode::RemoteCommandTimeout,
                "lab-a timed out",
                serde_json::Value::Null,
            ))],
        )
        .expect_err("no authoritative observation completed");

        assert_eq!(
            error.code,
            homeboy_core::error::ErrorCode::RemoteCommandTimeout
        );
        assert_eq!(
            error.details["runner_failures"][0]["code"],
            "remote.command_timeout"
        );
    }

    #[test]
    fn hot_controller_admission_refresh_routes_ready_runner_after_stale_cache() {
        let mut stale = default_lab_candidate("lab", RunnerTunnelMode::Reverse, true);
        stale.stale_daemon = true;
        assert_eq!(
            lab_runner_readiness_from_candidates(None, vec![stale]).state,
            LabRunnerReadinessState::Stale
        );

        let ready = lab_runner_readiness_from_candidates(
            None,
            vec![default_lab_candidate(
                "lab",
                RunnerTunnelMode::Reverse,
                true,
            )],
        );
        assert_eq!(ready.state, LabRunnerReadinessState::ConnectedReady);
        assert_eq!(ready.available_runner_ids, ["lab"]);
    }

    #[test]
    fn hot_controller_admission_refresh_reports_connected_blocked_runner() {
        let mut blocked = default_lab_candidate("lab", RunnerTunnelMode::Reverse, true);
        blocked.active_jobs_available = false;
        let blocked = lab_runner_readiness_from_candidates(None, vec![blocked]);
        assert_eq!(blocked.state, LabRunnerReadinessState::ConnectedIneligible);
        assert!(blocked
            .reasons
            .contains(&"active_jobs_unavailable".to_string()));
        assert_eq!(blocked.remediation_commands, ["homeboy runner status lab"]);
    }

    #[test]
    fn hot_controller_admission_refresh_reports_no_runner() {
        let absent = lab_runner_readiness_from_candidates(None, Vec::new());
        assert_eq!(absent.state, LabRunnerReadinessState::Absent);
        assert_eq!(
            absent.remediation_commands,
            ["homeboy runner connect <runner-id>"]
        );
    }

    /// #11106's counter-property. Reverse runners now report an `unverified`
    /// daemon where they previously reported nothing at all. That must rank
    /// them below a verified peer without fencing them, because fencing every
    /// reverse lab is #11101's failure mode wearing this fix's clothes.
    #[test]
    fn unverified_runner_ranks_below_verified_without_dropping_out() {
        let mut unverified =
            default_lab_candidate("lab-unverified", RunnerTunnelMode::Reverse, true);
        unverified.unverified_daemon = true;

        let readiness = unverified.readiness();
        assert!(
            readiness.eligible,
            "an unverified runner is not a stale runner and must stay selectable"
        );
        let verified = default_lab_candidate("lab-verified", RunnerTunnelMode::Reverse, true);
        assert!(
            readiness.score < verified.readiness().score,
            "unverified must rank strictly below an otherwise identical verified runner"
        );
        assert!(readiness.score > 0);

        // Alone, it is still the default: a deprioritized runner is not an
        // absent one.
        assert_eq!(
            resolve_default_lab_runner_from_candidates(None, vec![unverified.clone()]).as_deref(),
            Some("lab-unverified")
        );
        // Against a verified peer, the verified one wins.
        assert_eq!(
            resolve_default_lab_runner_from_candidates(None, vec![unverified.clone(), verified])
                .as_deref(),
            Some("lab-verified")
        );

        // And it is reported, not silent.
        let readiness = lab_runner_readiness_from_candidates(None, vec![unverified]);
        assert_eq!(readiness.state, LabRunnerReadinessState::ConnectedReady);
        assert!(readiness.reasons.contains(&"daemon_unverified".to_string()));
        assert_eq!(readiness.available_runner_ids, ["lab-unverified"]);

        // A *proven* mismatch still fences, so the two states never converge.
        let mut stale = default_lab_candidate("lab-stale", RunnerTunnelMode::Reverse, true);
        stale.stale_daemon = true;
        assert!(!stale.readiness().eligible);
    }

    #[test]
    fn delayed_reverse_runner_is_selected_only_for_durable_capacity_queueing() {
        let mut full_reverse = default_lab_candidate("lab-queue", RunnerTunnelMode::Reverse, true);
        full_reverse.capacity = Some(1);
        full_reverse.active_jobs = 1;
        assert_eq!(
            detached_queue_runner_from_candidates(vec![full_reverse.clone()]).as_deref(),
            Some("lab-queue")
        );

        full_reverse.mode = RunnerTunnelMode::DirectSsh;
        assert_eq!(
            detached_queue_runner_from_candidates(vec![full_reverse]),
            None
        );
    }

    #[test]
    fn lab_runner_readiness_distinguishes_absent_ready_ineligible_stale_and_capacity() {
        let absent = lab_runner_readiness_from_candidates(None, Vec::new());
        assert_eq!(absent.state, LabRunnerReadinessState::Absent);

        let ready = lab_runner_readiness_from_candidates(
            None,
            vec![default_lab_candidate(
                "ready",
                RunnerTunnelMode::DirectSsh,
                true,
            )],
        );
        assert_eq!(ready.state, LabRunnerReadinessState::ConnectedReady);
        assert_eq!(ready.selected_runner_id.as_deref(), Some("ready"));
        assert_eq!(ready.available_runner_ids, ["ready"]);

        let mut ineligible = default_lab_candidate("ineligible", RunnerTunnelMode::DirectSsh, true);
        ineligible.active_jobs = 1;
        let ineligible = lab_runner_readiness_from_candidates(None, vec![ineligible]);
        assert_eq!(
            ineligible.state,
            LabRunnerReadinessState::ConnectedIneligible
        );
        assert!(ineligible.reasons.contains(&"capacity_unknown".to_string()));
        assert_eq!(
            ineligible.remediation_commands,
            ["homeboy runner status ineligible"]
        );

        let mut stale = default_lab_candidate("stale", RunnerTunnelMode::DirectSsh, true);
        stale.stale_daemon = true;
        let stale = lab_runner_readiness_from_candidates(None, vec![stale]);
        assert_eq!(stale.state, LabRunnerReadinessState::Stale);
        assert_eq!(
            stale.remediation_commands,
            ["homeboy runner doctor stale --scope lab-offload"]
        );

        let mut version_mismatch =
            default_lab_candidate("version-mismatch", RunnerTunnelMode::DirectSsh, true);
        version_mismatch.admission_fresh = false;
        version_mismatch.admission_remediation =
            Some("homeboy runner refresh-homeboy version-mismatch --reconnect".to_string());
        let version_mismatch = lab_runner_readiness_from_candidates(None, vec![version_mismatch]);
        assert_eq!(version_mismatch.state, LabRunnerReadinessState::Stale);
        assert_eq!(
            version_mismatch.remediation_commands,
            ["homeboy runner refresh-homeboy version-mismatch --reconnect"]
        );

        let mut lease_missing =
            default_lab_candidate("lease-missing", RunnerTunnelMode::DirectSsh, true);
        lease_missing.admission_fresh = false;
        lease_missing.admission_remediation =
            Some("homeboy runner doctor lease-missing --scope lab-offload".to_string());
        let lease_missing = lab_runner_readiness_from_candidates(None, vec![lease_missing]);
        assert_eq!(lease_missing.state, LabRunnerReadinessState::Stale);
        assert_eq!(
            lease_missing.remediation_commands,
            ["homeboy runner doctor lease-missing --scope lab-offload"]
        );

        let mut full = default_lab_candidate("full", RunnerTunnelMode::DirectSsh, true);
        full.capacity = Some(1);
        full.active_jobs = 1;
        let full = lab_runner_readiness_from_candidates(None, vec![full]);
        assert_eq!(full.state, LabRunnerReadinessState::CapacityBlocked);
        assert!(full.reasons.contains(&"capacity_reached".to_string()));
    }

    fn create_ssh_runner(id: &str) {
        server::create(
            &format!(r#"{{"id":"{id}","host":"192.168.86.63","user":"user"}}"#),
            false,
        )
        .expect("create server");
        create(&format!(r#"{{"id":"{id}","kind":"ssh"}}"#), false)
            .expect("enable runner capability");
    }

    #[test]
    fn runner_registry_persists_local_runner() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "lab-local",
                "kind": "local",
                "workspace_root": "/Users/user/Developer",
                "homeboy_path": "/usr/local/bin/homeboy",
                "daemon": true,
                "concurrency_limit": 2,
                "artifact_policy": "copy",
                "env": {"RUST_LOG": "info"},
                "resources": {"cpu": 8}
            }"#;

            create(spec, false).expect("create runner");
            let runner = load("lab-local").expect("load runner");

            assert_eq!(runner.id, "lab-local");
            assert_eq!(runner.kind, RunnerKind::Local);
            assert_eq!(runner.server_id, None);
            assert_eq!(
                runner.workspace_root.as_deref(),
                Some("/Users/user/Developer")
            );
            assert_eq!(runner.settings.concurrency_limit, Some(2));
            assert_eq!(runner.env.get("RUST_LOG").map(String::as_str), Some("info"));
            assert_eq!(runner.resources.get("cpu"), Some(&Value::from(8)));
        });
    }

    #[test]
    fn builtin_local_runner_does_not_require_registry_entry() {
        test_support::with_isolated_home(|_| {
            let runner = load("local").expect("load local runner");

            assert_eq!(runner.id, "local");
            assert_eq!(runner.kind, RunnerKind::Local);
            assert_eq!(runner.server_id, None);
            assert!(runner.workspace_root.is_some());
        });
    }

    #[test]
    fn runner_lookup_and_list_resolve_current_lab_execution_context() {
        test_support::with_isolated_home(|_| {
            let execution_runner = homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
            let previous = std::env::var_os(execution_runner);
            std::env::set_var(execution_runner, "homeboy-lab");

            let runner = load("homeboy-lab").expect("nested lookup resolves current runner");
            let listed = list().expect("nested runner list resolves current runner");

            match previous {
                Some(value) => std::env::set_var(execution_runner, value),
                None => std::env::remove_var(execution_runner),
            }

            assert_eq!(runner.id, "homeboy-lab");
            assert_eq!(runner.kind, RunnerKind::Local);
            assert!(listed.iter().any(|runner| runner.id == "homeboy-lab"));
        });
    }

    #[test]
    fn runner_registry_persists_trust_policy() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "lab-local",
                "kind": "local",
                "policy": {
                    "accepted_peer_ids": ["extra-chill"],
                    "accepted_peer_fingerprints": ["SHA256:abc123"],
                    "allowed_projects": ["extrachill"],
                    "allowed_commands": ["test", "bench"],
                    "allow_raw_exec": false,
                    "workspace_roots": ["/home/user/Developer"],
                    "artifact_policy": "metadata"
                }
            }"#;

            create(spec, false).expect("create runner");
            let runner = load("lab-local").expect("load runner");

            assert_eq!(runner.policy.accepted_peer_ids, vec!["extra-chill"]);
            assert_eq!(
                runner.policy.accepted_peer_fingerprints,
                vec!["SHA256:abc123"]
            );
            assert_eq!(runner.policy.allowed_projects, vec!["extrachill"]);
            assert_eq!(runner.policy.allowed_commands, vec!["test", "bench"]);
            assert_eq!(runner.policy.allow_raw_exec, Some(false));
            assert_eq!(runner.policy.allow_homeboy_convergence, None);
            assert_eq!(runner.policy.workspace_roots, vec!["/home/user/Developer"]);
            assert_eq!(runner.policy.artifact_policy.as_deref(), Some("metadata"));
            assert!(serde_json::to_value(&runner)
                .expect("serialize legacy policy")
                .pointer("/policy/allow_homeboy_convergence")
                .is_none());
        });
    }

    #[test]
    fn runner_registry_persists_explicit_homeboy_convergence_policy() {
        test_support::with_isolated_home(|_| {
            create(
                r#"{"id":"lab-local","kind":"local","policy":{"allow_homeboy_convergence":true}}"#,
                false,
            )
            .expect("create runner");

            let runner = load("lab-local").expect("load runner");
            assert_eq!(runner.policy.allow_homeboy_convergence, Some(true));
        });
    }

    #[test]
    fn ssh_runner_requires_existing_server() {
        test_support::with_isolated_home(|_| {
            let spec = r#"{
                "id": "remote-lab",
                "kind": "ssh",
                "server_id": "remote-lab",
                "workspace_root": "/srv/homeboy"
            }"#;

            let err = create(spec, false).expect_err("missing server rejects ssh runner");
            assert_eq!(err.code.as_str(), "server.not_found");
        });
    }

    #[test]
    fn ssh_runner_is_server_capability() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"user"}"#,
                false,
            )
            .expect("create server");

            create(
                r#"{
                    "id":"homeboy-lab",
                    "kind":"ssh",
                    "server_id":"homeboy-lab",
                    "workspace_root":"/home/user/Developer",
                    "concurrency_limit":4,
                    "artifact_policy":"copy"
                }"#,
                false,
            )
            .expect("enable runner capability");

            let runner = load("homeboy-lab").expect("load server runner");
            assert_eq!(runner.id, "homeboy-lab");
            assert_eq!(runner.kind, RunnerKind::Ssh);
            assert_eq!(runner.server_id.as_deref(), Some("homeboy-lab"));
            assert_eq!(
                runner.workspace_root.as_deref(),
                Some("/home/user/Developer")
            );
            assert_eq!(runner.settings.concurrency_limit, Some(4));

            let stored_server = server::load("homeboy-lab").expect("load server");
            assert!(stored_server.runner.is_some());
        });
    }

    #[test]
    fn runner_load_keeps_exact_runner_id_first() {
        test_support::with_isolated_home(|_| {
            create_ssh_runner("lab");
            create_ssh_runner("homeboy-lab");

            let runner = load("lab").expect("exact runner id wins over reserved alias");

            assert_eq!(runner.id, "lab");
        });
    }

    #[test]
    fn runner_load_resolves_lab_alias_to_single_ssh_runner() {
        test_support::with_isolated_home(|_| {
            create_ssh_runner("homeboy-lab");

            let runner = load("lab").expect("lab alias resolves to only configured Lab runner");

            assert_eq!(runner.id, "homeboy-lab");
            assert_eq!(runner.kind, RunnerKind::Ssh);
        });
    }

    #[test]
    fn runner_load_resolves_lab_alias_to_configured_preferred_runner() {
        test_support::with_isolated_home(|_| {
            create_ssh_runner("backup-lab");
            create_ssh_runner("homeboy-lab");
            defaults::save_config(&defaults::HomeboyConfig {
                lab: defaults::LabConfig {
                    preferred_runner: Some("homeboy-lab".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .expect("save preferred Lab runner");

            let runner = load("lab").expect("lab alias resolves to preferred Lab runner");

            assert_eq!(runner.id, "homeboy-lab");
            assert_eq!(runner.kind, RunnerKind::Ssh);
        });
    }

    #[test]
    fn runner_load_suggests_single_matching_runner() {
        test_support::with_isolated_home(|_| {
            create_ssh_runner("homeboy-lab");

            let err = load("labx").expect_err("unknown runner id returns suggestions");

            assert_eq!(err.code.as_str(), "runner.not_found");
            let hints = error_hints(&err);
            assert!(hints.contains("homeboy-lab"));
            assert!(!hints.contains("Server not found"));
        });
    }

    #[test]
    fn runner_load_rejects_ambiguous_lab_alias_with_runner_list() {
        test_support::with_isolated_home(|_| {
            create_ssh_runner("homeboy-lab");
            create_ssh_runner("backup-lab");

            let err = load("lab").expect_err("ambiguous Lab alias rejects");

            assert_eq!(err.code.as_str(), "runner.not_found");
            let hints = error_hints(&err);
            assert!(hints.contains("homeboy-lab"));
            assert!(hints.contains("backup-lab"));
        });
    }

    fn error_hints(err: &Error) -> String {
        err.hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn runner_spec_preserves_server_runner_fields_and_effective_env() {
        let server_runner = ServerRunner {
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings {
                homeboy_path: Some("/usr/local/bin/homeboy".to_string()),
                daemon: true,
                concurrency_limit: Some(2),
                artifact_policy: Some("copy".to_string()),
                ..RunnerSettings::default()
            },
            env: HashMap::from([
                ("PATH".to_string(), "/runner/bin".to_string()),
                ("RUST_LOG".to_string(), "info".to_string()),
            ]),
            resources: HashMap::from([("cpu".to_string(), Value::from(4))]),
            security: server::RunnerSecurityConfig {
                secret_env: HashMap::from([(
                    "TOKEN".to_string(),
                    RunnerSecretEnvRef {
                        env: Some("TOKEN".to_string()),
                        file: None,
                        secret: None,
                    },
                )]),
                policy: RunnerPolicy {
                    allowed_commands: vec!["test".to_string()],
                    ..Default::default()
                },
            },
        };

        let spec = RunnerSpec::from(server_runner.clone());
        assert_eq!(spec.clone().into_server_runner(), server_runner);

        let runner = runner_from_spec("lab", spec.clone());
        assert_eq!(runner.id, "lab");
        assert_eq!(runner.kind, RunnerKind::Ssh);
        assert_eq!(runner.server_id.as_deref(), Some("lab"));
        assert_eq!(runner.workspace_root.as_deref(), Some("/srv/homeboy"));
        assert_eq!(runner.settings.concurrency_limit, Some(2));
        assert_eq!(runner.secret_env["TOKEN"].env.as_deref(), Some("TOKEN"));
        assert_eq!(runner.resources.get("cpu"), Some(&Value::from(4)));
        assert_eq!(runner.policy.allowed_commands, vec!["test"]);

        let env = spec.effective_env();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/usr/local/bin:/runner/bin")
        );
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
    }

    #[test]
    fn remote_runner_homeboy_path_allows_local_bare_homeboy() {
        let runner = Runner {
            id: "local".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            settings: RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        };

        assert_eq!(
            remote_runner_homeboy_path(&runner, "test").expect("local fallback"),
            "homeboy"
        );
    }

    #[test]
    fn remote_runner_homeboy_path_requires_ssh_configuration() {
        let runner = Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("lab".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        };

        let err = remote_runner_homeboy_path(&runner, "Lab offload preflight")
            .expect_err("ssh runner without homeboy_path rejects");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert_eq!(err.details["id"], Value::from("lab"));
        assert_eq!(err.details["field"], Value::from("homeboy_path"));
        assert!(err.message.contains("runner.homeboy_path"));
        assert!(err.message.contains("bare `homeboy`"));
    }

    #[test]
    fn runner_settings_validation_rejects_zero_for_both_config_shapes() {
        test_support::with_isolated_home(|_| {
            let local_err = create(
                r#"{"id":"lab-local","kind":"local","concurrency_limit":0}"#,
                false,
            )
            .expect_err("local runner rejects zero concurrency");
            assert_eq!(local_err.code.as_str(), "validation.invalid_argument");
            assert!(local_err.message.contains("concurrency_limit"));

            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"user"}"#,
                false,
            )
            .expect("create server");

            let ssh_err = create(
                r#"{"id":"homeboy-lab","kind":"ssh","concurrency_limit":0}"#,
                false,
            )
            .expect_err("server runner rejects zero concurrency");
            assert_eq!(ssh_err.code.as_str(), "validation.invalid_argument");
            assert!(ssh_err.message.contains("concurrency_limit"));
        });
    }

    #[test]
    fn runner_create_update_and_bulk_import_reject_printable_secret_env() {
        test_support::with_isolated_home(|_| {
            let create_error = create(
                r#"{"id":"secret-local","kind":"local","env":{"OPENCODE_API_KEY":"secret-value"}}"#,
                false,
            )
            .expect_err("create must reject likely secret env");
            assert!(create_error.message.contains("secret_env.OPENCODE_API_KEY"));
            assert!(!create_error.message.contains("secret-value"));

            create(r#"{"id":"public-local","kind":"local"}"#, false).expect("create public runner");
            let update_error = merge(
                Some("public-local"),
                r#"{"env":{"SERVICE_TOKEN":"secret-value"}}"#,
                &[],
            )
            .expect_err("update must reject likely secret env");
            assert!(update_error.message.contains("SERVICE_TOKEN"));
            assert!(!update_error.message.contains("secret-value"));

            let imported = create(
                r#"[{"id":"bulk-secret","kind":"local","env":{"ACCESS_TOKEN":"secret-value"}}]"#,
                false,
            )
            .expect("bulk import returns summary");
            let CreateOutput::Bulk(summary) = imported else {
                panic!("expected bulk summary");
            };
            assert_eq!(summary.errors, 1);
            assert!(load("bulk-secret").is_err());

            server::create(
                r#"{"id":"server-lab","host":"example.test","user":"runner"}"#,
                false,
            )
            .expect("create server");
            let server_runner_error = create(
                r#"{"id":"server-lab","kind":"ssh","env":{"SERVICE_TOKEN":"secret-value"}}"#,
                false,
            )
            .expect_err("server-backed runner must reject likely secret env");
            assert!(server_runner_error.message.contains("SERVICE_TOKEN"));
            assert!(!server_runner_error.message.contains("secret-value"));
        });
    }

    #[test]
    fn config_save_enforces_runner_secret_env_invariant_and_allows_false_positive() {
        test_support::with_isolated_home(|_| {
            let mut runner = Runner {
                id: "local".to_string(),
                kind: RunnerKind::Local,
                server_id: None,
                workspace_root: None,
                settings: RunnerSettings::default(),
                env: HashMap::from([("OPENCODE_API_KEY".to_string(), "secret-value".to_string())]),
                secret_env: HashMap::new(),
                resources: HashMap::new(),
                policy: RunnerPolicy::default(),
            };
            let error = config::save(&runner).expect_err("direct config write must validate");
            assert!(!error.message.contains("secret-value"));

            runner.env = HashMap::from([("MONKEY".to_string(), "public-value".to_string())]);
            config::save(&runner).expect("unrelated public name remains allowed");
        });
    }

    #[test]
    fn migration_plan_is_value_free_and_uses_generic_keychain_references() {
        let runner = Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            settings: RunnerSettings::default(),
            env: HashMap::from([
                ("OPENCODE_API_KEY".to_string(), "secret-value".to_string()),
                ("MONKEY".to_string(), "public-value".to_string()),
            ]),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        };
        let plan = secret_env_migration_plan_for_runner(&runner);
        let rendered = serde_json::to_string(&plan).expect("serialize plan");
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].key, "OPENCODE_API_KEY");
        assert_eq!(plan.entries[0].secret, "runner/lab/OPENCODE_API_KEY");
        assert!(!rendered.contains("secret-value"));
        assert!(!rendered.contains("public-value"));
    }

    #[test]
    fn ssh_runner_id_must_match_server_id() {
        test_support::with_isolated_home(|_| {
            server::create(
                r#"{"id":"homeboy-lab","host":"192.168.86.63","user":"user"}"#,
                false,
            )
            .expect("create server");

            let err = create(
                r#"{
                    "id":"lab",
                    "kind":"ssh",
                    "server_id":"homeboy-lab",
                    "workspace_root":"/home/user/Developer"
                }"#,
                false,
            )
            .expect_err("ssh runner cannot use a second ID");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("SSH runner IDs are server IDs"));
        });
    }

    #[test]
    fn runner_set_updates_fields() {
        test_support::with_isolated_home(|_| {
            create(
                r#"{"id":"lab-local","kind":"local","workspace_root":"/tmp/a"}"#,
                false,
            )
            .expect("create runner");

            let result = merge(
                Some("lab-local"),
                r#"{"workspace_root":"/tmp/b","concurrency_limit":3}"#,
                &[],
            )
            .expect("merge runner");

            match result {
                MergeOutput::Single(result) => {
                    assert_eq!(result.id, "lab-local");
                    assert!(result
                        .updated_fields
                        .contains(&"workspace_root".to_string()));
                    assert!(result
                        .updated_fields
                        .contains(&"concurrency_limit".to_string()));
                }
                MergeOutput::Bulk(_) => panic!("expected single merge"),
            }

            let runner = load("lab-local").expect("load runner");
            assert_eq!(runner.workspace_root.as_deref(), Some("/tmp/b"));
            assert_eq!(runner.settings.concurrency_limit, Some(3));
        });
    }

    #[test]
    fn runner_secret_env_refs_resolve_from_env_and_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let secret_file = temp.path().join("runner-token");
        std::fs::write(&secret_file, "dummy-file-secret\n").expect("write dummy secret");
        std::env::set_var("HOMEBOY_DUMMY_SECRET_REF", "dummy-env-secret");

        let resolved = resolve_runner_secret_env(&HashMap::from([
            (
                "FROM_ENV".to_string(),
                server::RunnerSecretEnvRef {
                    env: Some("HOMEBOY_DUMMY_SECRET_REF".to_string()),
                    file: None,
                    secret: None,
                },
            ),
            (
                "FROM_FILE".to_string(),
                server::RunnerSecretEnvRef {
                    env: None,
                    file: Some(secret_file.display().to_string()),
                    secret: None,
                },
            ),
        ]))
        .expect("resolve secret refs");

        assert_eq!(
            resolved.get("FROM_ENV").map(String::as_str),
            Some("dummy-env-secret")
        );
        assert_eq!(
            resolved.get("FROM_FILE").map(String::as_str),
            Some("dummy-file-secret")
        );
        std::env::remove_var("HOMEBOY_DUMMY_SECRET_REF");
    }

    #[test]
    fn runner_secret_env_refs_resolve_from_configured_homeboy_secret() {
        homeboy_core::test_support::with_isolated_home(|_| {
            homeboy_agents::agent_task_secrets::set_config_secret(
                "HOMEBOY_DUMMY_CONFIGURED_SECRET",
                "dummy-configured-secret",
            )
            .expect("configure secret");

            let resolved = resolve_runner_secret_env(&HashMap::from([(
                "FROM_SECRET".to_string(),
                server::RunnerSecretEnvRef {
                    env: None,
                    file: None,
                    secret: Some("HOMEBOY_DUMMY_CONFIGURED_SECRET".to_string()),
                },
            )]))
            .expect("resolve configured secret ref");

            assert_eq!(
                resolved.get("FROM_SECRET").map(String::as_str),
                Some("dummy-configured-secret")
            );
        });
    }

    #[test]
    fn runner_secret_env_refs_reject_multiple_sources() {
        let err = resolve_runner_secret_env(&HashMap::from([(
            "INVALID".to_string(),
            server::RunnerSecretEnvRef {
                env: Some("HOMEBOY_DUMMY_SECRET_REF".to_string()),
                file: None,
                secret: Some("HOMEBOY_DUMMY_CONFIGURED_SECRET".to_string()),
            },
        )]))
        .expect_err("multiple sources rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("exactly one"));
    }

    #[test]
    fn default_lab_runner_prefers_configured_connected_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-b"),
            vec![
                default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true),
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_selects_single_runner_when_unconfigured() {
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, false),
                default_lab_candidate("lab-b", RunnerTunnelMode::Reverse, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));

        let disconnected = resolve_default_lab_runner_from_candidates(
            None,
            vec![default_lab_candidate(
                "lab-a",
                RunnerTunnelMode::DirectSsh,
                false,
            )],
        );

        assert_eq!(disconnected.as_deref(), Some("lab-a"));
    }

    #[test]
    fn default_lab_runner_uses_readiness_when_connected_state_is_not_unique() {
        let none_connected_with_multiple_candidates = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, false),
                default_lab_candidate("lab-b", RunnerTunnelMode::Reverse, false),
            ],
        );
        let multiple_connected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true),
                default_lab_candidate("lab-b", RunnerTunnelMode::Reverse, true),
            ],
        );

        assert_eq!(
            none_connected_with_multiple_candidates.as_deref(),
            Some("lab-a")
        );
        assert_eq!(multiple_connected.as_deref(), Some("lab-a"));
    }

    #[test]
    fn default_lab_runner_uses_available_preferred_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-a"),
            vec![default_lab_candidate(
                "lab-a",
                RunnerTunnelMode::DirectSsh,
                false,
            )],
        );

        assert_eq!(selected.as_deref(), Some("lab-a"));
    }

    #[test]
    fn default_lab_runner_does_not_pin_busy_preferred_runner() {
        let mut preferred = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        preferred.active_jobs = 4;
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-a"),
            vec![
                preferred,
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_excludes_runner_at_capacity() {
        let mut busy = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        busy.capacity = Some(1);
        busy.active_jobs = 1;

        let selected = resolve_default_lab_runner_from_candidates(None, vec![busy]);

        assert_eq!(selected, None);
    }

    #[test]
    fn default_lab_runner_does_not_pin_unknown_preferred_runner() {
        let mut preferred = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        preferred.active_jobs_available = false;
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-a"),
            vec![
                preferred,
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_falls_back_when_preferred_is_missing() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-missing"),
            vec![default_lab_candidate(
                "lab-b",
                RunnerTunnelMode::DirectSsh,
                true,
            )],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_rejects_ineligible_preferred_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            Some("lab-a"),
            vec![
                default_lab_candidate("lab-a", RunnerTunnelMode::Reverse, false),
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_can_select_connected_reverse_runner() {
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![default_lab_candidate(
                "homeboy-lab",
                RunnerTunnelMode::Reverse,
                true,
            )],
        );

        assert_eq!(selected.as_deref(), Some("homeboy-lab"));
    }

    #[test]
    fn default_lab_runner_prefers_less_busy_ready_runner() {
        let mut busy = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        busy.active_jobs = 3;
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                busy,
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn default_lab_runner_prefers_known_active_job_state() {
        let mut unknown = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        unknown.active_jobs_available = false;
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                unknown,
                default_lab_candidate("lab-b", RunnerTunnelMode::DirectSsh, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }

    #[test]
    fn runner_availability_reports_unavailable_runner_reasons() {
        let mut unavailable = default_lab_candidate("lab-a", RunnerTunnelMode::Reverse, false);
        unavailable.active_jobs_available = false;

        assert_eq!(
            unavailable.availability(),
            RunnerAvailability {
                runner_id: "lab-a".to_string(),
                connected: false,
                accepts_jobs: false,
                active_job_count: 0,
                capacity: None,
                reasons: vec![
                    "not_connected".to_string(),
                    "active_jobs_unavailable".to_string()
                ],
            }
        );
    }

    #[test]
    fn runner_availability_reports_busy_runner_at_capacity() {
        let mut busy = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        busy.capacity = Some(2);
        busy.active_jobs = 2;

        assert_eq!(
            busy.availability(),
            RunnerAvailability {
                runner_id: "lab-a".to_string(),
                connected: true,
                accepts_jobs: false,
                active_job_count: 2,
                capacity: Some(2),
                reasons: vec!["capacity_reached".to_string()],
            }
        );
        assert!(busy.availability().is_capacity_exhausted());
    }

    #[test]
    fn runner_availability_does_not_treat_substrate_failures_as_queueable_capacity() {
        let availability = RunnerAvailability::from_status_parts(
            "lab-a",
            false,
            false,
            1,
            &RunnerActiveJobState::Unavailable,
            Some(1),
        );

        assert!(!availability.is_capacity_exhausted());
        assert_eq!(
            availability.reasons,
            vec![
                "not_connected".to_string(),
                "capacity_reached".to_string(),
                "active_jobs_unavailable".to_string(),
            ]
        );
    }

    #[test]
    fn runner_availability_reports_ambiguous_unknown_capacity() {
        let mut ambiguous = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        ambiguous.active_jobs = 1;

        assert_eq!(
            ambiguous.availability(),
            RunnerAvailability {
                runner_id: "lab-a".to_string(),
                connected: true,
                accepts_jobs: false,
                active_job_count: 1,
                capacity: None,
                reasons: vec!["capacity_unknown".to_string()],
            }
        );
    }

    #[test]
    fn connected_direct_daemon_runner_with_failed_active_job_poll_still_accepts_jobs() {
        // Regression: a connected direct_daemon/direct_ssh runner (worker_pid:
        // None) whose live `/jobs` poll transiently failed reports
        // `active_job_state: Unavailable` with a 0 count. `runner status`
        // recovers to `available` on the next poll, so the lab-offload preflight
        // must not hard-reject it while it is connected and under capacity.
        let availability = RunnerAvailability::from_status_parts(
            "homeboy-lab",
            true,
            false,
            0,
            &RunnerActiveJobState::Unavailable,
            Some(8),
        );

        assert!(
            availability.accepts_jobs,
            "connected, under-capacity runner with a transient active-job poll \
             failure must still accept jobs: {availability:?}"
        );
        assert!(
            availability.reasons.is_empty(),
            "no blocking reasons expected: {:?}",
            availability.reasons
        );
        assert_eq!(availability.active_job_count, 0);
        assert_eq!(availability.capacity, Some(8));
    }

    #[test]
    fn available_direct_daemon_runner_accepts_jobs() {
        // The healthy steady-state both paths agree on.
        let availability = RunnerAvailability::from_status_parts(
            "homeboy-lab",
            true,
            false,
            0,
            &RunnerActiveJobState::Available,
            Some(8),
        );

        assert!(availability.accepts_jobs);
        assert!(availability.reasons.is_empty());
    }

    #[test]
    fn matching_product_build_runner_accepts_jobs() {
        let availability = RunnerAvailability::from_status_parts(
            "homeboy-lab",
            true,
            false,
            0,
            &RunnerActiveJobState::Available,
            Some(8),
        );

        assert!(availability.accepts_jobs);
        assert_ne!(
            env!("CARGO_PKG_VERSION"),
            homeboy_product_identity::product_version(),
            "internal runner crate versions must not make matching product builds stale"
        );
    }

    #[test]
    fn at_capacity_runner_still_rejects_even_with_failed_active_job_poll() {
        // A genuinely busy runner stays blocked; the soft active-job signal must
        // not let a saturated runner through.
        let availability = RunnerAvailability::from_status_parts(
            "homeboy-lab",
            true,
            false,
            8,
            &RunnerActiveJobState::Available,
            Some(8),
        );

        assert!(!availability.accepts_jobs);
        assert!(availability
            .reasons
            .iter()
            .any(|reason| reason == "capacity_reached"));
    }

    #[test]
    fn disconnected_runner_with_unavailable_active_jobs_still_rejects() {
        // A disconnected runner stays blocked, and the active-job signal remains
        // visible alongside the hard `not_connected` blocker for diagnostics.
        let availability = RunnerAvailability::from_status_parts(
            "homeboy-lab",
            false,
            false,
            0,
            &RunnerActiveJobState::Unavailable,
            Some(8),
        );

        assert!(!availability.accepts_jobs);
        assert_eq!(
            availability.reasons,
            vec![
                "not_connected".to_string(),
                "active_jobs_unavailable".to_string(),
            ]
        );
    }

    #[test]
    fn default_lab_runner_skips_stale_daemon_runner() {
        let mut stale = default_lab_candidate("lab-a", RunnerTunnelMode::DirectSsh, true);
        stale.stale_daemon = true;
        let selected = resolve_default_lab_runner_from_candidates(
            None,
            vec![
                stale,
                default_lab_candidate("lab-b", RunnerTunnelMode::Reverse, true),
            ],
        );

        assert_eq!(selected.as_deref(), Some("lab-b"));
    }
}
