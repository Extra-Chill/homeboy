//! Agent-task promotion: validate a succeeded outcome's patch artifact and
//! promote it into a managed worktree through a workspace provider, capturing
//! deterministic verification gate evidence.
//!
//! Split into focused submodules:
//! - [`types`]: promotion report data structures, status, and schema consts.
//! - [`promote`]: the promotion entrypoint and report assembly.
//! - [`committed_changes`]: committed-change discovery and evidence.
//! - [`patch`]: patch normalization and validation.
//! - [`apply`]: the workspace provider trait and external provider transport.

mod apply;
mod committed_changes;
mod fingerprint;
mod patch;
mod promote;
mod types;

pub use apply::{apply_materialized_workspace_patch, preflight_configured_workspace_provider};
pub use fingerprint::{
    candidate_fingerprint, AgentTaskCandidateFingerprint, AgentTaskPromotionCandidate,
};
pub(crate) use patch::{normalize_promotion_patch, validate_artifact_content};
pub use promote::promote;
pub use promote::promote_with_checkpoint;
pub use promote::resume_promoted_patch;
pub use types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

#[cfg(test)]
mod tests;
