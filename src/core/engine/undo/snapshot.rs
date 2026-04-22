//! Persistent undo snapshots for write operations.
//!
//! Before any `--write` operation (audit fix, refactor rename/move/transform/decompose),
//! callers snapshot all files that will be modified or created. After the operation,
//! `homeboy undo` restores the snapshot — even if the working tree had uncommitted changes.
//!
//! Snapshots are stored at `~/.cache/homeboy/snapshots/<id>/` with a manifest.json
//! and copies of original file contents. Created files are recorded with `original: null`
//! so undo can remove them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::entry::FileStateEntry;
use crate::Result;

/// Maximum number of snapshots to keep. Oldest are expired on save.
const MAX_SNAPSHOTS: usize = 20;

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

/// A builder for creating snapshots before write operations.
///
/// Usage:
/// ```ignore
/// let mut snap = UndoSnapshot::new(root, "audit fix");
/// snap.capture_file("src/core/fixer.rs");
/// snap.capture_file("tests/new_test.rs"); // doesn't exist yet — recorded as created
/// snap.save()?;
/// // ... do the write operation ...
/// ```
pub struct UndoSnapshot {
    root: PathBuf,
    label: String,
    entries: Vec<SnapshotEntry>,
    /// Actual file contents to persist, keyed by relative path.
    contents: Vec<(String, Vec<u8>)>,
}

impl UndoSnapshot {
    pub fn new(root: &Path, label: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            label: label.to_string(),
            entries: Vec::new(),
            contents: Vec::new(),
        }
    }

    /// Capture a file's current state before modification.
    /// If the file exists, its content is saved. If it doesn't exist (will be created),
    /// we record it so undo can remove it.
    pub fn capture_file(&mut self, relative_path: &str) {
        // Don't capture the same file twice
        if self
            .entries
            .iter()
            .any(|e| e.relative_path == relative_path)
        {
            return;
        }

        let abs = self.root.join(relative_path);

        // Use shared FileStateEntry for capture, then extract what we need
        // for persistent storage.
        let state = FileStateEntry::capture(&abs);

        if let Some(ref content) = state.original_content {
            self.contents
                .push((relative_path.to_string(), content.clone()));
        }

        self.entries.push(SnapshotEntry {
            relative_path: relative_path.to_string(),
            had_content: state.had_content(),
        });
    }

    /// Save the snapshot to disk. Returns the snapshot ID.
    pub fn save(self) -> Result<String> {
        save_to_dir(self, &snapshots_dir())
    }

    /// Capture multiple files and save the snapshot in one step.
    ///
    /// Convenience method that eliminates the repeated create-capture-save
    /// boilerplate. Logs a warning on save failure (non-fatal — undo is
    /// best-effort and should never block the primary operation).
    pub fn capture_and_save(
        root: &Path,
        label: &str,
        files: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        let mut snap = Self::new(root, label);
        for file in files {
            snap.capture_file(file.as_ref());
        }
        if snap.entries.is_empty() {
            return;
        }
        if let Err(e) = snap.save() {
            crate::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
        }
    }
}

