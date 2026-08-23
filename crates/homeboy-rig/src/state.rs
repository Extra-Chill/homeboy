//! Rig runtime state persisted to `~/.config/homeboy/rigs/{id}.state/state.json`.
//!
//! State is ephemeral — losing it means `rig up` will re-check services on
//! next invocation. Never source-of-truth for the rig spec.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use homeboy_lifecycle_contract::LifecycleSnapshotRef;
pub use homeboy_lifecycle_contract::{ComponentSnapshot, RigStateSnapshot};

use super::spec::RigResourcesSpec;

/// Snapshot of a rig's running state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RigState {
    /// Timestamp of last successful `rig up`, RFC3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_up: Option<String>,

    /// Timestamp of last `rig check`, RFC3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,

    /// Result of last `rig check` — `"pass"` / `"fail"` / absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_result: Option<String>,

    /// Services the rig is managing.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub services: HashMap<String, ServiceState>,

    /// Shared dependency symlinks created by this rig and safe to remove on
    /// cleanup. Keyed by expanded link path.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub shared_paths: HashMap<String, SharedPathState>,

    /// Long-lived ownership materialized by the last successful `rig up`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized: Option<MaterializedRigState>,

    /// Effective component identities selected by the most recent invocation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_effective_components: BTreeMap<String, ComponentSnapshot>,

    /// Live lifecycle snapshot handles captured by `lifecycle` pipeline steps,
    /// keyed by snapshot ref id.
    ///
    /// This is the missing sandbox handle. Without it a throwaway environment
    /// exists only for the duration of the step that created it, and anything
    /// downstream has to reverse-engineer its location. With it, a later step
    /// or `rig down` addresses the environment by the id its own runtime
    /// handed back. Homeboy never interprets the handle.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lifecycle_snapshots: BTreeMap<String, LifecycleSnapshotState>,
}

/// One live lifecycle snapshot handle owned by this rig.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleSnapshotState {
    /// Pipeline step that captured the handle — the step `id` when declared,
    /// otherwise the component id, otherwise `lifecycle`. A successful
    /// `teardown` step reaps the handles it owns by this key.
    pub step: String,

    /// Component the lifecycle contract was declared against, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    /// Timestamp when the handle was captured, RFC3339.
    pub captured_at: String,

    /// The opaque handle itself, exactly as the runtime returned it.
    pub snapshot: LifecycleSnapshotRef,
}

/// Persistent record of what a successful `rig up` materialized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterializedRigState {
    /// Rig identifier that wrote this ownership record.
    pub rig_id: String,

    /// Timestamp when ownership was materialized, RFC3339.
    pub materialized_at: String,

    /// Expanded rig resources captured at materialization time.
    pub resources: RigResourcesSpec,

    /// Component state captured at materialization time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, ComponentSnapshot>,
}

/// Per-service state: PID, start time, health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceState {
    /// Running process ID. `None` if the service isn't started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,

    /// Timestamp when the current PID was started, RFC3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// Last observed status — `"running"` / `"stopped"` / `"unknown"`.
    pub status: String,
}

/// Per-shared-path ownership marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedPathState {
    /// Expanded target path the rig linked to when it created the symlink.
    pub target: String,

    /// Timestamp when the symlink was created, RFC3339.
    pub created_at: String,
}

/// Storage for rig runtime state, bound to one Homeboy home.
///
/// `RigState::load`/`save` previously resolved the config root on every call,
/// which made the resolution invisible at the call site and let a single rig run
/// read state from one home and persist it into another. Every one of those
/// call sites is a read-modify-write against the same file, so the root belongs
/// on the store rather than on seventeen free functions (#7505).
///
/// Service logs live beneath the same per-rig state directory, so the store owns
/// those paths too.
#[derive(Debug, Clone)]
pub struct RigStateStore {
    config_root: PathBuf,
}

impl RigStateStore {
    /// Bind the store to an already-resolved config root.
    pub fn in_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            config_root: config_root.into(),
        }
    }

    /// Bind the store to the config root of an already-resolved [`PathRoots`].
    pub fn in_roots(roots: &paths::PathRoots) -> Self {
        Self::in_root(roots.config())
    }

    /// The config root this store reads and writes beneath.
    pub fn config_root(&self) -> &Path {
        &self.config_root
    }

    /// Per-rig state directory (`<config_root>/rigs/{id}.state/`).
    pub fn state_dir(&self, rig_id: &str) -> PathBuf {
        paths::rig_state_dir_in_root(&self.config_root, rig_id)
    }

    /// Per-rig service log directory.
    pub fn logs_dir(&self, rig_id: &str) -> PathBuf {
        paths::rig_logs_dir_in_root(&self.config_root, rig_id)
    }

    /// Log file for one supervised service.
    pub fn log_file(&self, rig_id: &str, service_id: &str) -> PathBuf {
        self.logs_dir(rig_id).join(format!("{}.log", service_id))
    }

    /// Load state for a rig, returning a default (empty) state if the file
    /// doesn't exist. Missing state is not an error — it just means the rig
    /// hasn't been brought up yet on this machine.
    pub fn load(&self, rig_id: &str) -> Result<RigState> {
        let path = paths::rig_state_file_in_root(&self.config_root, rig_id);
        if !path.exists() {
            return Ok(RigState::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to read rig state {}: {}",
                path.display(),
                e
            ))
        })?;
        if content.trim().is_empty() {
            return Ok(RigState::default());
        }
        serde_json::from_str(&content).map_err(|e| {
            Error::validation_invalid_json(
                e,
                Some(format!("parse rig state {}", path.display())),
                Some(content.chars().take(200).collect()),
            )
        })
    }

    /// Persist state to disk. Creates the state directory if needed.
    pub fn save(&self, rig_id: &str, state: &RigState) -> Result<()> {
        let dir = self.state_dir(rig_id);
        fs::create_dir_all(&dir).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to create rig state dir {}: {}",
                dir.display(),
                e
            ))
        })?;
        let path = paths::rig_state_file_in_root(&self.config_root, rig_id);
        let json = serde_json::to_string_pretty(state).map_err(|e| {
            Error::internal_unexpected(format!("Failed to serialize rig state: {}", e))
        })?;
        fs::write(&path, json).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to write rig state {}: {}",
                path.display(),
                e
            ))
        })?;
        Ok(())
    }
}

/// A [`RigStateStore`] bound to the isolated home a test installs.
///
/// A test is the entry point for its own unit of work, so resolving once here
/// is a boundary resolution. What matters is that the production path beneath
/// it resolves nothing (#7505). Lives here rather than in each test file so
/// nested test modules can reach it by path.
#[cfg(test)]
pub(crate) fn test_state_store() -> RigStateStore {
    RigStateStore::in_roots(&paths::PathRoots::from_environment().expect("path roots"))
}

/// RFC3339 timestamp for state fields.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
#[path = "../../../tests/core/rig/state_test.rs"]
mod state_test;
