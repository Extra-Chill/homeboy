//! Pure serializable source-snapshot contract types.
//!
//! `SourceSnapshot` and `SourceSnapshotPolicy` describe the identity of a source
//! tree and its sync-exclude rules as they cross process boundaries. They are
//! behavior-free (serde only); the git/component/extension/fs-driven collection
//! behavior lives in `homeboy-core`.

pub mod source_snapshot;
pub mod workspace_content_identity;

pub use source_snapshot::{default_sync_excludes, SourceSnapshot, SourceSnapshotPolicy};
pub use workspace_content_identity::{
    workspace_content_hash_algorithm, WorkspaceContentManifest, WorkspaceContentManifestEntry,
    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY, WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
};
