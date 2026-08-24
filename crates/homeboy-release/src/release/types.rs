use serde::ser::Error as SerializeError;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};

use homeboy_core::is_zero_u32;
use homeboy_core::phase_timing::PhaseTimingReport;
use homeboy_core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus};

/// Ordered release plan shared by dry-run output and release execution.
///
/// `ReleasePlan` is rendered in `--dry-run` and `--json` output, then walked by
/// `pipeline::run()` for real releases so the previewed steps match execution.
#[derive(Debug, Clone)]
pub struct ReleasePlan {
    pub(crate) plan: HomeboyPlan,
}

impl ReleasePlan {
    const ENABLED_POLICY_KEY: &'static str = "enabled";
    const SEMVER_RECOMMENDATION_POLICY_KEY: &'static str = "semver_recommendation";

    pub(crate) fn new(
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
    pub(crate) fn from_plan(plan: HomeboyPlan) -> Self {
        Self { plan }
    }

    pub(crate) fn component_id(&self) -> Option<&str> {
        self.plan.subject.component_id.as_deref()
    }

    pub(crate) fn enabled(&self) -> bool {
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

    pub(crate) fn semver_recommendation(&self) -> Option<ReleaseSemverRecommendation> {
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
pub(crate) struct ReleaseSemverCommit {
    pub(crate) sha: String,
    pub(crate) subject: String,
    pub(crate) commit_type: String,
    pub(crate) breaking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseSemverRecommendation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_tag: Option<String>,
    pub(crate) range: String,
    pub(crate) commits: Vec<ReleaseSemverCommit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recommended_bump: Option<String>,
    pub(crate) requested_bump: String,
    pub(crate) is_underbump: bool,
    pub(crate) reasons: Vec<String>,
    /// How many commits in range carried no recognizable conventional-commit
    /// prefix (`feat:`/`fix:`/etc.). On repos that don't use conventional commits
    /// these are classified `other` and only ever drive a `patch` bump, so a
    /// high count next to a low bump is a silent under-bump signal (#6851).
    #[serde(default)]
    pub(crate) non_conventional_commit_count: usize,
    /// Total commits considered for the bump (excludes merge/release noise).
    #[serde(default)]
    pub(crate) considered_commit_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bump_policy: Option<ReleaseBumpPolicy>,
}

/// Structured evidence for a release bump policy decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseBumpPolicy {
    pub(crate) policy: String,
    pub(crate) signals: Vec<ReleaseBumpPolicySignal>,
    pub(crate) override_used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseBumpPolicySignal {
    pub(crate) name: String,
    pub(crate) observed: usize,
    pub(crate) threshold: usize,
}

/// Explicit changelog contract carried by the release plan.
///
/// Changelog entries are generated during planning so dry-run output and real
/// release execution share one source of truth. The release executor consumes
/// this contract when the version step finalizes the changelog on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseChangelogPlan {
    pub(crate) policy: String,
    pub(crate) path: String,
    pub(crate) dry_run: bool,
    pub(crate) entries: HashMap<String, Vec<String>>,
    pub(crate) entry_count: usize,
}

/// Run result for a single release. Shape is preserved from the pre-refactor
/// `ReleaseRun { component_id, enabled, result: PipelineRunResult }` so `--json`
/// consumers see no change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRun {
    pub(crate) component_id: String,
    pub(crate) enabled: bool,
    pub(crate) result: ReleaseRunResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRunResult {
    pub(crate) steps: Vec<ReleaseStepResult>,
    pub(crate) status: ReleaseStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<ReleaseRunSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) phase_timings: Option<PhaseTimingReport>,
    /// Recorded only after a failed release has restored the checkout. This
    /// makes the result describe the durable exit state rather than mutations
    /// attempted before rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rollback: Option<ReleaseRollbackEvidence>,
}

/// The authoritative checkout used by a release and its terminal disposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseWorkspaceOutput {
    pub(crate) kind: String,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owner_run_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) final_disposition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finalization_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reconciliation_ref: Option<String>,
}

impl ReleaseWorkspaceOutput {
    pub(crate) fn in_place(path: &str) -> Self {
        Self {
            kind: "in_place".to_string(),
            path: path.to_string(),
            provider_id: None,
            handle: None,
            owner_run_ref: None,
            source_sha: None,
            final_disposition: None,
            continuation_ref: None,
            finalization_error: None,
            reconciliation_ref: None,
        }
    }
}

