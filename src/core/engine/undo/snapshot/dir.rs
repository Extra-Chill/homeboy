//! dir — extracted from snapshot.rs.

use std::path::{Path, PathBuf};
use crate::Result;
use serde::{Deserialize, Serialize};
use super::MAX_SNAPSHOTS;
use super::remove_empty_parents;
use super::expire_old_snapshots_in;
use super::latest_snapshot_id_in;
use super::RestoreResult;
use super::UndoSnapshot;
use super::now_unix;
use super::generate_snapshot_id;
use super::load_manifest;
use super::SnapshotManifest;
use super::sanitize_path;


/// Save a snapshot to a specific directory. Used internally and by tests.
pub(crate) fn save_to_dir(snap: UndoSnapshot, base_dir: &Path) -> Result<String> {
    if snap.entries.is_empty() {
        return Err(crate::Error::validation_invalid_argument(
            "undo",
            "No files to snapshot",
            None,
            None,
        ));
    }

    let id = generate_snapshot_id();
    let snapshot_dir = base_dir.join(&id);
    let files_dir = snapshot_dir.join("files");
    std::fs::create_dir_all(&files_dir).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to create snapshot dir: {}", e))
    })?;

    // Write file contents
    for (relative_path, content) in &snap.contents {
        let safe_name = sanitize_path(relative_path);
        let dest = files_dir.join(&safe_name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&dest, content).map_err(|e| {
            crate::Error::internal_unexpected(format!(
                "Failed to write snapshot file {}: {}",
                relative_path, e
            ))
        })?;
    }

    // Write manifest
    let manifest = SnapshotManifest {
        id: id.clone(),
        label: snap.label,
        root: snap.root.to_string_lossy().to_string(),
        entries: snap.entries,
        created_at: now_unix(),
    };

    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to serialize manifest: {}", e))
    })?;

    std::fs::write(snapshot_dir.join("manifest.json"), manifest_json).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to write manifest: {}", e))
    })?;

    log_status!(
        "undo",
        "Snapshot saved: {} ({} file(s))",
        id,
        manifest.entries.len()
    );

    // Expire old snapshots
    expire_old_snapshots_in(base_dir, MAX_SNAPSHOTS);

    Ok(id)
}

/// Restore from a specific snapshot directory. Used internally and by tests.
pub(crate) fn restore_from_dir(snapshot_id: Option<&str>, base_dir: &Path) -> Result<RestoreResult> {
    let id = match snapshot_id {
        Some(id) => id.to_string(),
        None => latest_snapshot_id_in(base_dir)?,
    };

    let snapshot_dir = base_dir.join(&id);
    let manifest = load_manifest(&snapshot_dir)?;
    let files_dir = snapshot_dir.join("files");
    let root = Path::new(&manifest.root);

    let mut files_restored = 0;
    let mut files_removed = 0;
    let mut errors = Vec::new();

    for entry in &manifest.entries {
        let target = root.join(&entry.relative_path);

        if entry.had_content {
            // Restore original content
            let safe_name = sanitize_path(&entry.relative_path);
            let source = files_dir.join(&safe_name);
            match std::fs::read(&source) {
                Ok(content) => {
                    if let Err(e) = std::fs::write(&target, content) {
                        errors.push(format!("Failed to restore {}: {}", entry.relative_path, e));
                    } else {
                        files_restored += 1;
                    }
                }
                Err(e) => {
                    errors.push(format!("Missing backup for {}: {}", entry.relative_path, e));
                }
            }
        } else {
            // File was created by the operation — remove it
            if target.exists() {
                if let Err(e) = std::fs::remove_file(&target) {
                    errors.push(format!("Failed to remove {}: {}", entry.relative_path, e));
                } else {
                    files_removed += 1;
                }
            }
            // If the parent directory is now empty, remove it too
            if let Some(parent) = target.parent() {
                remove_empty_parents(parent, root);
            }
        }
    }

    log_status!(
        "undo",
        "Restored {} file(s), removed {} created file(s)",
        files_restored,
        files_removed
    );

    // Remove the snapshot after successful restore
    if errors.is_empty() {
        let _ = std::fs::remove_dir_all(&snapshot_dir);
        log_status!("undo", "Snapshot {} consumed", id);
    }

    Ok(RestoreResult {
        snapshot_id: id,
        label: manifest.label,
        files_restored,
        files_removed,
        errors,
    })
}
