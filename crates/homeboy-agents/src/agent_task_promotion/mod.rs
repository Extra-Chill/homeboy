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
pub(crate) mod candidate_bundle;
pub use candidate_bundle::{
    fetch_candidate_bundle, materialize_candidate_tree, verify_candidate_ref, CandidateBundleError,
    CANDIDATE_BUNDLE_ARTIFACT_KIND,
};
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
pub(crate) use promote::gate_setup_outcome_failure;
pub(crate) use promote::{active_gate_supervision, with_gate_supervision};
pub(crate) use promote::{emit_promotion_progress, emit_promotion_progress_with_output};
pub use promote::{
    promote, promote_with_checkpoint, resume_promoted_patch, with_promotion_progress,
    PromotionProgress, PromotionProgressCallback,
};
pub(crate) use promote::{
    promote_with_checkpoint_in_observation_store, resume_promoted_patch_in_observation_store,
    resume_promoted_patch_replacement_gates_in_observation_store, validate_candidate_commit,
};
pub use run_plan_projection::mirror_agent_task_run_plan_aggregate;
pub use types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionReport,
    AgentTaskPromotionRequest, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

#[cfg(test)]
pub(crate) mod tests;
