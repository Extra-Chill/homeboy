use std::path::PathBuf;

use rusqlite::Connection;
use serde::Serialize;

mod artifacts;
mod findings;
mod helpers;
mod runs;
mod schema;
mod triage_items;

use super::context::RunContext;
pub use super::context::{
    LAB_OFFLOAD_METADATA_ENV, PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV,
    SOURCE_SNAPSHOT_METADATA_ENV,
};
use super::records::{
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, FindingListFilter,
    FindingRecord, NewFindingRecord, NewRunRecord, NewTraceRunRecord, NewTraceSpanRecord,
    NewTriageItemRecord, RunListFilter, RunRecord, RunStatus, TraceRunRecord, TraceSpanRecord,
    TriageItemRecord, TriagePullRequestSignals,
};
use crate::core::{paths, Error, Result};

pub(crate) use helpers::*;

pub const CURRENT_SCHEMA_VERSION: i64 = 6;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObservationDbStatus {
    pub path: String,
    pub exists: bool,
    pub schema_version: i64,
    pub migration_count: i64,
    pub table_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArtifactRecord {
    pub run: RunRecord,
    pub artifact: ArtifactRecord,
}

pub struct ObservationStore {
    connection: Connection,
    path: PathBuf,
}

pub fn database_path() -> Result<PathBuf> {
    schema::database_path()
}

/// Read local observation-store status without creating the database.
pub fn status() -> Result<ObservationDbStatus> {
    schema::status()
}

#[cfg(test)]
#[path = "../../../../tests/core/observation/store_test.rs"]
mod store_test;