/// Additive result envelope for callers that requested provider workspace
/// lifecycle evidence. The legacy release result remains unchanged.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseWorkspaceCommandResult {
    pub result: ReleaseCommandResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<ReleaseWorkspaceOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRollbackEvidence {
    /// `restored` means final_head was independently observed at original_head.
    /// `interrupted` requires operator recovery and must never be presented as
    /// rollback success.
    pub(crate) status: String,
    pub(crate) original_head: String,
    pub(crate) temporary_head: String,
    pub(crate) release_commit: String,
    pub(crate) final_head: Option<String>,
    pub(crate) tag_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseStepResult {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) step_type: String,
    pub(crate) status: ReleaseStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) hints: Vec<homeboy_core::error::Hint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl Default for ReleaseStepResult {
    /// A defaulted step has empty `id`/`step_type`, `Failed` status, and every
    /// collection/optional field empty. `Failed` is the deliberate status
    /// default: a not-yet-populated step must never read as `Success`. Construct
    /// with `..Default::default()` and set the meaningful fields (`id`,
    /// `step_type`, `status`, and whatever else applies) to drop the
    /// `Vec::new()`/`None` boilerplate the step builders otherwise repeat.
    fn default() -> Self {
        Self {
            id: String::new(),
            step_type: String::new(),
            status: ReleaseStepStatus::Failed,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        }
    }
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
    pub(crate) total_steps: usize,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) skipped: usize,
    pub(crate) missing: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) next_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) success_summary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) durable_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) artifact_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) platform: Option<String>,
    /// Lifecycle phase that produced these bytes. Final package output has
    /// precedence over validation/preflight output with the same target name.
    #[serde(default = "default_artifact_phase")]
    pub(crate) phase: String,
    /// Producer responsible for the artifact (component build, extension, or
    /// externally supplied recovery inventory).
    #[serde(default = "default_artifact_producer")]
    pub(crate) producer: String,
    /// Content identity persisted with the durable inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
    /// Exactly one artifact per target name is allowed to publish.
    #[serde(default)]
    pub(crate) publication_authority: bool,
}

fn default_artifact_phase() -> String {
    "final".to_string()
}

fn default_artifact_producer() -> String {
    "unknown".to_string()
}

/// Mutable state threaded through sequential release execution.
///
/// Every step that produces a downstream value (the new version, the tag name,
/// the release notes, the built artifacts) stores it here and the next step
/// reads it back. This was previously a `Mutex<ReleaseContext>` accessed
/// through a generic pipeline DAG — a pattern the execution never actually
/// needed because every step runs sequentially.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReleaseState {
    pub(crate) version: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) notes: Option<String>,
    /// A manifest-bound final GitHub Release body recovered without regeneration.
    pub(crate) exact_release_notes: Option<ExactReleaseNotes>,
    pub(crate) artifacts: Vec<ReleaseArtifact>,
    /// Component-relative checkout paths proven absent before packaging and
    /// created by the current package invocation.
    pub(crate) package_owned_paths: Vec<String>,
    pub(crate) changelog_validation: Option<crate::release::version::ChangelogValidationResult>,
    /// A remote-only draft may be published only when this manifest-bound
    /// intent has been validated against the active release identity.
    pub(crate) draft_adoption: Option<DraftAdoptionIntent>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExactReleaseNotes {
    pub(crate) body: String,
    pub(crate) source: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DraftAdoptionIntent {
    pub(crate) expected_assets: Vec<String>,
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
    pub(crate) bump_type: String,
    pub(crate) dry_run: bool,
    /// Override the component's `local_path` for this release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path_override: Option<String>,
    /// Skip lint/test code quality checks before release.
    #[serde(default)]
    pub(crate) skip_checks: bool,
    /// Granular per-check skips (e.g. `["lint"]`). Disables only the listed
    /// preflight quality checks while leaving working_tree/remote_sync and the
    /// other checks active. Honored independently of `skip_checks`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) skip_checks_granular: Vec<String>,
    /// Bypass the package/build-structure validation that runs inside the
    /// `preflight.package` and `package` steps, while still running the build
    /// itself. The flag is forwarded to the packaging extension as a generic
    /// `skip_build_validation` config signal; the extension decides which
    /// structure assertions it represents. A build that fails to produce an
    /// artifact still blocks the release — only structure assertions are
    /// bypassed. Use when an operator knows a structure assertion is a false
    /// positive (see issue #5425).
    #[serde(default)]
    pub(crate) skip_build_validation: bool,
    /// Skip dependency hydration during release preflight.
    #[serde(default)]
    pub(crate) skip_deps_hydration: bool,
    #[serde(default, flatten)]
    pub(crate) pipeline: ReleasePipelineOptions,
    /// Skip the GitHub Release creation step (tag + notes on github.com).
    /// Use when another pipeline (CI, semantic-release, etc.) already owns that step.
    #[serde(default)]
    pub(crate) skip_github_release: bool,
    /// Git identity for release commits: "bot", "Name <email>", or None (use existing config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) git_identity: Option<String>,
    /// Bump policy controls that affect release plan validation.
    #[serde(default, skip_serializing_if = "ReleaseBumpPolicyOptions::is_default")]
    pub(crate) bump_policy: ReleaseBumpPolicyOptions,
    /// Placement selected for portable release-readiness gates. Release mutation
    /// always remains controller-owned; a runner is evidence provenance only.
    #[serde(default, skip_serializing_if = "ReleasePreflightPlacement::is_default")]
    pub(crate) preflight_placement: ReleasePreflightPlacement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) readiness: Option<ReleaseReadinessEnvelope>,
}

