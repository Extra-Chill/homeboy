use std::path::PathBuf;

use rusqlite::Connection;
use serde::Serialize;

use crate::paths::PathRoots;

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
    NewTriageItemRecord, RunCursor, RunListFilter, RunPage, RunRecord, RunStatus, TraceRunRecord,
    TraceSpanRecord, TriageItemRecord, TriagePullRequestSignals,
};
use crate::{Error, Result};
pub use artifacts::directory_tree_sha256;
pub use artifacts::{
    ArtifactListFilter, ArtifactListPage, ArtifactPublication, ArtifactPublicationType,
    BoundedArtifactProjection,
};
pub use runs::{DEFAULT_RUN_PAGE_LIMIT, MAX_EXHAUSTIVE_RUN_ROWS, MAX_RUN_PAGE_LIMIT};

pub(crate) use helpers::*;

/// The schema version a fully migrated store reports.
///
/// Always the last declared migration. Writing it down separately is what let
/// it fall behind the migration list.
pub const CURRENT_SCHEMA_VERSION: i64 = schema::LATEST_MIGRATION_VERSION;

/// Migrations a fully initialized store has applied.
///
/// Exposed so callers assert against the declared schema rather than a copied
/// number that goes stale the next time a migration is added.
pub const CURRENT_MIGRATION_COUNT: i64 = schema::MIGRATION_COUNT;

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
    readonly: bool,
    /// Filesystem roots this store was opened against, when it was opened
    /// through a rooted constructor.
    ///
    /// The store spans TWO roots — `data` locates the SQLite database and
    /// `artifacts` locates the bytes the database indexes — so they are held
    /// together rather than as two independently resolvable fields. Resolving
    /// one from injection and the other from ambient process state is the
    /// "split home" defect class this field exists to make impossible (#7505).
    ///
    /// `None` means the store was opened at the ambient boundary; artifact
    /// resolution then falls back to `paths::artifact_root()`.
    roots: Option<PathRoots>,
}

pub fn database_path() -> Result<PathBuf> {
    schema::database_path()
}

/// Read local observation-store status without creating the database.
pub fn status() -> Result<ObservationDbStatus> {
    schema::status()
}

#[cfg(test)]
#[path = "../../../../../tests/core/observation/store_test.rs"]
mod store_test;
