//! Provider source-root type read by both core and the agents crate.
//!
//! Core's agent-runtime manifest reads a source root's `git_ref`/`id` for
//! immutability checks and remote-drift diagnostics, so this type lives in the
//! contract (below core) rather than the agents crate.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A named, extension-declared source checkout that homeboy keeps synced on the
/// runner. Core treats this generically: it materializes/refreshes a git
/// checkout to the intended ref/remote. It has no knowledge of what the source
/// is (a runtime checkout, a CLI, a toolchain) — extensions declare the path/remote/ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRunnerSource {
    pub id: String,
    pub label: String,
    /// Absolute path (or `$HOME`/`~`-prefixed path) of the managed checkout on
    /// the runner, e.g. a path under the runner's homeboy cache directory.
    pub path: String,
    /// Optional canonical remote URL the checkout must track. When set, homeboy
    /// re-points `origin` if the checkout tracks a different remote (fixing the
    /// "tracks wrong remote" drift), then fetches and fast-forwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Optional explicit ref (branch, tag, or sha) to check out and sync to.
    /// When omitted, homeboy fast-forwards the current branch to its upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}
