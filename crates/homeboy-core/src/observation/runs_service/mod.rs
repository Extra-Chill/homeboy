//! Run/artifact observation service.
//!
//! Reusable lookup, enrichment, artifact retrieval, and mirrored-daemon
//! evidence refresh primitives extracted from `src/commands/runs.rs`. The
//! goals here are:
//!
//! * Keep CLI argument parsing and output enum serialization in the
//!   `commands::runs` adapter where it belongs.
//! * Expose run/artifact query and mutation primitives that other
//!   consumers (HTTP API, MCP, future automation) can reuse without
//!   going through the CLI output enum.
//!
//! Behavior here mirrors the previous `commands::runs` helpers byte-for-byte,
//! including the order of side effects (reconcile → refresh evidence → index
//! nested publication artifacts → list artifacts → enrich links). The
//! `commands::runs` callers are thin wrappers that map the returned data
//! into `RunsOutput` variants.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use super::{
    ArtifactCleanupCandidateRecord, ArtifactCleanupFilter, ArtifactRecord, ObservationStore,
    RunListFilter, RunRecord, RunStatus,
};
use crate::artifact_links::{cached_validated_viewer_links, public_artifact_url};
use crate::engine::temp::CleanupSizeTotals;
use crate::execution_contract::EXECUTION_CONTRACT;
use crate::runners::RunnerArtifactRef;
use crate::Error;
use crate::Result;

/// Output of a successful artifact byte retrieval (whether the bytes came
/// from a locally-recorded file or from a remote runner).
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactFetchOutcome {
    pub run_id: String,
    pub artifact_id: String,
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<RunnerArtifactRef>,
}

/// Outcome of `get_artifact_bytes` describing where the bytes were written.
pub enum ArtifactGetSource {
    /// Bytes copied from a locally-recorded file artifact.
    Local,
    /// Bytes fetched from a remote runner cache.
    Remote,
}

#[derive(Debug, Clone, Default)]
pub struct RunnerDownloadCleanupOptions {
    pub apply: bool,
    pub runner: Option<String>,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDownloadCleanupOutcome {
    pub dry_run: bool,
    pub root: PathBuf,
    pub removed: bool,
    pub file_count: usize,
    pub directory_count: usize,
    pub size_bytes: u64,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PersistedArtifactCleanupOptions {
    pub apply: bool,
    pub older_than_days: i64,
    pub run_id: Option<String>,
    pub kind: Option<String>,
    pub artifact_type: Option<String>,
    pub run_kind: Option<String>,
    pub component_id: Option<String>,
    pub limit: i64,
    /// Only terminal, known run states may release persisted evidence.
    pub terminal_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistedArtifactCleanupOutcome {
    pub dry_run: bool,
    pub artifact_root: PathBuf,
    pub older_than_days: i64,
    #[serde(flatten)]
    pub totals: CleanupSizeTotals,
    pub planned_record_count: usize,
    pub planned_file_count: usize,
    pub planned_directory_count: usize,
    pub removed_record_count: usize,
    pub removed_file_count: usize,
    pub removed_directory_count: usize,
    pub skipped_count: usize,
    pub rows: Vec<PersistedArtifactCleanupRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistedArtifactCleanupRow {
    pub artifact_id: String,
    pub run_id: String,
    pub run_kind: String,
    pub run_status: String,
    pub component_id: Option<String>,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub created_at: String,
    pub exists: bool,
    pub action: String,
    pub reason: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct TerminalRunRetentionOptions {
    pub apply: bool,
    pub older_than_days: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalRunRetentionOutcome {
    pub dry_run: bool,
    pub older_than_days: i64,
    pub candidate_run_ids: Vec<String>,
    pub removed_run_count: usize,
}

#[derive(Debug, Default)]
struct RunnerDownloadCleanupPreview {
    file_count: usize,
    directory_count: usize,
    size_bytes: u64,
    paths: Vec<String>,
}

/// Storage classes recognized by [`classify_artifact_storage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStorage {
    LocalFile,
    Remote,
    MetadataOnly,
    Other,
}

mod artifact_links;
mod artifact_resolve;
mod persisted_cleanup;
mod run_lookup;
mod runner_downloads;

pub use artifact_links::*;
pub use artifact_resolve::*;
pub use persisted_cleanup::*;
pub use run_lookup::*;
pub use runner_downloads::*;

/// Safe, bounded retention of terminal observation rows. Artifact byte cleanup
/// remains a separate explicit operation because artifact roots may have their
/// own evidence-retention policy.
pub fn retain_terminal_runs(
    options: TerminalRunRetentionOptions,
) -> Result<TerminalRunRetentionOutcome> {
    if options.older_than_days < 0 {
        return Err(Error::validation_invalid_argument(
            "older_than_days",
            "--older-than-days must be zero or greater",
            Some(options.older_than_days.to_string()),
            None,
        ));
    }
    let finished_before = (Utc::now() - Duration::days(options.older_than_days)).to_rfc3339();
    let mut store = ObservationStore::open_initialized()?;
    let candidate_run_ids = store.terminal_run_ids_before(&finished_before, options.limit)?;
    if options.apply {
        store.delete_terminal_runs(&candidate_run_ids)?;
    }
    Ok(TerminalRunRetentionOutcome {
        dry_run: !options.apply,
        older_than_days: options.older_than_days,
        removed_run_count: usize::from(options.apply) * candidate_run_ids.len(),
        candidate_run_ids,
    })
}

#[cfg(test)]
mod tests;
