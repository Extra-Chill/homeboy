mod advanced_remote;
pub mod cascade;
pub mod changelog;
mod checkout_guard;
mod context;
mod deployment;
mod execution_dispatch;
mod execution_plan;
mod execution_projection;
mod executor;
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
mod types;
mod utils;
pub mod version;
mod workflow;
mod workflow_recover;

pub use cascade::{run_cascade, CascadeResult, CascadeStepResult, ReleasedCoordinates};
pub use package_recovery::{package_existing_tag, ReleasePackageResult};
pub use pipeline::run;
pub use planner::plan;
pub use types::{
    BatchReleaseComponentResult, BatchReleaseResult, BatchReleaseSummary, ReleaseArtifact,
    ReleaseCommandInput, ReleaseCommandResult, ReleaseDeploymentResult, ReleaseDeploymentSummary,
    ReleaseExecutionPlan, ReleaseOptions, ReleasePhase, ReleasePipelineOptions, ReleasePlan,
    ReleaseProjectDeployResult, ReleaseRun, ReleaseRunResult, ReleaseRunSummary,
    ReleaseSemverCommit, ReleaseSemverRecommendation, ReleaseStepResult, ReleaseStepStatus,
};
pub use utils::{extract_latest_notes, parse_release_artifacts};
pub use workflow::{run_batch, run_command, SKIPPED_RELEASE_EXIT_CODE};

/// Whether this component would normally get a reviewer-facing GitHub Release
/// created as part of a release (i.e. it resolves to a GitHub remote).
///
/// Used by the CLI to decide whether `--no-github-release` is a sharp,
/// confirmation-gated override on a manual/local release: suppressing the
/// GitHub Release only matters when one would otherwise be created.
pub fn github_release_expected(component: &crate::core::component::Component) -> bool {
    plan_steps::github_release_applies(component)
}
