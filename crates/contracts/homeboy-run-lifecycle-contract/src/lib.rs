//! Pure serializable run-lifecycle record contract types.
//!
//! `RunLifecycleRecord` and its component lifecycle structs/enums describe the
//! typed runtime state of a run (execution, provider runtime, heartbeat,
//! cleanup, finalization, artifact retention) as it crosses process boundaries
//! between the runner, Lab, daemon, and agent-task surfaces. These are
//! behavior-free data structures depending only on serde, which keeps this a
//! leaf crate other crates can depend on without pulling in core.

pub mod run_lifecycle_record;

pub use run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
    ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState, RunHeartbeat,
    RunLifecycleRecord, RUN_LIFECYCLE_RECORD_SCHEMA,
};
