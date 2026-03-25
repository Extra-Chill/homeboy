//! types — extracted from snapshot.rs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use crate::Result;


/// Maximum number of snapshots to keep. Oldest are expired on save.
pub(crate) const MAX_SNAPSHOTS: usize = 20;

/// A file entry in a snapshot — either an existing file with original content,
/// or a new file that didn't exist before (original = None).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Relative path from the project root.
    pub relative_path: String,
    /// Original content before modification, or None if the file was newly created.
    pub had_content: bool,
}

/// Snapshot manifest stored alongside file backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Unique snapshot ID (timestamp-based).
    pub id: String,
    /// Human-readable label describing the operation (e.g., "audit fix", "refactor rename").
    pub label: String,
    /// Absolute path to the project root where files are relative to.
    pub root: String,
    /// File entries in this snapshot.
    pub entries: Vec<SnapshotEntry>,
    /// When the snapshot was created (Unix timestamp).
    pub created_at: u64,
}

/// Result of restoring a snapshot.
#[derive(Debug, Serialize)]
pub struct RestoreResult {
    pub snapshot_id: String,
    pub label: String,
    pub files_restored: usize,
    pub files_removed: usize,
    pub errors: Vec<String>,
}

/// Summary of a snapshot for listing.
#[derive(Debug, Serialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub label: String,
    pub root: String,
    pub file_count: usize,
    pub created_at: u64,
    /// Human-readable age string (e.g., "2 minutes ago").
    pub age: String,
}