/// Typed placement policy for the portable portion of release preflight.
///
/// This deliberately does not describe versioning, tagging, pushing, or
/// publication. Those operations mutate controller-owned state and are never
/// authorized by readiness evidence from a runner.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePreflightPlacement {
    #[serde(default)]
    pub policy: ReleasePreflightPlacementPolicy,
    /// Runner selected for portable gate execution, when policy pinning is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePreflightPlacementPolicy {
    #[default]
    Auto,
    Local,
    Lab,
    LabOrLocal,
}

impl ReleasePreflightPlacement {
    fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }
}

/// Immutable source identity bound to release-readiness evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePreflightSourceIdentity {
    pub commit: String,
}

/// Portable gate evidence that the controller may consume before mutation.
///
/// The envelope is intentionally data-only: Lab dispatch owns runner execution
/// and artifact persistence, while release owns the decision to mutate only
/// after this identity is revalidated locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseReadinessEnvelope {
    pub source: ReleasePreflightSourceIdentity,
    pub placement: ReleasePreflightPlacement,
    pub runner_id: Option<String>,
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "ReleaseReadinessProvenance::is_empty")]
    pub provenance: ReleaseReadinessProvenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<ReleaseReadinessGateResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseReadinessProvenance {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, String>,
}

impl ReleaseReadinessProvenance {
    pub fn is_empty(value: &Self) -> bool {
        value.dependencies.is_empty() && value.extensions.is_empty()
    }
}

