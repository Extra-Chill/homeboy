//! Rig runtime state persisted to `~/.config/homeboy/rigs/{id}.state/state.json`.
//!
//! State is ephemeral — losing it means `rig up` will re-check services on
//! next invocation. Never source-of-truth for the rig spec.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

use crate::error::{Error, Result};
use crate::paths;

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

impl RigState {
    /// Load state for a rig, returning a default (empty) state if the file
    /// doesn't exist. Missing state is not an error — it just means the rig
    /// hasn't been brought up yet on this machine.
    pub fn load(rig_id: &str) -> Result<Self> {
        let path = paths::rig_state_file(rig_id)?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to read rig state {}: {}",
                path.display(),
                e
            ))
        })?;
        if content.trim().is_empty() {
            return Ok(Self::default());
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
    pub fn save(&self, rig_id: &str) -> Result<()> {
        let dir = paths::rig_state_dir(rig_id)?;
        fs::create_dir_all(&dir).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to create rig state dir {}: {}",
                dir.display(),
                e
            ))
        })?;
        let path = paths::rig_state_file(rig_id)?;
        let json = serde_json::to_string_pretty(self).map_err(|e| {
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

/// RFC3339 timestamp for state fields.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
#[path = "../../../tests/core/rig/state_test.rs"]
mod state_test;
