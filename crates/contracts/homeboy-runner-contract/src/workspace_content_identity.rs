//! Canonical specification of a materialized workspace's *content identity*.
//!
//! When the controller declares a Lab workspace and the runner verifies the
//! materialized result, both sides must agree on exactly one thing: does the
//! runner's tree contain the same content the controller described? That
//! agreement hinges on computing an identical **content hash** on both sides,
//! which in turn requires an identical **algorithm identity** — the marker
//! string that names the hashing rules (traversal version + permission policy).
//!
//! This module is the single source of truth for that algorithm identity: the
//! permission-policy names, the default policy, and the policy → algorithm
//! marker mapping. It is pure data + a pure `match`, so it lives in the runner
//! contract rather than in the runner's filesystem-walking code — the
//! controller-declare path and the runner-verify path both derive the marker
//! from here, so they cannot disagree on what a given policy *means*.
//!
//! ## Algorithm lineage (why the markers are versioned)
//!
//! The hashing rules have evolved; each change is a new marker so a digest can
//! never be reinterpreted under different rules:
//! Retired v1 sorted-entry records are refused by the current verifier.
//! Supported policy markers are:
//! - `homeboy-workspace-content-v2+<policy>` — streaming traversal; the
//!   permission policy decides whether/which execute bit is bound.
//! - `homeboy-workspace-content-v3+unix-owner-executable` — binds only the
//!   owner-execute bit, ignoring umask'd group/other bits.
//!
//! The *filesystem traversal* that consumes these markers (reading modes,
//! hashing bytes, applying excludes) stays in `homeboy-lab-runner`; only the
//! identity spec lives here.

use serde::{Deserialize, Serialize};

/// Content-only identity: bytes and structure, no execute bit. Portable across
/// platforms.
pub const WORKSPACE_CONTENT_PERMISSION_PORTABLE: &str = "portable-content-only";

/// Binds whether *any* execute bit is set (Unix).
pub const WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE: &str = "unix-executable";

/// Binds only the *owner* execute bit, ignoring umask'd group/other bits (Unix).
pub const WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE: &str = "unix-owner-executable";

/// The default permission policy for new workspace-content identities.
///
/// Snapshot archives can cross controller and runner platform boundaries. File
/// bytes and paths survive that transfer, while Unix mode bits are not a
/// portable provenance capability. New snapshots therefore use the content-only
/// policy; explicit Unix policies remain available to verify their versioned
/// historical contracts.
pub const WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY: &str = WORKSPACE_CONTENT_PERMISSION_PORTABLE;

/// The algorithm-identity marker string for a permission policy, or `None` if
/// the policy is unsupported on this platform.
///
/// This marker is embedded in the hash and recorded in the workspace
/// verification metadata; controller and runner both derive it from here so a
/// digest is always interpreted under the same rules that produced it.
pub fn workspace_content_hash_algorithm(policy: &str) -> Option<String> {
    match policy {
        WORKSPACE_CONTENT_PERMISSION_PORTABLE => {
            Some("homeboy-workspace-content-v2+portable-content-only".to_string())
        }
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE if cfg!(unix) => {
            Some("homeboy-workspace-content-v2+unix-executable".to_string())
        }
        WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE if cfg!(unix) => {
            Some("homeboy-workspace-content-v3+unix-owner-executable".to_string())
        }
        _ => None,
    }
}

/// A deterministic, content-free inventory of every path a snapshot
/// materializes, persisted with the immutable snapshot and used to derive
/// explicit incremental transport and deletion sets and to diagnose content-hash
/// mismatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceContentManifest {
    pub entry_count: usize,
    pub entries: Vec<WorkspaceContentManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceContentManifestEntry {
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_executable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Algorithm markers are recorded in workspace verification metadata and
    /// compared across controller and runner versions, so each supported
    /// policy keeps its versioned marker; the default stays the portable
    /// policy, and an unknown policy has no algorithm.
    #[test]
    fn policies_map_to_their_versioned_markers() {
        let mut cases = vec![
            (
                WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                Some("homeboy-workspace-content-v2+portable-content-only"),
            ),
            (
                WORKSPACE_CONTENT_PERMISSION_PORTABLE,
                Some("homeboy-workspace-content-v2+portable-content-only"),
            ),
            ("nonsense-policy", None),
        ];
        if cfg!(unix) {
            cases.extend([
                (
                    WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
                    Some("homeboy-workspace-content-v2+unix-executable"),
                ),
                (
                    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
                    Some("homeboy-workspace-content-v3+unix-owner-executable"),
                ),
            ]);
        }
        for (policy, marker) in cases {
            assert_eq!(
                workspace_content_hash_algorithm(policy).as_deref(),
                marker,
                "{policy}"
            );
        }
    }
}
