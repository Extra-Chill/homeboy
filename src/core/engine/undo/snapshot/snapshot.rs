//! snapshot — extracted from snapshot.rs.

use std::path::{Path, PathBuf};
use crate::Result;
use serde::{Deserialize, Serialize};


pub(crate) fn generate_snapshot_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_millis())
}

pub(crate) fn latest_snapshot_id_in(base_dir: &Path) -> Result<String> {
    if !base_dir.exists() {
        return Err(crate::Error::internal_unexpected(
            "No undo snapshots available. Run a --write operation first.",
        ));
    }

    let mut entries: Vec<_> = std::fs::read_dir(base_dir)
        .map_err(|e| {
            crate::Error::internal_unexpected(format!("Failed to read snapshots dir: {}", e))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    entries.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    entries
        .first()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .ok_or_else(|| {
            crate::Error::internal_unexpected(
                "No undo snapshots available. Run a --write operation first.",
            )
        })
}
