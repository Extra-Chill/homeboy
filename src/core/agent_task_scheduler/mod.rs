//! Durable agent-task scheduling public surface.
//!
//! The run loop, isolated attempt workspace, harvest, scheduling policy, and
//! outcome construction modules remain independently focused siblings.

mod attempt_workspace;
mod candidate_adoption;
mod engine;
mod harvest;
mod outcome;
mod outcome_artifacts;
mod outcome_status;
mod outcome_templates;
mod resources;
mod scheduling;
#[cfg(test)]
mod tests;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_OUTCOME_SCHEMA,
};
pub use crate::core::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
    AgentTaskArtifactOutputDeclaration, AgentTaskArtifactRunBinding, AgentTaskBackpressureStatus,
    AgentTaskCancellationToken, AgentTaskCandidateAdoption, AgentTaskCandidateAdoptionDecision,
    AgentTaskChildRun, AgentTaskExecutionBudget, AgentTaskExecutionContext, AgentTaskOutputBinding,
    AgentTaskOutputDependencies, AgentTaskPlan, AgentTaskProgressEvent,
    AgentTaskProviderRotationAttempt, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskQueueStatus, AgentTaskResourceBudget,
    AgentTaskResourceBudgetStatus, AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
    AGENT_TASK_AGGREGATE_SCHEMA, AGENT_TASK_PLAN_SCHEMA,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::agent_task_timeout_artifacts::{
    append_unique_artifacts, append_unique_evidence_refs, is_actionable_patch_artifact,
    is_empty_patch_artifact, merge_timeout_outcome, TimeoutArtifactDiscovery,
};
use attempt_workspace::{
    prepare_attempt_workspace, prepare_committed_harvest, remap_workspace_config, AttemptWorkspace,
};
use candidate_adoption::{
    attach_candidate_adoption_provenance, finalize_candidate_artifacts, select_candidate_adoption,
    validate_and_apply_candidate_adoption,
};
pub use engine::*;
use engine::{QuarantinedTask, ResourceWait, RunningTask, ScheduledTask};
use harvest::{
    committed_harvest_failure, committed_harvest_preflight_outcome, harvest_committed_patch,
    harvest_uncommitted_patch,
};
pub(crate) use harvest::{git_output, HarvestError};
use outcome::event;
use resources::{
    active_resource_units, adaptive_concurrency_decision, executor_key, model_key,
    task_resource_units,
};
pub(crate) use scheduling::AgentTaskScheduleSupport;
