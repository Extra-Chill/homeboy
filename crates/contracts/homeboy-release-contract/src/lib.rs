//! Shared release/deploy state data types.
//!
//! These sit below `homeboy-core` so that core's status-reporting mechanics
//! (fleet / project / context) and the `homeboy-release` feature crate can both
//! reference them without a dependency cycle. They are pure data (plus trivial
//! derived accessors) — all release/deploy *behavior* lives in `homeboy-release`.

use serde::{Deserialize, Serialize};

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Reason a component was selected for (or flagged during) a deploy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployReason {
    /// Component was explicitly specified by ID
    ExplicitlySelected,
    /// --all flag was used
    AllSelected,
    /// Local and remote versions differ
    VersionMismatch,
    /// Could not determine local version
    UnknownLocalVersion,
    /// Could not determine remote version (not deployed or no version file)
    UnknownRemoteVersion,
}

/// Status indicator for component version comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Local and remote versions match
    UpToDate,
    /// Local version ahead of remote (needs deploy)
    NeedsUpdate,
    /// Remote version ahead of local (local behind)
    BehindRemote,
    /// Local checkout is behind its upstream branch
    BehindUpstream,
    /// Remote matches a configured source checkout that is stale or detached
    SourceStale,
    /// Cannot determine status
    Unknown,
}

impl ComponentStatus {
    /// Whether deploying the configured source would advance the target.
    pub fn requires_deploy(&self) -> bool {
        matches!(self, Self::NeedsUpdate)
    }
}

/// Release state tracking for deployment decisions.
/// Captures git state relative to the last version tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseState {
    /// Number of commits since the last version tag
    pub commits_since_version: u32,
    /// Number of code commits (non-docs)
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    /// Number of docs-only commits
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
    /// Whether there are uncommitted changes in the working directory
    pub has_uncommitted_changes: bool,
    /// The baseline reference (tag or commit hash) used for comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    /// Warning emitted when the detected baseline may not align with the current version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}

/// High-level status derived from a component release state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStateStatus {
    Uncommitted,
    NeedsRelease,
    DocsOnly,
    Clean,
    Unknown,
}

impl ReleaseState {
    pub fn status(&self) -> ReleaseStateStatus {
        if self.has_uncommitted_changes {
            ReleaseStateStatus::Uncommitted
        } else if self.code_commits > 0 {
            ReleaseStateStatus::NeedsRelease
        } else if self.docs_only_commits > 0 {
            ReleaseStateStatus::DocsOnly
        } else {
            ReleaseStateStatus::Clean
        }
    }
}

impl ReleaseStateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStateStatus::Uncommitted => "uncommitted",
            ReleaseStateStatus::NeedsRelease => "needs_release",
            ReleaseStateStatus::DocsOnly => "docs_only",
            ReleaseStateStatus::Clean => "clean",
            ReleaseStateStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseStateBuckets {
    pub ready_to_deploy: Vec<String>,
    pub needs_release: Vec<String>,
    pub docs_only: Vec<String>,
    pub has_uncommitted: Vec<String>,
    pub unknown: Vec<String>,
}

/// Information about a version target after reading a component's version files.
#[derive(Debug, Clone, Serialize)]
pub struct VersionTargetInfo {
    pub file: String,
    pub pattern: String,
    pub full_path: String,
    pub match_count: usize,
    /// Warning message when target exists but didn't match or had issues
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// A component's resolved version plus the targets it was read from.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentVersionSnapshot {
    pub component_id: String,
    pub version: String,
    pub targets: Vec<VersionTargetInfo>,
}

/// Compact per-component deploy status, surfaced to core's fleet/project status
/// mechanics by the release provider (so they don't depend on the full deploy
/// result type). Mirrors the fields those mechanics actually read.
#[derive(Debug, Clone)]
pub struct ComponentDeployStatus {
    pub id: String,
    pub component_status: Option<ComponentStatus>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
}

/// A component's most recent finalized release, as read from its changelog.
#[derive(Debug, Clone)]
pub struct FinalizedReleaseSnapshot {
    pub tag: String,
    pub date: Option<String>,
    pub summary: Option<String>,
}

/// A component's unreleased changelog section (path + label + entries).
#[derive(Debug, Clone)]
pub struct ChangelogSnapshotData {
    pub path: String,
    pub label: String,
    pub items: Vec<String>,
}

/// Baseline-alignment validation warning for a component's version target.
#[derive(Debug, Clone)]
pub struct BaselineAlignmentWarning {
    pub message: String,
}
