//! Undo system for homeboy write operations.
//!
//! Two independent subsystems:
//! - **Snapshot** (`UndoSnapshot`): Persistent disk-based undo for `homeboy undo`.
//!   Saves file state before `--write` operations, restores on demand.
//! - **Rollback** (`InMemoryRollback`): Ephemeral in-memory rollback for per-chunk
//!   verification during fixer operations.

mod rollback;
mod snapshot;

// Re-export everything at module level to preserve existing import paths.
pub use rollback::InMemoryRollback;
pub use snapshot::{
    delete_snapshot, list_snapshots, restore, RestoreResult, SnapshotEntry, SnapshotManifest,
    SnapshotSummary, UndoSnapshot,
};
