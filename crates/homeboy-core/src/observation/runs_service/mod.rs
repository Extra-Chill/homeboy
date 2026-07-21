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
use crate::Error;
use crate::Result;
use homeboy_lab_runner_contract::RunnerArtifactRef;

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
    pub artifact_cleanup: Vec<PersistedArtifactCleanupOutcome>,
    pub lifecycle_directories: Vec<TerminalRunLifecycleDirectory>,
    pub skipped_run_ids: Vec<String>,
    pub removed_run_count: usize,
}

/// A controller-owned lifecycle directory that is retained or removed with its
/// terminal observation row. Persisted artifact bytes remain owned by the
/// existing persisted-artifact cleanup path.
#[derive(Debug, Clone, Serialize)]
pub struct TerminalRunLifecycleDirectory {
    pub run_id: String,
    pub path: PathBuf,
    pub exists: bool,
    pub size_bytes: u64,
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
pub mod runner_evidence;

pub use artifact_links::*;
pub use artifact_resolve::*;
pub use persisted_cleanup::*;
pub use run_lookup::*;
pub use runner_downloads::*;
pub use runner_evidence::with_runner_evidence;
pub use runner_evidence::{
    mirrored_runner_job_identities, register_runner_evidence_provider, RemoteArtifactDownloadInfo,
    RunnerConnectionInfo, RunnerEvidenceProvider, StaleRunnerJobInfo,
};

/// Safe, bounded retention of terminal observation rows, registered local
/// artifact bytes, and controller-owned lifecycle directories. Artifact path
/// validation remains owned by the persisted-artifact cleanup implementation.
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
    let mut artifact_cleanup = Vec::new();
    let mut lifecycle_directories = Vec::new();
    let mut removable_run_ids = Vec::new();
    let mut skipped_run_ids = Vec::new();
    for run_id in &candidate_run_ids {
        let artifacts = cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
            apply: false,
            older_than_days: 0,
            run_id: Some(run_id.clone()),
            kind: None,
            artifact_type: None,
            run_kind: None,
            component_id: None,
            limit: 10_000,
            terminal_only: true,
        })?;
        let lifecycle_directory = terminal_run_lifecycle_directory(&store, run_id)?;
        let blocked = artifacts
            .rows
            .iter()
            .any(|row| row.action == "skip" && row.exists);
        artifact_cleanup.push(artifacts);
        if blocked {
            skipped_run_ids.push(run_id.clone());
        } else {
            removable_run_ids.push(run_id.clone());
            if let Some(directory) = lifecycle_directory {
                lifecycle_directories.push(directory);
            }
        }
    }
    if options.apply {
        // Revalidate and remove artifact bytes before deleting any provenance.
        // A blocked resource leaves the terminal run record and lifecycle root intact.
        for run_id in &removable_run_ids {
            let artifacts = cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
                apply: false,
                older_than_days: 0,
                run_id: Some(run_id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 10_000,
                terminal_only: true,
            })?;
            if artifacts
                .rows
                .iter()
                .any(|row| row.action == "skip" && row.exists)
            {
                skipped_run_ids.push(run_id.clone());
                continue;
            }
            let records = store.list_artifacts(run_id)?;
            let run = store.get_run(run_id)?;
            let artifact_root = crate::artifacts::root()?;
            for row in artifacts.rows.iter().filter(|row| row.action == "remove") {
                let artifact = records
                    .iter()
                    .find(|artifact| artifact.id == row.artifact_id)
                    .ok_or_else(|| {
                        Error::internal_unexpected("retention artifact disappeared before deletion")
                    })?;
                remove_persisted_artifact_record_bytes(artifact, &artifact_root)?;
                if let Some(run) = run.as_ref() {
                    runner_evidence::with_runner_evidence(|provider| {
                        provider.retire_durable_result_owner(run, Some(&artifact.id))
                    })?;
                }
            }
        }
        for directory in &lifecycle_directories {
            if skipped_run_ids.contains(&directory.run_id) {
                continue;
            }
            if directory.exists {
                fs::remove_dir_all(&directory.path).map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(format!(
                            "remove terminal run lifecycle directory {}",
                            directory.path.display()
                        )),
                    )
                })?;
            }
        }
        let retained = skipped_run_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        let deleted = removable_run_ids
            .iter()
            .filter(|run_id| !retained.contains(run_id))
            .cloned()
            .collect::<Vec<_>>();
        for run_id in &deleted {
            if let Some(run) = store.get_run(run_id)? {
                runner_evidence::with_runner_evidence(|provider| {
                    provider.retire_durable_result_owner(&run, None)
                })?;
            }
        }
        store.delete_terminal_runs(&deleted)?;
    }
    Ok(TerminalRunRetentionOutcome {
        dry_run: !options.apply,
        older_than_days: options.older_than_days,
        removed_run_count: if options.apply {
            removable_run_ids
                .len()
                .saturating_sub(skipped_run_ids.len())
        } else {
            0
        },
        candidate_run_ids,
        artifact_cleanup,
        lifecycle_directories,
        skipped_run_ids,
    })
}

fn terminal_run_lifecycle_directory(
    store: &ObservationStore,
    run_id: &str,
) -> Result<Option<TerminalRunLifecycleDirectory>> {
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    if run.kind != "agent-task" {
        return Ok(None);
    }

    let root = crate::paths::homeboy_data()?.join("agent-task-runs");
    let path = root.join(crate::paths::sanitize_path_segment(run_id));
    let exists = path.exists();
    let size_bytes = if exists { path_size_bytes(&path)? } else { 0 };
    Ok(Some(TerminalRunLifecycleDirectory {
        run_id: run_id.to_string(),
        path,
        exists,
        size_bytes,
    }))
}

fn path_size_bytes(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    fs::read_dir(path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?
        .try_fold(metadata.len(), |total, entry| {
            Ok(total
                + path_size_bytes(
                    &entry
                        .map_err(|error| Error::internal_io(error.to_string(), None))?
                        .path(),
                )?)
        })
}

#[cfg(test)]
mod tests;
