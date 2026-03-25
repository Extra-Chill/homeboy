//! helpers — extracted from snapshot.rs.

use std::path::{Path, PathBuf};
use crate::Result;
use serde::{Deserialize, Serialize};
use super::SnapshotManifest;


pub(crate) fn load_manifest(snapshot_dir: &Path) -> Result<SnapshotManifest> {
    let manifest_path = snapshot_dir.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to read manifest: {}", e))
    })?;
    serde_json::from_str(&content)
        .map_err(|e| crate::Error::internal_unexpected(format!("Failed to parse manifest: {}", e)))
}

/// Convert a path like "src/core/fixer.rs" to a safe filename for snapshot storage.
/// Replaces `/` with `__` to flatten the directory structure.
pub(crate) fn sanitize_path(relative_path: &str) -> String {
    relative_path.replace('/', "__")
}

pub(crate) fn expire_old_snapshots_in(base_dir: &Path, keep: usize) {
    if !base_dir.exists() {
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(base_dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };

    if entries.len() <= keep {
        return;
    }

    // Sort newest first (snapshot IDs are timestamps)
    entries.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    // Remove everything beyond the keep count
    for entry in entries.into_iter().skip(keep) {
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// Remove empty parent directories up to (but not including) the root.
pub(crate) fn remove_empty_parents(dir: &Path, root: &Path) {
    let mut current = dir;
    while current != root {
        if current.is_dir() {
            match std::fs::read_dir(current) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        let _ = std::fs::remove_dir(current);
                    } else {
                        break; // Not empty, stop
                    }
                }
                Err(_) => break,
            }
        } else {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
}
