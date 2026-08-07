mod advanced_remote;
pub mod cascade;
mod checkout_guard;
// "Is my fix released yet?" — commit→release containment and the inverse
// installed-versus-latest gap (#11754). Public because the CLI surfaces it
// directly and it is pure git plus release metadata.
pub mod containment;
mod context;
mod deployment;
mod execution_dispatch;
mod execution_plan;
mod execution_projection;
mod executor;
// Durable operation/finalization records. Lived in homeboy-core until #11143;
// the release workspace finalizer is their only consumer.
pub mod operation_record;
mod orchestrator;
mod package_recovery;
mod pipeline;
mod pipeline_capabilities;
mod pipeline_summary;
mod plan_steps;
mod planner;
mod planning_changelog;
mod planning_git;
mod planning_policy;
mod planning_quality;
mod planning_semver;
mod planning_worktree;
mod preflight_identity;
pub use homeboy_deploy::provider_impl;
mod types;
mod utils;
mod version_guard;
mod workflow;
mod workflow_recover;
mod workspace;

// `changelog`, `scope`, and the version readers moved down into
// `homeboy-version` so `homeboy-deploy` can use them without depending on this
// crate (#11144). Re-exported under their original names so every release-side
// `super::scope::`, `crate::release::changelog::`, and `version::` path keeps
// resolving unchanged.
pub use homeboy_version::{changelog, scope};

/// The shared version primitives, plus the release-only version mutation guard.
///
/// `version_guard` is the one piece of the old `version` module that could not
/// move down: it reaches into `planning_worktree` for release-owned lockfile
/// derivation. Re-merging the two here keeps that split invisible to callers,
/// which is why `version::release_owned_lockfiles` still resolves.
pub mod version {
    pub use crate::release::version_guard::*;
    pub use homeboy_version::version::*;
}

pub use cascade::{run_cascade, CascadeResult, CascadeStepResult, ReleasedCoordinates};
pub use containment::{
    ContainmentAssessment, ContainmentStatus, ContainsQuery, GapStatus, ReleaseContainsReport,
    ReleaseGapAssessment, ReleaseGapReport,
};
pub use context::readiness_provenance;
pub use executor::artifacts::{
    write_artifact_source_authority_manifest, ArtifactSourceAuthorityManifest,
};
pub use package_recovery::{package_existing_tag, ReleasePackageResult};
pub use pipeline::run;
pub use planner::plan;
pub use types::readiness_is_valid;
pub use types::{
    BatchReleaseComponentResult, BatchReleaseResult, BatchReleaseSummary, ReleaseArtifact,
    ReleaseCommandInput, ReleaseCommandResult, ReleaseDeploymentResult, ReleaseDeploymentSummary,
    ReleaseExecutionPlan, ReleaseOptions, ReleasePhase, ReleasePipelineOptions, ReleasePlan,
    ReleasePreflightPlacement, ReleasePreflightPlacementPolicy, ReleasePreflightSourceIdentity,
    ReleaseProjectDeployResult, ReleaseReadinessEnvelope, ReleaseReadinessGateResult,
    ReleaseReadinessLocalOnly, ReleaseReadinessProvenance, ReleaseRollbackEvidence, ReleaseRun,
    ReleaseRunResult, ReleaseRunSummary, ReleaseSemverCommit, ReleaseSemverRecommendation,
    ReleaseStepResult, ReleaseStepStatus, ReleaseWorkspaceCommandResult, ReleaseWorkspaceOutput,
};
pub use utils::{extract_latest_notes, parse_release_artifacts};
pub use workflow::{
    run_batch, run_command, run_command_with_recovery_owner, run_command_with_workspace,
    SKIPPED_RELEASE_EXIT_CODE,
};

// The component tag-naming contract moved to `homeboy-version` alongside
// `scope`. Deploy resolves release tags too (tag-gap detection, ref checkout),
// so the contract has to sit below both subsystems rather than inside release.
pub use homeboy_version::{component_tag_name, component_tag_prefix, latest_component_tag};

/// Whether this component would normally get a reviewer-facing GitHub Release
/// created as part of a release (i.e. it resolves to a GitHub remote).
///
/// Used by the CLI to decide whether `--no-github-release` is a sharp,
/// confirmation-gated override on a manual/local release: suppressing the
/// GitHub Release only matters when one would otherwise be created.
pub fn github_release_expected(component: &homeboy_core::component::Component) -> bool {
    plan_steps::github_release_applies(component)
}
