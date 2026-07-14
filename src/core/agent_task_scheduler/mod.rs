//! Durable agent-task scheduling public surface.
//!
//! Execution, attempt-workspace isolation, and artifact harvesting stay in the
//! execution child module; scheduling policy and outcome construction remain
//! independently focused siblings.

mod execution;
mod outcome;
mod scheduling;
#[cfg(test)]
mod tests;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::agent_task_timeout_artifacts::{
    append_unique_artifacts, append_unique_evidence_refs, is_actionable_patch_artifact,
    is_empty_patch_artifact, merge_timeout_outcome, TimeoutArtifactDiscovery,
};
pub use execution::*;
pub(crate) use execution::{fingerprint, git_output, git_output_raw};
use execution::{QuarantinedTask, ResourceWait, RunningTask, ScheduledTask};
pub(crate) use scheduling::AgentTaskScheduleSupport;