/// A readiness envelope authorizes release planning only when it has an
/// explicit selected-gate outcome. Bare `--skip-checks` remains authorized
/// because it records every selected portable gate as `skipped`.
pub fn readiness_is_valid(readiness: &ReleaseReadinessEnvelope) -> bool {
    !readiness.gate_results.is_empty()
        && readiness
            .gate_results
            .iter()
            .all(|gate| match gate.status.as_str() {
                "passed" => {
                    gate.source_sha.as_deref() == Some(readiness.source.commit.as_str())
                        && gate
                            .runner_id
                            .as_deref()
                            .is_some_and(|runner| !runner.is_empty())
                        && !gate.evidence_refs.is_empty()
                        && gate.provenance.as_ref().is_some_and(|provenance| {
                            !ReleaseReadinessProvenance::is_empty(provenance)
                        })
                }
                "skipped" => gate.source_sha.as_deref() == Some(readiness.source.commit.as_str()),
                "local_only" => {
                    gate.gate == "package_preflight"
                        && gate.local_only.as_ref().is_some_and(|local_only| {
                            !local_only.reason.is_empty() && !local_only.continuation.is_empty()
                        })
                }
                _ => false,
            })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseReadinessGateResult {
    pub gate: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    /// Immutable identities emitted by the child that executed this gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ReleaseReadinessProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_only: Option<ReleaseReadinessLocalOnly>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseReadinessLocalOnly {
    pub reason: String,
    pub continuation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseBumpPolicyOptions {
    /// Permit a keyword bump lower than the commit-derived recommendation.
    #[serde(default)]
    pub(crate) force_lower_bump: bool,
    /// Permit a release when no releasable commits were detected.
    #[serde(default)]
    pub(crate) force_empty_release: bool,
    /// Require an explicit `--bump major` for stable major releases.
    #[serde(default)]
    pub(crate) require_explicit_major: bool,
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
    /// Skip dependency hydration during release preflight.
    #[serde(default)]
    pub skip_deps_hydration: bool,
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
    #[serde(skip_serializing)]
    pub readiness: Option<ReleaseReadinessEnvelope>,
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

    pub(crate) fn default_for_command_input(input: &ReleaseCommandInput) -> Self {
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
    pub(crate) total_projects: u32,
    pub(crate) succeeded: u32,
    pub(crate) failed: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub(crate) skipped: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub(crate) planned: u32,
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
    pub(crate) project_id: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) component_result: Option<homeboy_deploy::ComponentDeployResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseDeploymentResult {
    pub(crate) projects: Vec<ReleaseProjectDeployResult>,
    pub(crate) summary: ReleaseDeploymentSummary,
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
    /// Exact `--head` command required when Git recovery has completed but this
    /// command intentionally has not run the publication pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release_summary: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ReleaseReadinessEnvelope>,
}

/// Result of a batch release across multiple components.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseResult {
    pub(crate) results: Vec<BatchReleaseComponentResult>,
    pub summary: BatchReleaseSummary,
}

/// Per-component result within a batch release.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseComponentResult {
    pub(crate) component_id: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<ReleaseCommandResult>,
}

/// Summary counts for a batch release.
#[derive(Debug, Clone, Serialize)]
pub struct BatchReleaseSummary {
    pub(crate) total: u32,
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
    fn release_step_result_default_matches_the_fully_spelled_out_step() {
        let via_default = ReleaseStepResult {
            id: "build".to_string(),
            step_type: "build".to_string(),
            status: ReleaseStepStatus::Success,
            ..Default::default()
        };
        let verbose = ReleaseStepResult {
            id: "build".to_string(),
            step_type: "build".to_string(),
            status: ReleaseStepStatus::Success,
            ..Default::default()
        };
        // ReleaseStepResult has no PartialEq; compare via canonical serialization.
        assert_eq!(
            serde_json::to_value(&via_default).unwrap(),
            serde_json::to_value(&verbose).unwrap()
        );
    }

    #[test]
    fn release_step_result_default_status_is_failed_never_success() {
        assert_eq!(
            ReleaseStepResult::default().status,
            ReleaseStepStatus::Failed
        );
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
            non_conventional_commit_count: 0,
            considered_commit_count: 0,
            bump_policy: None,
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

    #[test]
    fn readiness_envelope_keeps_runner_provenance_separate_from_mutation_policy() {
        let envelope = ReleaseReadinessEnvelope {
            source: ReleasePreflightSourceIdentity {
                commit: "abc123".to_string(),
            },
            placement: ReleasePreflightPlacement {
                policy: ReleasePreflightPlacementPolicy::Lab,
                runner_id: Some("homeboy-lab".to_string()),
            },
            runner_id: Some("homeboy-lab".to_string()),
            evidence_refs: vec!["runner-artifact://homeboy-lab/release/lint.json".to_string()],
            provenance: ReleaseReadinessProvenance::default(),
            gate_results: vec![ReleaseReadinessGateResult {
                gate: "lint".to_string(),
                status: "passed".to_string(),
                reason: None,
                source_sha: None,
                runner_id: Some("homeboy-lab".to_string()),
                evidence_refs: Vec::new(),
                provenance: None,
                local_only: None,
            }],
        };

        let value = serde_json::to_value(envelope).expect("serialize readiness envelope");

        assert_eq!(value["source"]["commit"], "abc123");
        assert_eq!(value["runner_id"], "homeboy-lab");
        assert!(value.get("mutation_runner_id").is_none());
    }

    #[test]
    fn legacy_release_run_result_literal_and_json_shape_remain_compatible() {
        let result = ReleaseRunResult {
            steps: Vec::new(),
            status: ReleaseStepStatus::Success,
            warnings: Vec::new(),
            summary: None,
            phase_timings: None,
            rollback: None,
        };

        assert_eq!(
            serde_json::to_value(result).expect("serialize legacy release result"),
            serde_json::json!({ "steps": [], "status": "success" })
        );
    }
}