/// Save a snapshot to a specific directory. Used internally and by tests.
fn save_to_dir(snap: UndoSnapshot, base_dir: &Path) -> Result<String> {
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

/// Restore the most recent snapshot, or a specific one by ID.
pub fn restore(snapshot_id: Option<&str>) -> Result<RestoreResult> {
    restore_from_dir(snapshot_id, &snapshots_dir())
}

/// Restore from a specific snapshot directory. Used internally and by tests.
fn restore_from_dir(snapshot_id: Option<&str>, base_dir: &Path) -> Result<RestoreResult> {
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

/// List all available snapshots, newest first.
pub fn list_snapshots() -> Result<Vec<SnapshotSummary>> {
    list_snapshots_in(&snapshots_dir())
}

/// List snapshots from a specific directory. Used internally and by tests.
fn list_snapshots_in(base_dir: &Path) -> Result<Vec<SnapshotSummary>> {
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
fn delete_snapshot_in(snapshot_id: &str, base_dir: &Path) -> Result<()> {
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

// ============================================================================
// Internal helpers
// ============================================================================

fn snapshots_dir() -> PathBuf {
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

fn generate_snapshot_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_millis())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn latest_snapshot_id_in(base_dir: &Path) -> Result<String> {
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

fn load_manifest(snapshot_dir: &Path) -> Result<SnapshotManifest> {
    let manifest_path = snapshot_dir.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        crate::Error::internal_unexpected(format!("Failed to read manifest: {}", e))
    })?;
    serde_json::from_str(&content)
        .map_err(|e| crate::Error::internal_unexpected(format!("Failed to parse manifest: {}", e)))
}

/// Convert a path like "src/core/fixer.rs" to a safe filename for snapshot storage.
/// Replaces `/` with `__` to flatten the directory structure.
fn sanitize_path(relative_path: &str) -> String {
    relative_path.replace('/', "__")
}

fn expire_old_snapshots_in(base_dir: &Path, keep: usize) {
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

fn format_age(seconds: u64) -> String {
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

/// Remove empty parent directories up to (but not including) the root.
fn remove_empty_parents(dir: &Path, root: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create an isolated project root for a test.
    fn test_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-undo-{}-{}", name, nanos))
    }

    /// Create an isolated snapshot directory for a test (avoids parallel test interference).
    fn test_snap_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-snapdir-{}-{}", name, nanos))
    }

    /// Save a snapshot to a test-isolated snapshot directory.
    fn save_isolated(snap: UndoSnapshot, snap_dir: &Path) -> crate::Result<String> {
        save_to_dir(snap, snap_dir)
    }

    #[test]
    fn snapshot_captures_existing_file_and_restores() {
        let root = test_root("capture-restore");
        let snap_dir = test_snap_dir("capture-restore");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        // Snapshot the file
        let mut snap = UndoSnapshot::new(&root, "test fix");
        snap.capture_file("src/main.rs");
        let id = save_isolated(snap, &snap_dir).unwrap();

        // Modify the file (simulate a --write operation)
        fs::write(root.join("src/main.rs"), "fn main() { changed }\n").unwrap();
        assert!(fs::read_to_string(root.join("src/main.rs"))
            .unwrap()
            .contains("changed"));

        // Undo
        let result = restore_from_dir(Some(&id), &snap_dir).unwrap();
        assert_eq!(result.files_restored, 1);
        assert_eq!(result.files_removed, 0);
        assert!(result.errors.is_empty());

        // File is back to original
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn snapshot_removes_created_files_on_undo() {
        let root = test_root("remove-created");
        let snap_dir = test_snap_dir("remove-created");
        fs::create_dir_all(root.join("src")).unwrap();

        // Snapshot a file that doesn't exist yet
        let mut snap = UndoSnapshot::new(&root, "test scaffold");
        snap.capture_file("tests/new_test.rs");
        let id = save_isolated(snap, &snap_dir).unwrap();

        // Create the file (simulate a --write operation)
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("tests/new_test.rs"), "#[test]\nfn test_it() {}\n").unwrap();
        assert!(root.join("tests/new_test.rs").exists());

        // Undo
        let result = restore_from_dir(Some(&id), &snap_dir).unwrap();
        assert_eq!(result.files_restored, 0);
        assert_eq!(result.files_removed, 1);

        // File is gone
        assert!(!root.join("tests/new_test.rs").exists());
        // Empty parent dir is cleaned up
        assert!(!root.join("tests").exists());

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn snapshot_handles_mixed_existing_and_new_files() {
        let root = test_root("mixed");
        let snap_dir = test_snap_dir("mixed");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub mod foo;\n").unwrap();

        let mut snap = UndoSnapshot::new(&root, "audit fix");
        snap.capture_file("src/lib.rs");
        snap.capture_file("src/foo.rs");
        let id = save_isolated(snap, &snap_dir).unwrap();

        // Simulate writes
        fs::write(root.join("src/lib.rs"), "pub mod foo;\npub mod bar;\n").unwrap();
        fs::write(root.join("src/foo.rs"), "pub fn foo() {}\n").unwrap();

        let result = restore_from_dir(Some(&id), &snap_dir).unwrap();
        assert_eq!(result.files_restored, 1);
        assert_eq!(result.files_removed, 1);

        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "pub mod foo;\n"
        );
        assert!(!root.join("src/foo.rs").exists());

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn deduplicates_same_file_captured_twice() {
        let root = test_root("dedup");
        let snap_dir = test_snap_dir("dedup");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.rs"), "original\n").unwrap();

        let mut snap = UndoSnapshot::new(&root, "test");
        snap.capture_file("file.rs");
        snap.capture_file("file.rs"); // duplicate
        let id = save_isolated(snap, &snap_dir).unwrap();

        let snapshots = list_snapshots_in(&snap_dir).unwrap();
        let found = snapshots.iter().find(|s| s.id == id).unwrap();
        assert_eq!(found.file_count, 1);

        // Clean up
        delete_snapshot_in(&id, &snap_dir).unwrap();
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn list_snapshots_returns_newest_first() {
        let root = test_root("list-order");
        let snap_dir = test_snap_dir("list-order");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rs"), "a\n").unwrap();

        let mut snap1 = UndoSnapshot::new(&root, "first");
        snap1.capture_file("a.rs");
        let id1 = save_isolated(snap1, &snap_dir).unwrap();

        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(5));

        let mut snap2 = UndoSnapshot::new(&root, "second");
        snap2.capture_file("a.rs");
        let id2 = save_isolated(snap2, &snap_dir).unwrap();

        let snapshots = list_snapshots_in(&snap_dir).unwrap();
        let ids: Vec<&str> = snapshots.iter().map(|s| s.id.as_str()).collect();
        let pos1 = ids.iter().position(|id| *id == id1).unwrap();
        let pos2 = ids.iter().position(|id| *id == id2).unwrap();
        assert!(pos2 < pos1, "newest (id2) should come before oldest (id1)");

        // Clean up
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn restore_latest_picks_most_recent() {
        let root = test_root("restore-latest");
        let snap_dir = test_snap_dir("restore-latest");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rs"), "original\n").unwrap();

        let mut snap1 = UndoSnapshot::new(&root, "old");
        snap1.capture_file("a.rs");
        let _id1 = save_isolated(snap1, &snap_dir).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));

        // Modify the file
        fs::write(root.join("a.rs"), "after-first\n").unwrap();

        let mut snap2 = UndoSnapshot::new(&root, "new");
        snap2.capture_file("a.rs");
        let _id2 = save_isolated(snap2, &snap_dir).unwrap();

        // Modify again
        fs::write(root.join("a.rs"), "after-second\n").unwrap();

        // Undo latest (should restore to "after-first")
        let result = restore_from_dir(None, &snap_dir).unwrap();
        assert_eq!(result.label, "new");
        assert_eq!(
            fs::read_to_string(root.join("a.rs")).unwrap(),
            "after-first\n"
        );

        // Undo again (should restore to "original")
        let result = restore_from_dir(None, &snap_dir).unwrap();
        assert_eq!(result.label, "old");
        assert_eq!(fs::read_to_string(root.join("a.rs")).unwrap(), "original\n");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn empty_snapshot_returns_error() {
        let root = test_root("empty");
        let snap_dir = test_snap_dir("empty");
        fs::create_dir_all(&root).unwrap();

        let snap = UndoSnapshot::new(&root, "empty");
        assert!(save_isolated(snap, &snap_dir).is_err());

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(snap_dir);
    }

    #[test]
    fn sanitize_path_flattens_slashes() {
        assert_eq!(sanitize_path("src/core/fixer.rs"), "src__core__fixer.rs");
        assert_eq!(sanitize_path("simple.rs"), "simple.rs");
    }

    #[test]
    fn format_age_outputs_readable_strings() {
        assert_eq!(format_age(30), "30s ago");
        assert_eq!(format_age(90), "1m ago");
        assert_eq!(format_age(7200), "2h ago");
        assert_eq!(format_age(172800), "2d ago");
    }
}
