use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::super::validation_dependencies::RunnerValidationDependencySyncOutput;
use super::super::RunnerWorkspaceLease;
use super::snapshot::WorkspaceContentManifest;
use homeboy_core::resource_lifecycle_index::ResourceLifecycleRecord;

pub(crate) const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".git/**",
    ".homeboy",
    ".homeboy/**",
    ".homeboy-build",
    ".homeboy-build/**",
    ".homeboy-bin",
    ".homeboy-bin/**",
    "._*",
    "**/._*",
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa",
    "id_ed25519",
    ".ssh",
    ".ssh/**",
    "*.p12",
    "*.pfx",
];

// RunnerWorkspaceSyncMode + RunnerWorkspaceSyncOptions are behavior-free data;
// they now live in the shared runner-contract crate so core can name them
// without a core -> runner edge. Re-exported so internal/CLI call sites resolve
// unchanged.
pub use homeboy_lab_runner_contract::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};

#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceSyncOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub materialization_plan: RunnerWorkspaceMaterializationPlan,
    pub current_workspace: RunnerWorkspaceCurrentSummary,
    pub workspace_lease: RunnerWorkspaceLease,
    pub resource_lifecycle: ResourceLifecycleRecord,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub snapshot_identity: String,
    #[serde(flatten)]
    pub counts: ByteFileCounts,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub workspace_cleanliness: String,
    pub validation_dependencies: Vec<RunnerValidationDependencySyncOutput>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceMaterializationContract {
    pub workspace_root: String,
    pub local_path: String,
    pub local_basename: String,
    pub remote_path: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub identity: String,
    pub path_strategy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_isolation_token: Option<String>,
    pub declared_inputs: RunnerWorkspaceDeclaredInputs,
    pub source_provenance: RunnerWorkspaceSourceProvenance,
    pub dirty_policy: RunnerWorkspaceDirtyPolicy,
    pub output_paths: RunnerWorkspaceOutputPaths,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_git_bundle: Option<ControllerGitBundleProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_transfer: Option<SnapshotTransferStats>,
}

/// Content accounting for a snapshot transport. `reused` is linked from the
/// compatible immutable seed; `transferred` is copied from the controller.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SnapshotTransferStats {
    pub reused: ByteFileCounts,
    pub transferred: ByteFileCounts,
    pub final_size: ByteFileCounts,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerGitBundleProvenance {
    pub provenance: &'static str,
    pub source_sha: String,
    pub source_refs: Vec<String>,
    pub sha256: String,
    pub cleanup_owner: &'static str,
    pub cleanup_ttl: &'static str,
}

