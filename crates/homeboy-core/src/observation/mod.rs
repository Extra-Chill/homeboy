//! Local observation store.
//!
//! Boundary: JSON/files describe desired state (`homeboy.json`, rig specs,
//! stack specs, baselines). SQLite stores observed state from command runs and
//! generated artifacts. This module only provides the storage substrate.

pub mod artifact_preview;
pub mod audit_artifact_provider;
mod budget_findings;
pub mod bundle;
pub mod context;
pub mod disk_budget;
pub mod evidence_report;
mod lifecycle;
pub mod loop_inventory_run;
pub mod observed_workflow;
pub mod records;
mod run_failure_causes;

pub mod runs_service;
pub mod store;
mod test_findings;
pub use homeboy_lifecycle_contract::timeline;

pub use lifecycle::{
    finish_run_best_effort, merge_metadata, run_has_active_remote_job, run_owner_pid,
    running_status_note, ActiveObservation, ACTIVE_RUN_ID_ENV,
};

pub use bundle::{
    build_bundle, bundle_artifact_uri, extract_directory_artifact_archive, portable_artifact_label,
    read_bundle_dir, write_bundle_dir, ObservationBundle, ObservationBundleArtifactBytes,
    ObservationBundleManifest, BUNDLE_FORMAT, BUNDLE_VERSION,
};
pub use loop_inventory_run::persist_loop_inventory_run;

pub use crate::notification_route::NotificationRoute;
pub use budget_findings::finding_records_from_budget;
pub use context::{
    env_json, resolve_json_value, RunContext, RunProvenance, PROVENANCE_REFERENCE_SCHEMA,
};
pub use homeboy_lifecycle_contract::timeline::{
    ObservationEvent, ObservationPhaseMilestone, ObservationSpanDefinition, ObservationSpanResult,
    ObservationSpanStatus,
};
pub use observed_workflow::{
    finish_adapted_observed_workflow, finish_observed_workflow, ObservationPersistenceWarning,
    ObservedWorkflowRunner, WorkflowObservationAdapter,
};
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
pub use run_failure_causes::{nested_failure_causes_from_run_detail, RunFailureCause};
pub use store::{
    ObservationDbStatus, ObservationStore, CURRENT_SCHEMA_VERSION, LAB_OFFLOAD_METADATA_ENV,
    PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV, SOURCE_SNAPSHOT_METADATA_ENV,
};
pub use test_findings::{
    finding_records_from_failure_clusters, finding_records_from_test_analysis_input,
    homeboy_findings_from_test_analysis_input,
};
