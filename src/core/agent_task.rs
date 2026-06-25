pub use super::agent_task_aggregate::{
    AgentTaskAggregateReport, AgentTaskAggregateSummary, AgentTaskArtifactInventoryItem,
    AgentTaskDecisionRef, AgentTaskMatrixRow, AgentTaskReconciliationDecision,
    AgentTaskReconciliationItem, AGENT_TASK_AGGREGATE_SCHEMA,
};
pub use super::agent_task_fanout::{
    AgentTaskFanoutAggregate, AgentTaskFanoutPlan, AgentTaskFanoutPlane, AgentTaskFanoutScheduler,
    AGENT_TASK_FANOUT_AGGREGATE_SCHEMA, AGENT_TASK_FANOUT_PLAN_SCHEMA,
};

mod artifacts;
mod executor;
mod matrix;
mod outcome;
mod policy;
mod request;
mod schema;

#[cfg(test)]
mod tests;

pub(crate) use matrix::expand_agent_task_matrix;
pub use matrix::{
    AgentTaskMatrixAggregate, AgentTaskMatrixAggregateCell, AgentTaskMatrixAxis,
    AgentTaskMatrixCell, AgentTaskMatrixError, AgentTaskMatrixExecutionState, AgentTaskMatrixPlan,
};

pub use artifacts::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskFollowUp, AgentTaskTypedArtifact,
};
pub use executor::{
    AgentTaskExecutor, AgentTaskRuntimeSelection, AgentTaskSourceRef, AgentTaskWorkspace,
    AgentTaskWorkspaceMode,
};
pub use outcome::{
    AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence, AgentTaskWorkflowStepStatus,
    AgentTaskWorkflowStepSuggestion,
};
pub use policy::{
    AgentTaskLimits, AgentTaskPolicy, AgentToolExecutionLocation, AgentToolPolicy,
    AgentToolPolicyRule, AgentToolRequest, AgentToolResult, AgentToolResultStatus,
};
pub use request::{
    AgentTaskComponentContract, AgentTaskExecutionHandle, AgentTaskExecutionHandleKind,
    AgentTaskExecutionState, AgentTaskExecutorCapabilities, AgentTaskPreparedWorkspace,
    AgentTaskProgress, AgentTaskProgressEvent, AgentTaskRequest, AgentTaskStart,
};
pub use schema::{
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_MATRIX_AGGREGATE_SCHEMA, AGENT_TASK_MATRIX_PLAN_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA,
    AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};
