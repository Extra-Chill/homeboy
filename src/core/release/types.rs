use serde::ser::Error as SerializeError;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;

use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus};
use crate::is_zero_u32;

/// Ordered release plan shared by dry-run output and release execution.
///
/// `ReleasePlan` is rendered in `--dry-run` and `--json` output, then walked by
/// `pipeline::run()` for real releases so the previewed steps match execution.
#[derive(Debug, Clone)]
pub struct ReleasePlan {
    pub plan: HomeboyPlan,
}

impl ReleasePlan {
    const ENABLED_POLICY_KEY: &'static str = "enabled";
    const SEMVER_RECOMMENDATION_POLICY_KEY: &'static str = "semver_recommendation";

    pub fn new(
        component_id: impl Into<String>,
        enabled: bool,
        steps: Vec<PlanStep>,
        semver_recommendation: Option<ReleaseSemverRecommendation>,
        warnings: Vec<String>,
        hints: Vec<String>,
    ) -> Self {
        let component_id = component_id.into();
        let mut plan = HomeboyPlan::for_component(PlanKind::Release, component_id.clone());
        plan.steps = steps;
        plan.warnings = warnings;
        plan.hints = hints;
        plan.policy.insert(
            Self::ENABLED_POLICY_KEY.to_string(),
            serde_json::Value::Bool(enabled),
        );
        if let Some(semver_recommendation) = semver_recommendation {
            plan.policy.insert(
                Self::SEMVER_RECOMMENDATION_POLICY_KEY.to_string(),
                serde_json::to_value(semver_recommendation).unwrap_or(serde_json::Value::Null),
            );
        }

        Self::from_plan(plan)
    }

    /// Wrap a generic Homeboy plan in the release compatibility contract.
    ///
    /// Release execution consumes `plan.steps` directly. The legacy top-level
    /// JSON fields (`component_id`, `enabled`, and `semver_recommendation`) are
    /// projected from the generic plan subject/policy during serialization so
    /// existing release JSON consumers keep the same shape without creating a
    /// second authoritative release data store.
    pub fn from_plan(plan: HomeboyPlan) -> Self {
        Self { plan }
    }

    pub fn component_id(&self) -> Option<&str> {
        self.plan.subject.component_id.as_deref()
    }

    pub fn enabled(&self) -> bool {
        if let Some(enabled) = self
            .plan
            .policy
            .get(Self::ENABLED_POLICY_KEY)
            .and_then(|value| value.as_bool())
        {
            return enabled;
        }

        self.plan
            .steps
            .iter()
            .any(|step| matches!(step.status, PlanStepStatus::Ready | PlanStepStatus::Running))
    }

    pub fn semver_recommendation(&self) -> Option<ReleaseSemverRecommendation> {
        self.plan
            .policy
            .get(Self::SEMVER_RECOMMENDATION_POLICY_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    }
}

impl Serialize for ReleasePlan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut value = serde_json::to_value(&self.plan).map_err(S::Error::custom)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("release plan did not serialize to a JSON object"))?;

        if let Some(component_id) = self.component_id() {
            object.insert(
                "component_id".to_string(),
                serde_json::Value::String(component_id.to_string()),
            );
        }
        object.insert(
            "enabled".to_string(),
            serde_json::Value::Bool(self.enabled()),
        );
        if let Some(semver_recommendation) = self.semver_recommendation() {
            object.insert(
                "semver_recommendation".to_string(),
                serde_json::to_value(semver_recommendation).map_err(S::Error::custom)?,
            );
        }

        value.serialize(serializer)
    }
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

/// Explicit changelog contract carried by the release plan.
///
/// Changelog entries are generated during planning so dry-run output and real
/// release execution share one source of truth. The release executor consumes
/// this contract when the version step finalizes the changelog on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseChangelogPlan {
    pub policy: String,
    pub path: String,
    pub dry_run: bool,
    pub entries: HashMap<String, Vec<String>>,
    pub entry_count: usize,
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
    pub hints: Vec<crate::core::error::Hint>,
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

