//! Transport-neutral runner workspace state contracts.

use serde::{Deserialize, Serialize};

use crate::RunnerLifecycleOwner;

/// File and byte counts for a runner workspace transfer.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct ByteFileCounts {
    pub files: usize,
    pub bytes: u64,
}

/// A lease describing a runner's materialized workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerWorkspaceLease {
    pub runner_id: String,
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: String,
    pub materialized: bool,
    pub lifecycle_owner: RunnerLifecycleOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
}

/// A summary of a runner's current workspace materialization.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerWorkspaceCurrentSummary {
    pub local_path: String,
    pub remote_path: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
    /// Commit SHA of the synthetic git checkout created for a `snapshot-git`
    /// sync, so write-capable agent-task dispatches can trace the dirty
    /// controller-side worktree back to the synthetic commit that carries it
    /// into the runner workspace. `None` for plain `snapshot`/`git` syncs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_checkout_tree: Option<String>,
}

/// How a runner workspace is synced before a job runs.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerWorkspaceSyncMode {
    #[default]
    Snapshot,
    /// Deliberate exception to `rename_all`: this mode's wire string is
    /// `snapshot-git`, the spelling every durable consumer already uses.
    #[serde(rename = "snapshot-git")]
    SnapshotGit,
    Git,
}

impl RunnerWorkspaceSyncMode {
    /// This mode as its canonical serialized value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::SnapshotGit => "snapshot-git",
            Self::Git => "git",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_file_counts_keep_their_wire_shape_and_zero_default() {
        assert_eq!(
            serde_json::to_value(ByteFileCounts::default()).expect("serialize"),
            serde_json::json!({ "files": 0, "bytes": 0 })
        );
        assert_eq!(
            serde_json::to_value(ByteFileCounts {
                files: 3,
                bytes: 1024,
            })
            .expect("serialize"),
            serde_json::json!({ "files": 3, "bytes": 1024 })
        );
    }

    #[test]
    fn workspace_lease_keeps_its_minimal_wire_shape() {
        let value = serde_json::json!({
            "runner_id": "lab-a",
            "local_path": "/local/project",
            "remote_path": "/runner/project",
            "sync_mode": "snapshot",
            "materialized": true,
            "lifecycle_owner": "controller",
        });
        let lease = RunnerWorkspaceLease {
            runner_id: "lab-a".to_string(),
            local_path: "/local/project".to_string(),
            remote_path: "/runner/project".to_string(),
            sync_mode: "snapshot".to_string(),
            materialized: true,
            lifecycle_owner: RunnerLifecycleOwner::Controller,
            source_commit: None,
            source_ref: None,
            source_dirty: None,
        };

        assert_eq!(serde_json::to_value(&lease).expect("serialize"), value);
        assert_eq!(
            serde_json::from_value::<RunnerWorkspaceLease>(value).expect("deserialize"),
            lease
        );
    }

    #[test]
    fn current_workspace_keeps_its_complete_wire_shape() {
        let summary = RunnerWorkspaceCurrentSummary {
            local_path: "/local/project".to_string(),
            remote_path: "/runner/project".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::SnapshotGit,
            materialized: true,
            source_commit: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: Some(true),
            synthetic_checkout_commit: Some("def456".to_string()),
            synthetic_checkout_ref: Some("refs/homeboy/snapshot".to_string()),
            synthetic_checkout_tree: Some("789abc".to_string()),
        };

        assert_eq!(
            serde_json::to_value(summary).expect("serialize"),
            serde_json::json!({
                "local_path": "/local/project",
                "remote_path": "/runner/project",
                "sync_mode": "snapshot-git",
                "materialized": true,
                "source_commit": "abc123",
                "source_ref": "main",
                "source_dirty": true,
                "synthetic_checkout_commit": "def456",
                "synthetic_checkout_ref": "refs/homeboy/snapshot",
                "synthetic_checkout_tree": "789abc",
            })
        );
    }

    #[test]
    fn workspace_sync_mode_matches_its_serialized_form() {
        for mode in [
            RunnerWorkspaceSyncMode::Snapshot,
            RunnerWorkspaceSyncMode::SnapshotGit,
            RunnerWorkspaceSyncMode::Git,
        ] {
            assert_eq!(
                serde_json::to_value(mode).expect("serialize"),
                serde_json::json!(mode.as_str()),
                "{mode:?}"
            );
        }
    }
}
