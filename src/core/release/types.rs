use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::is_zero_u32;

/// Dry-run description of a release step.
///
/// `ReleasePlan` is a *preview* shape — it exists to render human-readable
/// step lists in `--dry-run` mode and in `--json` output. It does NOT drive
/// execution. The actual release runs through a straight-line function in
/// `pipeline::execute()` that calls each step directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePlan {
    pub component_id: String,
    pub enabled: bool,
    pub steps: Vec<ReleasePlanStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semver_recommendation: Option<ReleaseSemverRecommendation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSemverCommit {
    pub sha: String,
    pub subject: String,
    pub commit_type: String,
    pub breaking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSemverRecommendation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_tag: Option<String>,
    pub range: String,
    pub commits: Vec<ReleaseSemverCommit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_bump: Option<String>,
    pub requested_bump: String,
    pub is_underbump: bool,
    pub reasons: Vec<String>,
}

/// Run result for a single release. Shape is preserved from the pre-refactor
/// `ReleaseRun { component_id, enabled, result: PipelineRunResult }` so `--json`
/// consumers see no change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRun {
    pub component_id: String,
    pub enabled: bool,
    pub result: ReleaseRunResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRunResult {
    pub steps: Vec<ReleaseStepResult>,
    pub status: ReleaseStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReleaseRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseStepResult {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    pub status: ReleaseStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<crate::error::Hint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStepStatus {
    Success,
    PartialSuccess,
    Failed,
    Skipped,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRunSummary {
    pub total_steps: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub missing: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_summary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Mutable state threaded through the straight-line release execution.
///
/// Every step that produces a downstream value (the new version, the tag name,
/// the release notes, the built artifacts) stores it here and the next step
/// reads it back. This was previously a `Mutex<ReleaseContext>` accessed
/// through a generic pipeline DAG — a pattern the execution never actually
/// needed because every step runs sequentially.
#[derive(Debug, Clone, Default)]
pub struct ReleaseState {
    pub version: Option<String>,
    pub tag: Option<String>,
    pub notes: Option<String>,
    pub artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePlanStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
    pub status: ReleasePlanStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePlanStatus {
    Ready,
    Missing,
    Disabled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseOptions {
    pub bump_type: String,
    pub dry_run: bool,
    /// Override the component's `local_path` for this release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    /// Skip lint/test code quality checks before release.
    #[serde(default)]
    pub skip_checks: bool,
    /// Skip publish/package steps (version bump + tag + push only).
    /// Use when CI handles publishing after the tag is pushed.
    #[serde(default)]
    pub skip_publish: bool,
    /// Deploy after release — defers artifact cleanup until after deployment.
    #[serde(default)]
    pub deploy: bool,
    /// Skip the GitHub Release creation step (tag + notes on github.com).
    /// Use when another pipeline (CI, semantic-release, etc.) already owns that step.
    #[serde(default)]
    pub skip_github_release: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReleaseCommandInput {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub deploy: bool,
    #[serde(default)]
    pub recover: bool,
    #[serde(default)]
    pub skip_checks: bool,
    /// Explicit bump override: "major", "minor", "patch", or a version string like "2.0.0".
    /// When set, overrides auto-detection from commit history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bump_override: Option<String>,
    #[serde(default)]
    pub skip_publish: bool,
    /// Skip the GitHub Release creation step (tag + notes on github.com).
    #[serde(default)]
    pub skip_github_release: bool,
    /// Git identity for release commits: "bot", "Name <email>", or None (use existing config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReleaseDeploymentSummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub skipped: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub planned: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_result: Option<crate::deploy::ComponentDeployResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseDeploymentResult {
    pub projects: Vec<ReleaseProjectDeployResult>,
    pub summary: ReleaseDeploymentSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseCommandResult {
    pub component_id: String,
    pub bump_type: String,
    pub dry_run: bool,
    pub releasable_commits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<ReleaseRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<ReleaseDeploymentResult>,
}

/// Result of a batch release across multiple components.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseResult {
    pub results: Vec<BatchReleaseComponentResult>,
    pub summary: BatchReleaseSummary,
}

/// Per-component result within a batch release.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseComponentResult {
    pub component_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ReleaseCommandResult>,
}

/// Summary counts for a batch release.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseSummary {
    pub total: u32,
    pub released: u32,
    pub skipped: u32,
    pub failed: u32,
}
