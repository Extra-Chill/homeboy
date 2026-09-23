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

mod acceptance_verifier;
mod action_eligibility;
pub mod activity_provider;
pub mod agent_task_handoff_event;
pub mod agent_task_lifecycle_event;
mod aggregate_transition;
mod artifact_materialization;
mod cancellation;
mod control_plane_identities;
pub mod controller_pin_reference_provider;
mod conversion;
mod cook_workspace_restore;
mod durable_progress;
mod failure_recording;
mod health;
mod lab_handoff_reconciliation;
mod lab_offload;
mod lifecycle_candidate_adoption;
mod lifecycle_ops;
mod lifecycle_record_ops;
mod lifecycle_runner_projection;
mod lifecycle_transport_proxy;
mod logs_projection;
mod operation_claims;
mod private_attachment;
mod records;
pub mod runner_continuation;
mod runner_exec;
mod workspace_authority;
mod workspace_claims;

pub(crate) use acceptance_verifier::revalidate_durable_attestation;
#[cfg(any(test, feature = "test-support"))]
pub use acceptance_verifier::{clear_acceptance_verifier_for_test, AcceptanceVerifierTestGuard};
pub use acceptance_verifier::{
    register_acceptance_verifier, register_acceptance_verifier_from_config,
    AgentTaskAcceptanceAttestation, AgentTaskAcceptanceVerificationRequest,
    AgentTaskAcceptanceVerifier, AgentTaskAcceptanceVerifierProvenance,
};
pub use action_eligibility::*;
pub(crate) use aggregate_transition::*;
pub use artifact_materialization::*;
pub use cancellation::*;
pub use control_plane_identities::{
    canonical_control_plane_identities, canonical_control_plane_identities_for_run,
    canonical_fanout_mission, canonical_mission, CanonicalControlPlaneIdentities,
};
pub use durable_progress::{
    migrate_durable_event_history, migrate_durable_event_history_in_store,
    record_promotion_progress, record_promotion_progress_in_store, AgentTaskEventHistoryMigration,
    EVENT_HISTORY_MIGRATION_SCHEMA,
};
pub use failure_recording::*;
pub use health::*;
pub use homeboy_core::controller_runtime::ControllerRuntimePruneResult;
pub use lab_handoff_reconciliation::*;
pub use lab_offload::*;
pub use lifecycle_candidate_adoption::*;
pub use lifecycle_ops::*;
pub use lifecycle_record_ops::cook_attempt_run_id;
pub use lifecycle_runner_projection::*;
pub(crate) use lifecycle_store::record_from_run;
pub use lifecycle_store::AgentTaskLifecycleStore;
pub use lifecycle_transport_proxy::*;
pub use logs_projection::*;
pub use operation_claims::*;
pub use private_attachment::*;
pub use records::*;
#[cfg(any(test, feature = "test-support"))]
pub use runner_continuation::{
    clear_runner_continuation_provider_for_test, RunnerContinuationTestGuard,
};
pub use runner_continuation::{
    register_runner_continuation_provider, runner_authority, runner_live_job_authority,
    RunnerAuthority, RunnerContinuationProvider, RunnerContinuationSubmission,
    RunnerJobReconciliation, RunnerLiveJobAuthority,
};
pub use runner_exec::*;
pub use workspace_authority::*;
pub use workspace_claims::*;

pub(crate) use conversion::*;
pub(crate) use lifecycle_record_ops::*;
pub(crate) use runner_continuation::with_runner_continuation;

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn fail_next_record_write_for_test() {
    store::fail_next_record_write_for_test();
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn fail_next_cook_index_projection_write_for_test() {
    store::fail_next_cook_index_projection_write_for_test();
}

/// Number of times historical Cook index import has run on this thread since
/// the last [`reset_historical_cook_index_import_invocations_for_test`].
/// Proves a submission's admission performed no historical projection work
/// (#14962).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn historical_cook_index_import_invocations_for_test() -> u32 {
    store::historical_cook_index_import_invocations_for_test()
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn reset_historical_cook_index_import_invocations_for_test() {
    store::reset_historical_cook_index_import_invocations_for_test();
}

pub(crate) use cancellation::is_already_terminal_cancel_error;
#[cfg(test)]
pub(crate) use cancellation::{
    install_before_resolved_cancellation_for_test, install_resolved_cancel_error_for_test,
};

#[cfg(test)]
mod tests;
