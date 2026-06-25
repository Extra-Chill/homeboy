use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskComponentContract, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskExecutionHandle, AgentTaskExecutionHandleKind, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkflowEvidence,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_provider::{role_aliases_for_provider, AgentTaskProviderRoleAliases};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
    AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
};
use crate::core::run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionState,
    RunHeartbeat, RunLifecycleRecord, RUN_LIFECYCLE_RECORD_SCHEMA,
};
use crate::core::{paths, Error, ErrorCode, Result};

#[path = "../lifecycle_store.rs"]
mod lifecycle_store;

use lifecycle_store as store;

mod cancellation;
mod conversion;
mod failure_recording;
mod lifecycle_ops;
mod lifecycle_record_ops;
mod records;

pub use cancellation::*;
pub use failure_recording::*;
pub use lifecycle_ops::*;
pub use records::*;

pub(crate) use conversion::*;
pub(crate) use lifecycle_record_ops::*;

#[cfg(test)]
mod tests;
