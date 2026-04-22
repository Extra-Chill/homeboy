//! Undo system for homeboy write operations.
//!
//! Two subsystems sharing common primitives:
//! - **Rollback** (`InMemoryRollback`): Ephemeral in-memory rollback for per-chunk
//!   verification during fixer operations.
//! - **Snapshot** (`UndoSnapshot`): Persistent disk-based undo for `homeboy undo`.
//!   Saves file state before `--write` operations, restores on demand.
//!
//! Both use [`FileStateEntry`] for capture and [`restore_entries`] for restore
//! logic, ensuring the in-memory and persistent undo paths share the same
//! underlying file-state primitives.

mod entry;
mod rollback;
mod snapshot;

// Re-export everything at module level to preserve existing import paths.
pub use entry::{restore_entries, FileStateEntry};
pub use rollback::InMemoryRollback;
pub use snapshot::{
    delete_snapshot, list_snapshots, restore, RestoreResult, SnapshotEntry, SnapshotManifest,
    SnapshotSummary, UndoSnapshot,
};
