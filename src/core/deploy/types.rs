use serde::Serialize;

use crate::component::Component;
use crate::config;
use crate::error::Result;
use crate::is_zero_u32;
use crate::paths as base_path;

/// Parse bulk component IDs from a JSON spec.
pub fn parse_bulk_component_ids(json_spec: &str) -> Result<Vec<String>> {
    let input = config::parse_bulk_ids(json_spec)?;
    Ok(input.component_ids)
}

pub struct DeployResult {
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
}

impl DeployResult {
    pub(super) fn success(exit_code: i32) -> Self {
        Self {
            success: true,
            exit_code,
            error: None,
        }
    }

    pub(super) fn failure(exit_code: i32, error: String) -> Self {
        Self {
            success: false,
            exit_code,
            error: Some(error),
        }
    }
}

pub struct DeployConfig {
    pub component_ids: Vec<String>,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    /// Skip build if artifact already exists (used by release --deploy)
    pub skip_build: bool,
    /// Keep build dependencies (skip cleanup even when auto_cleanup is enabled)
    pub keep_deps: bool,
    /// Assert expected version before deploying (abort if mismatch)
    pub expected_version: Option<String>,
    /// Skip auto-pulling latest changes before deploy
    pub no_pull: bool,
    /// Deploy from current branch HEAD instead of latest tag
    pub head: bool,
}

/// Reason why a component was selected for deployment.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Local and remote versions match
    UpToDate,
    /// Local version ahead of remote (needs deploy)
    NeedsUpdate,
    /// Remote version ahead of local (local behind)
    BehindRemote,
    /// Cannot determine status
    Unknown,
}

/// Release state tracking for deployment decisions.
/// Captures git state relative to the last version tag.
#[derive(Debug, Clone, Serialize)]
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
    NeedsBump,
    DocsOnly,
    Clean,
    Unknown,
}

impl ReleaseState {
    pub fn status(&self) -> ReleaseStateStatus {
        if self.has_uncommitted_changes {
            ReleaseStateStatus::Uncommitted
        } else if self.code_commits > 0 {
            ReleaseStateStatus::NeedsBump
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
            ReleaseStateStatus::NeedsBump => "needs_bump",
            ReleaseStateStatus::DocsOnly => "docs_only",
            ReleaseStateStatus::Clean => "clean",
            ReleaseStateStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseStateBuckets {
    pub ready_to_deploy: Vec<String>,
    pub needs_bump: Vec<String>,
    pub docs_only: Vec<String>,
    pub has_uncommitted: Vec<String>,
    pub unknown: Vec<String>,
}

/// Result for a single component deployment.
#[derive(Debug, Clone, Serialize)]

pub struct ComponentDeployResult {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_reason: Option<DeployReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_status: Option<ComponentStatus>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ReleaseState>,
    /// The git ref (tag or branch) that was built and deployed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed_ref: Option<String>,
}

impl ComponentDeployResult {
    pub(super) fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            status: String::new(),
            deploy_reason: None,
            component_status: None,
            local_version: None,
            remote_version: None,
            error: None,
            artifact_path: component.build_artifact.clone(),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_exit_code: None,
            deploy_exit_code: None,
            release_state: None,
            deployed_ref: None,
        }
    }

    /// Shorthand for the common failure pattern: status="failed" + versions + error.
    pub(super) fn failed(
        component: &Component,
        base_path: &str,
        local_version: Option<String>,
        remote_version: Option<String>,
        error: String,
    ) -> Self {
        Self::new(component, base_path)
            .with_status("failed")
            .with_versions(local_version, remote_version)
            .with_error(error)
    }

    pub(super) fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub(super) fn with_versions(mut self, local: Option<String>, remote: Option<String>) -> Self {
        self.local_version = local;
        self.remote_version = remote;
        self
    }

    pub(super) fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    pub(super) fn with_build_exit_code(mut self, code: Option<i32>) -> Self {
        self.build_exit_code = code;
        self
    }

    pub(super) fn with_deploy_exit_code(mut self, code: Option<i32>) -> Self {
        self.deploy_exit_code = code;
        self
    }

    pub(super) fn with_component_status(mut self, status: ComponentStatus) -> Self {
        self.component_status = Some(status);
        self
    }

    pub(super) fn with_remote_path(mut self, path: String) -> Self {
        self.remote_path = Some(path);
        self
    }

    pub(super) fn with_release_state(mut self, state: ReleaseState) -> Self {
        self.release_state = Some(state);
        self
    }

    pub(super) fn with_deployed_ref(mut self, git_ref: String) -> Self {
        self.deployed_ref = Some(git_ref);
        self
    }
}

/// Result of deploying to a single project within a multi-project run.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

/// Result of a multi-project deployment.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDeployResult {
    pub component_ids: Vec<String>,
    pub projects: Vec<ProjectDeployResult>,
    pub summary: MultiDeploySummary,
}

/// Summary of multi-project deployment.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDeploySummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
    pub planned: u32,
}

/// Summary of deploy orchestration.
#[derive(Debug, Clone, Serialize)]

pub struct DeploySummary {
    pub total: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// Result of deploy orchestration for multiple components.
#[derive(Debug, Clone, Serialize)]

pub struct DeployOrchestrationResult {
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}
