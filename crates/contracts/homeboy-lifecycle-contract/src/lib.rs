//! Pure serializable artifact / evidence / lifecycle contract types.
//!
//! These behavior-free data structures describe the shape of artifact and
//! lifecycle contracts shared across homeboy. They depend only on serde, which
//! keeps this a leaf crate other crates can depend on without pulling in core.
//!
//! Conversions that couple these types to core's observation records or
//! `ArtifactRef` (`from_record`, `to_artifact_ref`, `From<ArtifactRef>`) live
//! in `homeboy-core` as free functions, so this crate stays observation-free.
//!
//! Two distinct lifecycle vocabularies live here and do not overlap. `lifecycle`
//! is the workload *phase* lifecycle (`LifecyclePhaseKind`).
//! `run_lifecycle_record` is a run *state machine* (`RunExecutionState`,
//! `CleanupState`, `FinalizationState`, `ArtifactRetentionStatus`). The latter
//! was its own `homeboy-run-lifecycle-contract` crate until it was merged here;
//! the two share no types, and `homeboy-core` already imported that crate under
//! the module name `run_lifecycle_record`, which is the name it keeps.

pub mod artifact_contract;
pub mod lifecycle;
pub mod rig_snapshot;
pub mod run_lifecycle_record;
pub mod timeline;

pub use artifact_contract::{
    ArtifactContract, ArtifactRecord, ArtifactViewerLink, ARTIFACT_CONTRACT_SCHEMA,
};
pub use lifecycle::{
    LifecycleContract, LifecyclePhaseContract, LifecyclePhaseKind, LifecyclePhaseResult,
    LifecyclePhaseStatus, LifecycleResultMetadata, LifecycleSnapshotRef,
};
pub use rig_snapshot::{ComponentSnapshot, RigStateSnapshot};
pub use run_lifecycle_record::{
    ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
    ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
    ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState, RunHeartbeat,
    RunLifecycleRecord, RUN_LIFECYCLE_RECORD_SCHEMA,
};
pub use timeline::{
    event_matches_key, merge_span_definitions, parse_phase_milestone, parse_span_definition,
    phase_span_definitions, reporting_timeline, summarize_spans, ObservationEvent,
    ObservationPhaseMilestone, ObservationSpanDefinition, ObservationSpanResult,
    ObservationSpanStatus,
};
