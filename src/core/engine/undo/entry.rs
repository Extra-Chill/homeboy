//! Shared file-state primitives for the undo system.
//!
//! Both `InMemoryRollback` (ephemeral) and `UndoSnapshot` (persistent) track
//! the same concept: a file's state before modification. This module extracts
//! the shared entry type and restore logic so both subsystems use the same
//! primitives instead of reimplementing them independently.

use std::path::{Path, PathBuf};

/// A captured file state — either an existing file with its original content,
/// or a file that didn't exist yet (will be created by the operation).
///
/// Used by both `InMemoryRollback` (content held in memory) and `UndoSnapshot`
/// (content persisted to disk, entry tracks whether it existed).
#[derive(Debug, Clone)]
pub struct FileStateEntry {
    /// Absolute path to the file on disk.
    pub path: PathBuf,
    /// Original content if the file existed before capture, `None` if it
    /// was newly created (didn't exist on disk at capture time).
    pub original_content: Option<Vec<u8>>,
}

impl FileStateEntry {
    /// Capture the current state of a file on disk.
    ///
    /// If the file exists, its content is read into memory. If it doesn't
    /// exist, `original_content` is `None` so a later restore can remove it.
    pub fn capture(path: &Path) -> Self {
        let original_content = if path.is_file() {
            std::fs::read(path).ok()
        } else {
            None
        };
        Self {
            path: path.to_path_buf(),
            original_content,
        }
    }

    /// Whether this entry represents a file that existed before capture.
    pub fn had_content(&self) -> bool {
        self.original_content.is_some()
    }
}

/// Restore a set of captured file entries to their original state.
///
/// - Files that existed are rewritten with their original content.
/// - Files that were created (didn't exist) are removed.
///
/// Returns `(files_restored, files_removed)` counts. Best-effort — individual
/// I/O failures are silently ignored, matching the existing rollback semantics.
pub fn restore_entries(entries: &[FileStateEntry]) -> (usize, usize) {
    let mut restored = 0usize;
    let mut removed = 0usize;

    for entry in entries {
        match &entry.original_content {
            Some(content) => {
                if std::fs::write(&entry.path, content).is_ok() {
                    restored += 1;
                }
            }
            None => {
                if std::fs::remove_file(&entry.path).is_ok() {
                    removed += 1;
                }
            }
        }
    }

    (restored, removed)
}

/// Check whether a path is already tracked in a list of entries.
///
/// Shared dedup logic used by both `InMemoryRollback` and `UndoSnapshot`
/// to avoid capturing the same file twice.
pub fn is_tracked(entries: &[FileStateEntry], path: &Path) -> bool {
    entries.iter().any(|e| e.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-entry-{}-{}", name, nanos))
    }

    #[test]
    fn capture_reads_existing_file() {
        let root = test_root("capture-existing");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.rs");
        fs::write(&file, "hello\n").unwrap();

        let entry = FileStateEntry::capture(&file);
        assert!(entry.had_content());
        assert_eq!(entry.original_content.unwrap(), b"hello\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capture_records_missing_file_as_created() {
        let root = test_root("capture-missing");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("absent.rs");

        let entry = FileStateEntry::capture(&file);
        assert!(!entry.had_content());
        assert!(entry.original_content.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_rewrites_modified_file() {
        let root = test_root("restore-rewrite");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.rs");
        fs::write(&file, "original\n").unwrap();

        let entry = FileStateEntry::capture(&file);

        // Simulate modification
        fs::write(&file, "modified\n").unwrap();

        let (restored, removed) = restore_entries(&[entry]);
        assert_eq!(restored, 1);
        assert_eq!(removed, 0);
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_removes_created_file() {
        let root = test_root("restore-remove");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("new.rs");

        let entry = FileStateEntry::capture(&file); // doesn't exist

        // Simulate creation
        fs::write(&file, "created\n").unwrap();
        assert!(file.exists());

        let (restored, removed) = restore_entries(&[entry]);
        assert_eq!(restored, 0);
        assert_eq!(removed, 1);
        assert!(!file.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restore_handles_mixed_entries() {
        let root = test_root("restore-mixed");
        fs::create_dir_all(&root).unwrap();
        let existing = root.join("existing.rs");
        let new_file = root.join("new.rs");
        fs::write(&existing, "before\n").unwrap();

        let entries = vec![
            FileStateEntry::capture(&existing),
            FileStateEntry::capture(&new_file),
        ];

        // Simulate writes
        fs::write(&existing, "after\n").unwrap();
        fs::write(&new_file, "created\n").unwrap();

        let (restored, removed) = restore_entries(&entries);
        assert_eq!(restored, 1);
        assert_eq!(removed, 1);
        assert_eq!(fs::read_to_string(&existing).unwrap(), "before\n");
        assert!(!new_file.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn is_tracked_finds_existing_entry() {
        let root = test_root("tracked");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("a.rs");
        fs::write(&file, "x\n").unwrap();

        let entry = FileStateEntry::capture(&file);
        let entries = vec![entry];

        assert!(is_tracked(&entries, &file));
        assert!(!is_tracked(&entries, &root.join("other.rs")));

        let _ = fs::remove_dir_all(root);
    }
}
