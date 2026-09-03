//! Agent-task promotion: validate a succeeded outcome's patch artifact and
//! promote it into a native managed worktree, capturing
//! deterministic verification gate evidence.
//!
//! Split into focused submodules:
//! - [`types`]: promotion report data structures, status, and schema consts.
//! - [`promote`]: the promotion entrypoint and report assembly.
//! - [`committed_changes`]: committed-change discovery and evidence.
//! - [`patch`]: patch normalization and validation.
//! - [`apply`]: native workspace mutation and the serialized Lab adapter.

mod apply;
mod committed_changes;
pub(crate) use committed_changes::resolve_candidate_revision;
mod fingerprint;
mod patch;
mod promote;
mod run_plan_projection;
mod types;

pub use apply::{apply_materialized_workspace_patch, preflight_managed_workspace};
pub use fingerprint::{
    candidate_fingerprint, AgentTaskCandidateFingerprint, AgentTaskPromotionCandidate,
};
pub(crate) use patch::{normalize_promotion_patch, validate_artifact_content};
pub(crate) use promote::capture_declared_base;
pub(crate) use promote::emit_promotion_progress;
pub(crate) use promote::with_gate_supervision;
pub use promote::{canonical_recoverable_patch_artifacts, CanonicalRecoverablePatchArtifacts};
pub(crate) use promote::{
    canonical_recoverable_patch_artifacts_in_observation_store, outcome_has_patch_artifacts,
    preflight_patch_artifact_admission_in_observation_store,
    preflight_recoverable_candidate_promotion_in_observation_store,
    promote_with_checkpoint_in_observation_store, resume_promoted_patch_in_observation_store,
    resume_promoted_patch_replacement_gates_in_observation_store,
};
pub use promote::{
    promote, promote_with_checkpoint, resume_promoted_patch, with_promotion_progress,
    PromotionProgress, PromotionProgressCallback,
};
pub use run_plan_projection::mirror_agent_task_run_plan_aggregate;
pub use types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

#[cfg(test)]
mod tests;
