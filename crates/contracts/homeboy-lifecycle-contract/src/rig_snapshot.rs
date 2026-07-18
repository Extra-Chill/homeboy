//! Rig state snapshot contract types (component filesystem/git snapshot).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Captured component state for one entry in a rig's components map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComponentSnapshot {
    /// Effective filesystem path used for the invocation.
    ///
    /// When a `--path` override (or any caller-supplied effective path) is in
    /// effect, this is the override path — not the rig-declared one — so the
    /// snapshot accurately reflects what the workload actually exercised. See
    /// [`RigStateSnapshot::set_effective_component_path`].
    pub path: String,

    /// Rig-declared path that `path` was overridden away from, if any.
    ///
    /// Absent when `path` matches the rig spec. Present when the invocation
    /// pinned a different checkout via `--path` or another override, so
    /// callers can still see what the rig itself configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_path: Option<String>,

    /// `git rev-parse HEAD` for the path's repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,

    /// `git rev-parse --abbrev-ref HEAD`, or `HEAD` when detached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Snapshot of every component in a rig at a moment in time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RigStateSnapshot {
    pub rig_id: String,
    pub captured_at: String,
    pub components: BTreeMap<String, ComponentSnapshot>,
}

impl RigStateSnapshot {
    /// Record the effective filesystem path used for `component_id`.
    ///
    /// If the snapshot already lists the component and the rig-declared `path`
    /// differs from `effective_path`, the rig-declared path is preserved in
    /// `declared_path` and `path` is updated to the effective one. Git SHA and
    /// branch are re-queried against the effective path so the snapshot
    /// reflects what was actually exercised.
    ///
    /// No-op when the component isn't tracked by the snapshot or the effective
    /// path already matches `path`.
    pub fn set_effective_component_path(
        &mut self,
        component_id: &str,
        effective_path: &str,
        git_lookup: impl FnOnce(&str) -> (Option<String>, Option<String>),
    ) {
        let Some(snapshot) = self.components.get_mut(component_id) else {
            return;
        };
        if snapshot.path == effective_path {
            return;
        }
        let declared = std::mem::replace(&mut snapshot.path, effective_path.to_string());
        snapshot.declared_path = Some(declared);
        let (sha, branch) = git_lookup(effective_path);
        snapshot.sha = sha;
        snapshot.branch = branch;
    }
}
