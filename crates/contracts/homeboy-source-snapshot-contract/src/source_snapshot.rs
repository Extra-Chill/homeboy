//! Pure serializable source-snapshot contract types.
//!
//! `SourceSnapshot` captures the identity of a source tree (git branch/sha,
//! dirty state, sync mode, content hash) as it crosses process boundaries
//! between the controller, runner, and Lab. `SourceSnapshotPolicy` declares the
//! sync-exclude rules applied when collecting one.
//!
//! These are behavior-free data types. The collection behavior
//! (`collect_local`, `existing_remote`, git/component/extension-driven
//! exclude discovery, `SourceSnapshotPolicy::from_env` / `for_path`) lives in
//! `homeboy-core` as free functions, because it reaches into git, the component
//! inventory, the extension store, and the filesystem.

use serde::{Deserialize, Serialize};

const DEFAULT_SYNC_EXCLUDES: &[&str] = &[
    ".git/",
    ".homeboy-build/",
    ".homeboy-bin/",
    ".homeboy/",
    ".DS_Store",
    "._*",
    "**/._*",
    ".env",
    ".env.*",
];

/// The default set of sync-exclude globs applied to every source snapshot.
pub fn default_sync_excludes() -> Vec<String> {
    DEFAULT_SYNC_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshotPolicy {
    pub sync_excludes: Vec<String>,
}

impl SourceSnapshotPolicy {
    pub fn with_sync_excludes<I, S>(mut self, excludes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extend_sync_excludes(excludes.into_iter().map(Into::into));
        self
    }

    /// Append excludes, skipping blanks and duplicates. Public so core's
    /// filesystem/git-driven policy builders (`from_env`, `for_path`) can layer
    /// discovered excludes onto a base policy.
    pub fn extend_sync_excludes<I, S>(&mut self, excludes: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for exclude in excludes.into_iter().map(Into::into) {
            if !exclude.trim().is_empty() && !self.sync_excludes.contains(&exclude) {
                self.sync_excludes.push(exclude);
            }
        }
    }
}

impl Default for SourceSnapshotPolicy {
    fn default() -> Self {
        Self {
            sync_excludes: default_sync_excludes(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    pub dirty: bool,
    pub sync_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_identity: Option<String>,
    /// Original immutable source snapshot for a prepared workspace whose
    /// retained execution state has received one or more source deltas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_workspace_original_snapshot_identity: Option<String>,
    /// Ordered source snapshot identities applied after the original prepared
    /// snapshot. This is runner metadata, never a caller-supplied lease.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepared_workspace_update_lineage: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_tree: Option<String>,
    pub snapshot_hash: String,
    pub synced_at: String,
    pub sync_excludes: Vec<String>,
}
