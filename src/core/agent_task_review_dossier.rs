mod model;
mod profile;
mod rendering;
mod validation;

pub use model::{
    AgentTaskReviewAdditionalSection, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewEvidence, AgentTaskReviewIssueRelationship,
    AgentTaskReviewIssueRelationshipKind, AgentTaskReviewOverride, AgentTaskReviewOverrideTarget,
    AgentTaskReviewProfile, AgentTaskReviewSectionId, AgentTaskReviewTestStep,
};
pub use profile::{default_profile, resolve_review_profile};
pub use rendering::{enrich_dossier, render_review_dossier};
pub use validation::validate_profile;

pub const AGENT_TASK_REVIEW_DOSSIER_SCHEMA: &str = "homeboy/agent-task-review-dossier/v1";

#[cfg(test)]
mod tests;
