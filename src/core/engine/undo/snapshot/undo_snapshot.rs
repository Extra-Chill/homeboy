//! undo_snapshot — extracted from snapshot.rs.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::Result;


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
