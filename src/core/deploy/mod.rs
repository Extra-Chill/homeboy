mod execution;
mod orchestration;
pub(crate) mod permissions;
mod planning;
mod safety_and_artifact;
mod transfer;
mod types;
mod version_overrides;

// Public API — re-export types and entry points used outside the deploy module
pub use planning::{bucket_release_states, calculate_release_state, classify_release_state};
pub use types::{
    parse_bulk_component_ids, ComponentDeployResult, ComponentStatus, DeployConfig,
    DeployOrchestrationResult, DeployReason, DeploySummary, ReleaseState, ReleaseStateBuckets,
    ReleaseStateStatus,
};

use crate::context::resolve_project_ssh_with_base_path;
use crate::error::Result;
use crate::project;

/// High-level deploy entry point. Resolves SSH context internally.
///
/// This is the preferred entry point for callers - it handles project loading
/// and SSH context resolution, keeping those details encapsulated.
pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    orchestration::deploy_components(config, &project, &ctx, &base_path)
}
