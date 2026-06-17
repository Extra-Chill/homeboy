//! Local observation store.
//!
//! Boundary: JSON/files describe desired state (`homeboy.json`, rig specs,
//! stack specs, baselines). SQLite stores observed state from command runs and
//! generated artifacts. This module only provides the storage substrate.

mod budget_findings;
pub mod context;
mod lifecycle;
pub mod records;
pub mod runs_service;
pub mod store;
mod test_findings;
pub mod timeline;

pub use lifecycle::{
    merge_metadata, run_owner_pid, running_status_note, ActiveObservation, ACTIVE_RUN_ID_ENV,
};

pub use budget_findings::finding_records_from_budget;
pub use context::{RunContext, RunProvenance};
pub use records::{
    finding_record_from_audit, finding_record_from_lint, finding_records_from_annotation_file,
    finding_records_from_annotations_dir, finding_records_from_audit,
    finding_records_from_homeboy_findings, finding_records_from_lint, homeboy_finding_from_audit,
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, ArtifactViewerLink,
    FindingListFilter, FindingRecord, NewFindingRecord, NewRunRecord, NewRunRecordBuilder,
    NewTraceRunRecord, NewTraceRunRecordBuilder, NewTraceSpanRecord, NewTraceSpanRecordBuilder,
    NewTriageItemRecord, RecordedHomeboyFinding, RunEvidenceCommands, RunListFilter, RunRecord,
    RunStatus, TraceRunRecord, TraceSpanRecord, TriageItemRecord, TriagePullRequestSignals,
};
pub use store::{
    ObservationDbStatus, ObservationStore, CURRENT_SCHEMA_VERSION, LAB_OFFLOAD_METADATA_ENV,
    PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV, SOURCE_SNAPSHOT_METADATA_ENV,
};
pub(crate) use test_findings::{
    finding_records_from_failure_clusters, finding_records_from_test_analysis_input,
    homeboy_findings_from_test_analysis_input,
};
pub use timeline::{
    ObservationEvent, ObservationPhaseMilestone, ObservationSpanDefinition, ObservationSpanResult,
    ObservationSpanStatus,
};
