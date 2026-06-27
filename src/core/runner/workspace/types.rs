use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::super::validation_dependencies::RunnerValidationDependencySyncOutput;
use super::super::RunnerWorkspaceLease;

pub(crate) const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".git/**",
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
    "node_modules",
    "node_modules/**",
    "target",
    "target/**",
    "dist",
    "dist/**",
    ".next",
    ".next/**",
    ".turbo",
    ".turbo/**",
    ".cache",
    ".cache/**",
    "*.tsbuildinfo",
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkspaceSyncMode {
    #[default]
    Snapshot,
    SnapshotGit,
    Git,
}

impl RunnerWorkspaceSyncMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::SnapshotGit => "snapshot-git",
            Self::Git => "git",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspaceSyncOptions {
    pub path: String,
    pub mode: RunnerWorkspaceSyncMode,
    pub controller_routed_git: bool,
    pub changed_since_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
    pub snapshot_includes: Vec<String>,
    pub allow_dirty_lab_workspace: bool,
    /// Opaque per-run token (e.g. an agent-task run id) folded into the
    /// deterministic remote workspace path so two distinct cook/dispatch runs
    /// at the same source HEAD never share a long-lived remote checkout.
    ///
    /// Without this, the git-mode remote path is keyed only on
    /// `(source path, HEAD)`, so a later unrelated run reuses the earlier run's
    /// workspace directory and can observe leftover untracked artifacts from it
    /// (cross-run contamination, see #4393). When set, each run gets an
    /// isolated `_lab_workspaces/<name>-<digest>` directory.
    pub run_isolation_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceSyncOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub current_workspace: RunnerWorkspaceCurrentSummary,
    pub workspace_lease: RunnerWorkspaceLease,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub snapshot_identity: String,
    #[serde(flatten)]
    pub counts: ByteFileCounts,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub workspace_cleanliness: String,
    pub validation_dependencies: Vec<RunnerValidationDependencySyncOutput>,
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
    pub total_candidate_bytes: u64,
    pub total_removed_bytes: u64,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceCurrentSummary {
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
    /// Commit SHA of the synthetic git checkout created for a `snapshot-git`
    /// sync, so write-capable agent-task dispatches can trace the dirty
    /// controller-side worktree back to the synthetic commit that carries it
    /// into the runner workspace. `None` for plain `snapshot`/`git` syncs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_commit: Option<String>,
}

/// File + byte counts for a synced/snapshotted workspace tree.
///
/// Shared across the workspace-sync and git-dependency materialization outputs
/// so the `files` / `bytes` pair is declared once. Serialized flat via
/// `#[serde(flatten)]` to preserve the historical top-level JSON keys.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct ByteFileCounts {
    pub files: usize,
    pub bytes: u64,
}

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

pub(crate) fn canonical_workspace_path(path: &str) -> crate::core::error::Result<PathBuf> {
    use crate::core::error::Error;
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
