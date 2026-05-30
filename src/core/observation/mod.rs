//! Local observation store.
//!
//! Boundary: JSON/files describe desired state (`homeboy.json`, rig specs,
//! stack specs, baselines). SQLite stores observed state from command runs and
//! generated artifacts. This module only provides the storage substrate.

mod budget_findings;
mod finding_adapters;
mod lifecycle;
pub mod records;
pub mod store;
pub mod timeline;

pub use lifecycle::{merge_metadata, run_owner_pid, running_status_note, ActiveObservation};

pub use budget_findings::finding_records_from_budget;
pub use finding_adapters::{
    finding_record_from_annotation, finding_record_from_audit, finding_record_from_lint,
    finding_records_from_annotation_file, finding_records_from_annotations_dir,
    finding_records_from_audit, finding_records_from_lint, AnnotationFindingRecord,
};
pub use records::{
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, FindingListFilter,
    FindingRecord, NewFindingRecord, NewRunRecord, NewRunRecordBuilder, NewTraceRunRecord,
    NewTraceRunRecordBuilder, NewTraceSpanRecord, NewTraceSpanRecordBuilder, NewTriageItemRecord,
    RunListFilter, RunRecord, RunStatus, TraceRunRecord, TraceSpanRecord, TriageItemRecord,
    TriagePullRequestSignals,
};
pub use store::{
    ObservationDbStatus, ObservationStore, CURRENT_SCHEMA_VERSION, LAB_OFFLOAD_METADATA_ENV,
};
pub use timeline::{
    ObservationEvent, ObservationPhaseMilestone, ObservationSpanDefinition, ObservationSpanResult,
    ObservationSpanStatus,
};
