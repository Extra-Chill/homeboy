//! snapshots_dir — extracted from snapshot.rs.

use std::path::{Path, PathBuf};
use crate::Result;
use serde::{Deserialize, Serialize};
use super::RestoreResult;
use super::SnapshotSummary;
use super::SnapshotManifest;


/// Restore the most recent snapshot, or a specific one by ID.
pub fn restore(snapshot_id: Option<&str>) -> Result<RestoreResult> {
    restore_from_dir(snapshot_id, &snapshots_dir())
}

/// List all available snapshots, newest first.
pub fn list_snapshots() -> Result<Vec<SnapshotSummary>> {
    list_snapshots_in(&snapshots_dir())
}

/// List snapshots from a specific directory. Used internally and by tests.
pub(crate) fn list_snapshots_in(base_dir: &Path) -> Result<Vec<SnapshotSummary>> {
    if !base_dir.exists() {
        return Ok(vec![]);
    }

    let mut summaries = Vec::new();
    let now = now_unix();

    let mut entries: Vec<_> = std::fs::read_dir(base_dir)
        .map_err(|e| {
            crate::Error::internal_unexpected(format!("Failed to read snapshots dir: {}", e))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    entries.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    for entry in entries {
        let manifest_path = entry.path().join("manifest.json");
        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
            if let Ok(manifest) = serde_json::from_str::<SnapshotManifest>(&content) {
                summaries.push(SnapshotSummary {
                    id: manifest.id,
                    label: manifest.label,
                    root: manifest.root,
                    file_count: manifest.entries.len(),
                    age: format_age(now.saturating_sub(manifest.created_at)),
                    created_at: manifest.created_at,
                });
            }
        }
    }

    Ok(summaries)
}

/// Delete a specific snapshot without restoring.
pub fn delete_snapshot(snapshot_id: &str) -> Result<()> {
    delete_snapshot_in(snapshot_id, &snapshots_dir())
}

/// Delete a snapshot from a specific directory. Used internally and by tests.
pub(crate) fn delete_snapshot_in(snapshot_id: &str, base_dir: &Path) -> Result<()> {
    let snapshot_dir = base_dir.join(snapshot_id);
    if !snapshot_dir.exists() {
        return Err(crate::Error::validation_invalid_argument(
            "snapshot_id",
            format!("Snapshot '{}' not found", snapshot_id),
            None,
            None,
        ));
    }

    std::fs::remove_dir_all(&snapshot_dir).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to delete snapshot: {}", e))
    })?;

    log_status!("undo", "Deleted snapshot {}", snapshot_id);
    Ok(())
}

pub(crate) fn snapshots_dir() -> PathBuf {
    // Allow override via env var (used for custom snapshot storage locations)
    if let Ok(dir) = std::env::var("HOMEBOY_SNAPSHOTS_DIR") {
        return PathBuf::from(dir);
    }
    // Use $HOME/.cache/homeboy/snapshots (XDG default on Linux/macOS)
    // Falls back to /tmp if $HOME is not set (unlikely in practice)
    let cache_base = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".cache"))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    cache_base.join("homeboy").join("snapshots")
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s ago", seconds)
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}