/// Mutable state threaded through sequential release execution.
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
    pub changelog_validation: Option<crate::core::release::version::ChangelogValidationResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleasePipelineOptions {
    /// Skip publish/package steps (version bump + tag + push only).
    /// Use when CI handles publishing after the tag is pushed.
    #[serde(default)]
    pub skip_publish: bool,
    /// Finish a release whose version commit and tag already exist at HEAD.
    #[serde(default)]
    pub head: bool,
    /// Existing release artifacts to inventory instead of running release.package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_artifacts: Option<String>,
    /// Deploy after release — defers artifact cleanup until after deployment.
    #[serde(default)]
    pub deploy: bool,
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
    /// Granular per-check skips (e.g. `["lint"]`). Disables only the listed
    /// preflight quality checks while leaving working_tree/remote_sync and the
    /// other checks active. Honored independently of `skip_checks`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_checks_granular: Vec<String>,
    /// Bypass the package/build-structure validation that runs inside the
    /// `preflight.package` and `package` steps, while still running the build
    /// itself. The flag is forwarded to the packaging extension as a generic
    /// `skip_build_validation` config signal; the extension decides which
    /// structure assertions it represents. A build that fails to produce an
    /// artifact still blocks the release — only structure assertions are
    /// bypassed. Use when an operator knows a structure assertion is a false
    /// positive (see issue #5425).
    #[serde(default)]
    pub skip_build_validation: bool,
    #[serde(default, flatten)]
    pub pipeline: ReleasePipelineOptions,
    /// Skip the GitHub Release creation step (tag + notes on github.com).
    /// Use when another pipeline (CI, semantic-release, etc.) already owns that step.
    #[serde(default)]
    pub skip_github_release: bool,
    /// Git identity for release commits: "bot", "Name <email>", or None (use existing config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_identity: Option<String>,
    /// Bump policy controls that affect release plan validation.
    #[serde(default, skip_serializing_if = "ReleaseBumpPolicyOptions::is_default")]
    pub bump_policy: ReleaseBumpPolicyOptions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseBumpPolicyOptions {
    /// Permit a keyword bump lower than the commit-derived recommendation.
    #[serde(default)]
    pub force_lower_bump: bool,
    /// Permit a release when no releasable commits were detected.
    #[serde(default)]
    pub force_empty_release: bool,
    /// Require an explicit `--bump major` for stable major releases.
    #[serde(default)]
    pub require_explicit_major: bool,
}

impl ReleaseBumpPolicyOptions {
    fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReleaseCommandInput {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub recover: bool,
    /// During `--recover`, when the release tag exists but points at a commit
    /// strictly behind HEAD (e.g. config-only commits landed after tagging),
    /// move the tag to HEAD instead of refusing. Guarded: the tagged commit
    /// must be an ancestor of HEAD, HEAD must satisfy the version targets, and
    /// no GitHub Release may exist for the tag.
    #[serde(default)]
    pub retag: bool,
    #[serde(default)]
    pub skip_checks: bool,
    /// Granular per-check skips (e.g. `["lint"]`). Disables only the listed
    /// preflight quality checks while leaving the other gates active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_checks_granular: Vec<String>,
    /// Bypass the package/build-structure validation while still running the
    /// build (see [`ReleaseOptions::skip_build_validation`] and issue #5425).
    #[serde(default)]
    pub skip_build_validation: bool,
    /// Explicit bump override: "major", "minor", "patch", or a version string like "2.0.0".
    /// When set, overrides auto-detection from commit history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bump_override: Option<String>,
    /// Permit a keyword bump lower than the commit-derived recommendation.
    #[serde(default)]
    pub force_lower_bump: bool,
    #[serde(default, flatten)]
    pub pipeline: ReleasePipelineOptions,
    /// Skip the GitHub Release creation step (tag + notes on github.com).
    #[serde(default)]
    pub skip_github_release: bool,
    /// Git identity for release commits: "bot", "Name <email>", or None (use existing config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_identity: Option<String>,
    /// Internal execution contract resolved before the workflow runs.
    #[serde(skip_serializing)]
    pub execution: Option<ReleaseExecutionPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseExecutionPlan {
    pub phase: ReleasePhase,
    pub requires_apply: bool,
    pub apply_risks: Vec<&'static str>,
}

impl ReleaseExecutionPlan {
    pub fn new(phase: ReleasePhase, requires_apply: bool, apply_risks: Vec<&'static str>) -> Self {
        Self {
            phase,
            requires_apply,
            apply_risks,
        }
    }

    pub fn default_for_command_input(input: &ReleaseCommandInput) -> Self {
        let phase = if input.recover {
            ReleasePhase::Recover
        } else if input.dry_run {
            ReleasePhase::Plan
        } else if input.pipeline.deploy {
            ReleasePhase::Deploy
        } else if input.pipeline.skip_publish {
            ReleasePhase::Prepare
        } else {
            ReleasePhase::Publish
        };

        Self::new(phase, false, Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ReleaseDeploymentSummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub skipped: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub planned: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePhase {
    Plan,
    Prepare,
    Publish,
    Recover,
    Deploy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_result: Option<crate::core::deploy::ComponentDeployResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseDeploymentResult {
    pub projects: Vec<ReleaseProjectDeployResult>,
    pub summary: ReleaseDeploymentSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseCommandResult {
    pub component_id: String,
    pub status: String,
    pub phase: ReleasePhase,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release_summary: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_id() {
        let plan = ReleasePlan::new("demo", true, Vec::new(), None, Vec::new(), Vec::new());

        assert_eq!(plan.component_id(), Some("demo"));
    }

    #[test]
    fn test_enabled() {
        let enabled = ReleasePlan::new("demo", true, Vec::new(), None, Vec::new(), Vec::new());
        let disabled = ReleasePlan::new("demo", false, Vec::new(), None, Vec::new(), Vec::new());

        assert!(enabled.enabled());
        assert!(!disabled.enabled());
    }

    #[test]
    fn enabled_falls_back_to_plan_step_state_when_policy_is_absent() {
        let mut plan = HomeboyPlan::for_component(PlanKind::Release, "demo");
        plan.steps = vec![PlanStep::ready("version", "version").build()];

        assert!(ReleasePlan::from_plan(plan).enabled());

        let mut disabled_plan = HomeboyPlan::for_component(PlanKind::Release, "demo");
        disabled_plan.steps = vec![PlanStep::disabled("release.skip", "release.skip").build()];

        assert!(!ReleasePlan::from_plan(disabled_plan).enabled());
    }

    #[test]
    fn test_semver_recommendation() {
        let recommendation = ReleaseSemverRecommendation {
            latest_tag: Some("v1.0.0".to_string()),
            range: "v1.0.0..HEAD".to_string(),
            commits: Vec::new(),
            recommended_bump: Some("minor".to_string()),
            requested_bump: "minor".to_string(),
            is_underbump: false,
            reasons: Vec::new(),
        };
        let plan = ReleasePlan::new(
            "demo",
            true,
            Vec::new(),
            Some(recommendation),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            plan.semver_recommendation()
                .and_then(|recommendation| recommendation.recommended_bump),
            Some("minor".to_string())
        );
    }

    #[test]
    fn release_plan_serializes_legacy_component_fields_from_generic_plan() {
        let plan = ReleasePlan::new("demo", true, Vec::new(), None, Vec::new(), Vec::new());

        let serialized = serde_json::to_value(&plan).expect("serialize release plan");

        assert_eq!(serialized["id"], "release.demo");
        assert_eq!(serialized["kind"], "release");
        assert_eq!(serialized["subject"]["component_id"], "demo");
        assert_eq!(serialized["component_id"], "demo");
        assert_eq!(serialized["enabled"], true);
        assert_eq!(serialized["policy"]["enabled"], true);
        assert!(serialized.get("semver_recommendation").is_none());
    }

    #[test]
    fn release_command_input_defaults_do_not_force_lower_bumps() {
        let input = ReleaseCommandInput::default();

        assert!(!input.force_lower_bump);
    }
}