pub type RunnerWorkspaceMaterializationPlan = RunnerWorkspaceMaterializationContract;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceDeclaredInputs {
    pub path: String,
    pub mode: RunnerWorkspaceSyncMode,
    pub controller_routed_git: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
    pub snapshot_includes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceSourceProvenance {
    pub local_path: String,
    pub local_basename: String,
    pub identity: String,
    pub path_strategy: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceDirtyPolicy {
    pub allow_dirty_lab_workspace: bool,
    pub workspace_cleanliness: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceOutputPaths {
    pub workspace_root: String,
    pub lab_workspaces_root: String,
    pub remote_path: String,
    pub artifact_dir: String,
}

impl RunnerWorkspaceOutputPaths {
    pub fn for_remote_path(workspace_root: &str, remote_path: &str) -> Self {
        let workspace_root = workspace_root.trim_end_matches('/').to_string();
        Self {
            lab_workspaces_root: format!("{workspace_root}/_lab_workspaces"),
            workspace_root,
            remote_path: remote_path.to_string(),
            artifact_dir: Self::artifact_dir_for_workspace(remote_path),
        }
    }

    pub fn artifact_dir_for_workspace(remote_path: &str) -> String {
        format!("{}-homeboy-artifacts", remote_path.trim_end_matches('/'))
    }
}

impl RunnerWorkspaceMaterializationContract {
    #[allow(clippy::too_many_arguments)]
    pub fn from_sync_options(
        workspace_root: &str,
        local_path: &str,
        local_basename: &str,
        remote_path: &str,
        identity: &str,
        path_strategy: &'static str,
        options: &RunnerWorkspaceSyncOptions,
        snapshot_includes: &[String],
        workspace_cleanliness: &str,
    ) -> Self {
        let workspace_root = workspace_root.trim_end_matches('/').to_string();
        let run_isolation_token = options
            .run_isolation_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .map(ToString::to_string);
        Self {
            workspace_root: workspace_root.clone(),
            local_path: local_path.to_string(),
            local_basename: local_basename.to_string(),
            remote_path: remote_path.to_string(),
            sync_mode: options.mode,
            identity: identity.to_string(),
            path_strategy,
            run_isolation_token: run_isolation_token.clone(),
            declared_inputs: RunnerWorkspaceDeclaredInputs {
                path: options.path.clone(),
                mode: options.mode,
                controller_routed_git: options.controller_routed_git,
                changed_since_base: options.changed_since_base.clone(),
                git_fetch_refs: options.git_fetch_refs.clone(),
                snapshot_includes: snapshot_includes.to_vec(),
            },
            source_provenance: RunnerWorkspaceSourceProvenance {
                local_path: local_path.to_string(),
                local_basename: local_basename.to_string(),
                identity: identity.to_string(),
                path_strategy,
            },
            dirty_policy: RunnerWorkspaceDirtyPolicy {
                allow_dirty_lab_workspace: options.allow_dirty_lab_workspace,
                workspace_cleanliness: workspace_cleanliness.to_string(),
            },
            output_paths: RunnerWorkspaceOutputPaths::for_remote_path(&workspace_root, remote_path),
            controller_git_bundle: None,
            snapshot_transfer: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        workspace_root: &str,
        local_path: &str,
        local_basename: &str,
        remote_path: &str,
        sync_mode: RunnerWorkspaceSyncMode,
        identity: &str,
    ) -> Self {
        let options = RunnerWorkspaceSyncOptions {
            path: local_path.to_string(),
            mode: sync_mode,
            allow_dirty_lab_workspace: false,
            ..Default::default()
        };
        Self::from_sync_options(
            workspace_root,
            local_path,
            local_basename,
            remote_path,
            identity,
            "workspace_root_lab_workspaces_sanitized_basename_identity_digest",
            &options,
            &[],
            match sync_mode {
                RunnerWorkspaceSyncMode::Git => "clean_remote_required",
                RunnerWorkspaceSyncMode::SnapshotGit => "snapshot_synthetic_git_unique_workspace",
                RunnerWorkspaceSyncMode::Snapshot => "snapshot_unique_workspace",
            },
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspacePullOptions {
    pub remote_path: String,
    pub includes: Vec<String>,
    pub to: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspacePullPlan {
    pub runner_id: String,
    pub remote_path: String,
    pub includes: Vec<String>,
    pub local_destination: String,
    pub remote_sources: Vec<String>,
    pub allowed_roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspacePullOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub remote_path: String,
    pub includes: Vec<String>,
    pub local_destination: String,
    pub remote_sources: Vec<String>,
    pub allowed_roots: Vec<String>,
    pub dry_run: bool,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceListOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub workspace_root: String,
    pub lab_workspaces_root: String,
    pub workspaces: Vec<RunnerWorkspaceListEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceListEntry {
    pub remote_path: String,
    pub exec_command: String,
}

#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspacePruneOptions {
    pub apply: bool,
    pub min_age_hours: u64,
    pub limit: usize,
    pub passes: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspacePruneOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub workspace_root: String,
    pub lab_workspaces_root: String,
    pub min_age_hours: u64,
    pub candidates: Vec<RunnerWorkspacePruneEntry>,
    pub removed: Vec<RunnerWorkspacePruneEntry>,
    pub skipped: Vec<RunnerWorkspacePruneSkippedEntry>,
    pub total_candidate_count: usize,
    pub total_candidate_bytes: u64,
    pub total_removed_bytes: u64,
    pub remaining_candidate_count: usize,
    pub remaining_candidate_bytes: u64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    pub drain_command: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspacePruneEntry {
    pub remote_path: String,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_identity: Option<String>,
    pub age_seconds: u64,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspacePruneSkippedEntry {
    pub remote_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspaceSnapshotFilters {
    pub repo: Option<String>,
    pub source_ref: Option<String>,
    pub source_commit: Option<String>,
    pub run_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceSnapshotsOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub workspace_root: String,
    pub lab_workspaces_root: String,
    pub filters: RunnerWorkspaceSnapshotAppliedFilters,
    pub snapshots: Vec<RunnerWorkspaceSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceSnapshotAppliedFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerWorkspaceSnapshotEntry {
    pub runner_id: String,
    pub repo: String,
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: String,
    pub snapshot_identity: String,
    #[serde(default)]
    pub snapshot_excludes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_manifest: Option<WorkspaceContentManifest>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_lifecycle: Option<ResourceLifecycleRecord>,
    pub exec_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RunnerWorkspaceMetadata {
    pub schema: String,
    pub runner_id: String,
    #[serde(default)]
    pub repo: Option<String>,
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: String,
    pub snapshot_identity: String,
    #[serde(default)]
    pub snapshot_excludes: Vec<String>,
    #[serde(default)]
    pub content_manifest: Option<WorkspaceContentManifest>,
    pub synced_at: String,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub source_commit: Option<String>,
    #[serde(default)]
    pub source_dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub resource_lifecycle: Option<ResourceLifecycleRecord>,
}

// RunnerWorkspaceCurrentSummary now lives in the shared runner-contract crate
// (core's dev_run names it). Re-exported so runner-internal call sites resolve.
pub use homeboy_lab_runner_contract::RunnerWorkspaceCurrentSummary;

/// File + byte counts for a synced/snapshotted workspace tree.
///
/// Shared across the workspace-sync and git-dependency materialization outputs
/// so the `files` / `bytes` pair is declared once. Serialized flat via
/// `#[serde(flatten)]` to preserve the historical top-level JSON keys.
// ByteFileCounts now lives in the shared runner-contract crate (core's dev_run
// names it). Re-exported so runner-internal call sites resolve.
pub use homeboy_lab_runner_contract::ByteFileCounts;

pub(super) type SnapshotStats = ByteFileCounts;

#[derive(Default)]
pub(super) struct LocalGitState {
    pub(super) commit: Option<String>,
    pub(super) ref_name: Option<String>,
    pub(super) dirty: Option<bool>,
}

pub(super) struct GitSnapshot {
    pub(super) remote_url: String,
    pub(super) head: String,
    pub(super) branch: Option<String>,
    pub(super) changed_since_base: Option<String>,
    pub(super) git_fetch_refs: Vec<String>,
}

pub(crate) fn canonical_workspace_path(path: &str) -> homeboy_core::error::Result<PathBuf> {
    use homeboy_core::error::Error;
    use std::path::Path;

    let expanded = shellexpand::tilde(path).to_string();
    let path = Path::new(&expanded);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "path",
            format!("workspace sync path must be an existing directory: {expanded}"),
            None,
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(err.to_string(), Some("canonicalize sync path".to_string()))
    })
}
