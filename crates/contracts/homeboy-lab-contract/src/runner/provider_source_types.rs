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

#[cfg(test)]
mod tests {
    use super::*;

    /// Extension-declared source roots arrive as JSON from outside this
    /// repository, so the accepted shape is a wire contract: the three optional
    /// fields must stay optional and unknown keys must land in `extra` rather
    /// than being rejected. Asserted here so relocating the type across crates
    /// cannot silently tighten it.
    #[test]
    fn a_minimal_source_root_round_trips_and_keeps_unknown_keys() {
        let source: AgentTaskProviderRunnerSource = serde_json::from_value(serde_json::json!({
            "id": "runtime",
            "label": "Runtime checkout",
            "path": "~/.cache/homeboy/runtime",
            "declared_by": "sample-extension",
        }))
        .expect("minimal source root deserializes");

        assert_eq!(source.remote_url, None);
        assert_eq!(source.git_ref, None);
        assert_eq!(source.remediation, None);
        assert_eq!(
            source.extra.get("declared_by").and_then(Value::as_str),
            Some("sample-extension"),
            "unknown keys are preserved in `extra`, not rejected"
        );

        let reserialized = serde_json::to_value(&source).expect("source root serializes");
        assert_eq!(
            reserialized,
            serde_json::json!({
                "id": "runtime",
                "label": "Runtime checkout",
                "path": "~/.cache/homeboy/runtime",
                "declared_by": "sample-extension",
            }),
            "absent optional fields stay absent on the wire"
        );
    }
}
