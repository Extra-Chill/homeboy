//! In-memory file rollback for per-chunk undo within a single operation.
//!
//! Unlike `UndoSnapshot` (which persists to disk for `homeboy undo`), this is
//! ephemeral — used by the fixer's chunk verifier to rollback individual chunks
//! that fail verification without affecting the persistent undo stack.
//!
//! Uses [`FileStateEntry`] from the shared undo primitives module for capture
//! and restore logic, so the in-memory and persistent undo paths share the
//! same underlying file-state tracking.
//!
//! Usage:
//! ```ignore
//! let mut rollback = InMemoryRollback::new();
//! rollback.capture(&abs_path);           // existing file — saves content
//! rollback.capture(&new_file_path);      // doesn't exist yet — recorded as created
//! // ... do the write ...
//! if verification_failed {
//!     rollback.restore_all();            // restores originals, removes created files
//! }
//! ```

use super::entry::{is_tracked, restore_entries, FileStateEntry};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct InMemoryRollback {
    entries: Vec<FileStateEntry>,
}

impl InMemoryRollback {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Capture a file's current state before modification.
    /// Deduplicates — capturing the same path twice is a no-op.
    pub fn capture(&mut self, path: &Path) {
        if is_tracked(&self.entries, path) {
            return;
        }
        self.entries.push(FileStateEntry::capture(path));
    }

    /// Restore all captured files to their original state.
    /// Files that existed are restored. Files that were created are removed.
    pub fn restore_all(&self) {
        let _ = restore_entries(&self.entries);
    }

    /// Number of files captured.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any files have been captured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-undo-{}-{}", name, nanos))
    }

    #[test]
    fn in_memory_rollback_restores_modified_file() {
        let root = test_root("imr-restore");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.rs");
        fs::write(&file, "original\n").unwrap();

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        assert_eq!(rollback.len(), 1);

        // Simulate a write
        fs::write(&file, "modified\n").unwrap();
        assert!(fs::read_to_string(&file).unwrap().contains("modified"));

        // Rollback
        rollback.restore_all();
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_memory_rollback_removes_created_file() {
        let root = test_root("imr-remove");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("new.rs");

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file); // doesn't exist yet

        // Simulate a write
        fs::write(&file, "created\n").unwrap();
        assert!(file.exists());

        // Rollback
        rollback.restore_all();
        assert!(!file.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_memory_rollback_deduplicates() {
        let root = test_root("imr-dedup");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("dup.rs");
        fs::write(&file, "content\n").unwrap();

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        rollback.capture(&file); // duplicate — should be ignored
        assert_eq!(rollback.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn in_memory_rollback_handles_mixed_files() {
        let root = test_root("imr-mixed");
        fs::create_dir_all(&root).unwrap();
        let existing = root.join("existing.rs");
        let new_file = root.join("new.rs");
        fs::write(&existing, "before\n").unwrap();

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&existing);
        rollback.capture(&new_file);
        assert_eq!(rollback.len(), 2);

        // Simulate writes
        fs::write(&existing, "after\n").unwrap();
        fs::write(&new_file, "created\n").unwrap();

        // Rollback
        rollback.restore_all();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "before\n");
        assert!(!new_file.exists());

        let _ = fs::remove_dir_all(root);
    }
}
