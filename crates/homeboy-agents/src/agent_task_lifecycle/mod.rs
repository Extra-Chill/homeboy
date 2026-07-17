use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskComponentContract, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskExecutionHandle, AgentTaskExecutionHandleKind, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkflowEvidence,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::agent_task_provider::{role_aliases_for_provider, AgentTaskProviderRoleAliases};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
    AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
};
use homeboy_core::run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionState,
    RunHeartbeat, RunLifecycleRecord, RUN_LIFECYCLE_RECORD_SCHEMA,
};
use homeboy_core::{paths, Error, ErrorCode, Result};

#[path = "../lifecycle_store.rs"]
mod lifecycle_store;

use lifecycle_store as store;

pub mod activity_provider;
pub mod agent_task_lifecycle_event;
mod artifact_materialization;
mod cancellation;
pub mod controller_pin_reference_provider;
mod conversion;
mod failure_recording;
mod health;
mod lifecycle_ops;
mod lifecycle_record_ops;
mod records;
pub mod runner_continuation;

pub use homeboy_core::controller_runtime::ControllerRuntimePruneResult;
pub use artifact_materialization::*;
pub use cancellation::*;
pub use failure_recording::*;
pub use health::*;
pub use lifecycle_ops::*;
pub use lifecycle_record_ops::cook_attempt_run_id;
pub use records::*;
pub use runner_continuation::{register_runner_continuation_provider, RunnerContinuationProvider};

pub(crate) use conversion::*;
pub(crate) use lifecycle_record_ops::*;

#[cfg(test)]
mod tests;
