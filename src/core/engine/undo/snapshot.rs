//! Persistent undo snapshots for write operations.
//!
//! Before any `--write` operation (audit fix, refactor rename/move/transform/decompose),
//! callers snapshot all files that will be modified or created. After the operation,
//! `homeboy undo` restores the snapshot — even if the working tree had uncommitted changes.
//!
//! Snapshots are stored at `~/.cache/homeboy/snapshots/<id>/` with a manifest.json
//! and copies of original file contents. Created files are recorded with `original: null`
//! so undo can remove them.

mod dir;
mod helpers;
mod snapshot;
mod snapshots_dir;
mod types;
mod undo_snapshot;

pub use dir::*;
pub use helpers::*;
pub use snapshot::*;
pub use snapshots_dir::*;
pub use types::*;
pub use undo_snapshot::*;


use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::Result;

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
        let had_content = abs.is_file();

        if had_content {
            if let Ok(content) = std::fs::read(&abs) {
                self.contents.push((relative_path.to_string(), content));
            }
        }

        self.entries.push(SnapshotEntry {
            relative_path: relative_path.to_string(),
            had_content,
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

// ============================================================================
// Internal helpers
// ============================================================================

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
