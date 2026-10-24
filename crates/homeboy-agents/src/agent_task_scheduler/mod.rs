//! Durable agent-task scheduling public surface.
//!
//! The run loop, isolated attempt workspace, harvest, scheduling policy, and
//! outcome construction modules remain independently focused siblings.

mod attempt_workspace;
mod candidate_adoption;
mod engine;
mod harvest;
pub mod managed_services;
mod outcome;
mod outcome_artifacts;
mod outcome_status;
mod postprocess;
mod resources;
mod scheduling;
#[cfg(test)]
mod tests;

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::agent_task_evidence_discovery::{
    append_unique_artifacts, append_unique_evidence_refs, is_actionable_patch_artifact,
    is_empty_patch_artifact, merge_timeout_outcome, TimeoutArtifactDiscovery,
};
pub use crate::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateTotals,
    AgentTaskArtifactLineage, AgentTaskArtifactOutputDeclaration, AgentTaskArtifactPostprocessStep,
    AgentTaskArtifactRunBinding, AgentTaskBackpressureStatus, AgentTaskCancellationToken,
    AgentTaskCandidateAdoption, AgentTaskCandidateAdoptionDecision,
    AgentTaskCandidateCompletionPolicy, AgentTaskChildRun, AgentTaskExecutionBudget,
    AgentTaskExecutionContext, AgentTaskManagedService, AgentTaskManagedServiceBrowserOriginProbe,
    AgentTaskManagedServiceLifecycle, AgentTaskManagedServiceReadiness,
    AgentTaskManagedServiceReadinessKind, AgentTaskPlan, AgentTaskProgressEvent,
    AgentTaskProviderRotationAttempt, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskQueueStatus, AgentTaskResourceBudget,
    AgentTaskResourceBudgetStatus, AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
    AGENT_TASK_AGGREGATE_SCHEMA, AGENT_TASK_PLAN_SCHEMA,
};
use crate::agent_task_timeout::timeout_with_grace;
#[cfg(test)]
pub(crate) use attempt_workspace::set_snapshot_fence_test_hook;
pub use attempt_workspace::HarvestExecutionContext;
use attempt_workspace::{
    prepare_attempt_workspace, prepare_committed_harvest, remap_workspace_config, AttemptWorkspace,
};
use candidate_adoption::{
    attach_candidate_adoption_provenance, select_candidate_adoption,
    validate_and_apply_candidate_adoption,
};
pub use engine::*;
use engine::{QuarantinedTask, ResourceWait, RunningTask, ScheduledTask};
use harvest::mark_timeout_workspace_candidates_incomplete;
pub(crate) use harvest::{candidate_from_workspace, git_output, HarvestError};
use harvest::{
    committed_harvest_failure, committed_harvest_preflight_outcome, harvest_candidate_commit,
};
use outcome::event;
pub use postprocess::run_postprocess_worker;
use resources::{
    active_resource_units, adaptive_concurrency_decision, executor_key, model_key,
    task_resource_units,
};
pub use resources::{
    resolve_batch_concurrency, BatchConcurrencyDecision, BatchConcurrencyInputs,
    BatchConcurrencySource,
};
pub(crate) use scheduling::AgentTaskScheduleSupport;
