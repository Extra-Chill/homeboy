//! Agent-task promotion: validate a succeeded outcome's patch artifact and
//! promote it into a managed worktree through a workspace provider, capturing
//! deterministic verification gate evidence.
//!
//! Split into focused submodules:
//! - [`types`]: promotion report data structures, status, and schema consts.
//! - [`promote`]: the promote entrypoint, patch normalization, and validation.
//! - [`apply`]: the workspace provider trait and external provider transport.

mod apply;
mod promote;
mod types;

pub use promote::promote;
pub use types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

#[cfg(test)]
mod tests;
