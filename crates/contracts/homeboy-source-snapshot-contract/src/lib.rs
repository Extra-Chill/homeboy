//! Pure serializable source-snapshot contract types.
//!
//! `SourceSnapshot` and `SourceSnapshotPolicy` describe the identity of a source
//! tree and its sync-exclude rules as they cross process boundaries. They are
//! behavior-free (serde only); the git/component/extension/fs-driven collection
//! behavior lives in `homeboy-core`.

pub mod source_snapshot;

pub use source_snapshot::{default_sync_excludes, SourceSnapshot, SourceSnapshotPolicy};
