mod advanced_remote;
mod cascade;
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
// the release workspace finalizer is their only consumer. Public because
// `homeboy release readiness show` reads records back out of the store.
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
// `homeboy-core` reaches release/deploy behavior only through the
// `release_provider` hook; the CLI registers this implementation at startup.
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
// resolving unchanged. `changelog` is public because `homeboy release
// changelog` and `homeboy review` surface it directly; `scope` is internal.
pub use homeboy_version::changelog;
pub(crate) use homeboy_version::scope;

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

// Public API — every item below is reached from `homeboy-cli`. Anything the CLI
// does not name stays `pub(crate)` so `dead_code` can see it.
pub use cascade::{run_cascade, CascadeResult, ReleasedCoordinates};
pub use containment::{ContainsQuery, ReleaseContainsReport, ReleaseGapReport};
pub use context::readiness_provenance;
pub use executor::artifacts::{
    write_artifact_source_authority_manifest, ArtifactSourceAuthorityManifest,
};
pub use executor::release_notes_path;
pub use package_recovery::{package_existing_tag, ReleasePackageResult};
pub use pipeline::run;
pub use types::readiness_is_valid;
pub use types::{
    BatchReleaseResult, ReleaseCommandInput, ReleaseCommandResult, ReleaseExecutionPlan,
    ReleasePhase, ReleasePipelineOptions, ReleasePreflightPlacement,
    ReleasePreflightPlacementPolicy, ReleasePreflightSourceIdentity, ReleaseReadinessEnvelope,
    ReleaseReadinessGateResult, ReleaseReadinessLocalOnly, ReleaseReadinessProvenance,
    ReleaseWorkspaceOutput,
};
pub use workflow::{run_batch, run_command_with_workspace, SKIPPED_RELEASE_EXIT_CODE};

// Crate-internal re-exports: submodules reach these through `super::`/
// `crate::release::` paths, so the alias has to exist, but nothing outside the
// crate names them.
pub(crate) use homeboy_version::component_tag_name;
pub(crate) use planner::plan;
pub(crate) use workflow::run_command;

// The component tag-naming contract moved to `homeboy-version` alongside
// `scope`. Deploy resolves release tags too (tag-gap detection, ref checkout),
// so the contract has to sit below both subsystems rather than inside release.
// `homeboy status` derives its tag prefix through this re-export.
pub use homeboy_version::component_tag_prefix;

/// Whether this component would normally get a reviewer-facing GitHub Release
/// created as part of a release (i.e. it resolves to a GitHub remote).
///
/// Used by the CLI to decide whether `--no-github-release` is a sharp,
/// confirmation-gated override on a manual/local release: suppressing the
/// GitHub Release only matters when one would otherwise be created.
pub fn github_release_expected(component: &homeboy_core::component::Component) -> bool {
    plan_steps::github_release_applies(component)
}
