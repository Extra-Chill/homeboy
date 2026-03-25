//! types — extracted from types.rs.

use serde::Serialize;
use crate::component::Component;
use crate::error::Result;
use super::ComponentDeployResult;


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

#[derive(Debug, Clone, Default)]
pub struct ReleaseStateBuckets {
    pub ready_to_deploy: Vec<String>,
    pub needs_bump: Vec<String>,
    pub docs_only: Vec<String>,
    pub has_uncommitted: Vec<String>,
    pub unknown: Vec<String>,
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
