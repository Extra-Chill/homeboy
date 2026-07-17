//! Pure serializable artifact / evidence / lifecycle contract types.
//!
//! These behavior-free data structures describe the shape of artifact and
//! lifecycle contracts shared across homeboy. They depend only on serde, which
//! keeps this a leaf crate other crates can depend on without pulling in core.
//!
//! Conversions that couple these types to core's observation records or
//! `ArtifactRef` (`from_record`, `to_artifact_ref`, `From<ArtifactRef>`) live
//! in `homeboy-core` as free functions, so this crate stays observation-free.

pub mod artifact_contract;
pub mod lifecycle;
pub mod timeline;

pub use artifact_contract::{
    ArtifactContract, ArtifactRecord, ArtifactViewerLink, EvidenceContract,
    ARTIFACT_CONTRACT_SCHEMA, EVIDENCE_CONTRACT_SCHEMA,
};
pub use lifecycle::{
    LifecycleContract, LifecyclePhaseContract, LifecyclePhaseKind, LifecyclePhaseResult,
    LifecyclePhaseStatus, LifecycleResultMetadata, LifecycleSnapshotRef,
};
pub use timeline::{
    event_matches_key, merge_span_definitions, parse_phase_milestone, parse_span_definition,
    phase_span_definitions, reporting_timeline, summarize_spans, ObservationEvent,
    ObservationPhaseMilestone, ObservationSpanDefinition, ObservationSpanResult,
    ObservationSpanStatus,
};
