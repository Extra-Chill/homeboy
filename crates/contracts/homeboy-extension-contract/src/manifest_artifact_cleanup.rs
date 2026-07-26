//! Extension-owned artifact cleanup declarations.
//!
//! An extension owns which of its install/build outputs are reconstructable and
//! how an operator rehydrates them. It does not own removal: Homeboy core
//! resolves these declarations across managed worktrees and keeps ownership of
//! dry-run/apply, path containment, active-worktree and age gating, Git safety,
//! limits, and byte accounting.
//!
//! Declarations are intentionally narrow. A declaration names one relative
//! artifact path and the install scopes it may resolve beside, so resolution
//! stays anchored to scopes the extension actually supports instead of an
//! arbitrary recursive deletion glob.

use serde::{Deserialize, Serialize};

/// Default depth bound applied to nested install-scope discovery when a
/// declaration does not set its own bound. Deep enough for nested per-package
/// install scopes, shallow enough to stay a bounded scan.
pub const DEFAULT_NESTED_SCOPE_MAX_DEPTH: usize = 6;

/// Artifact classes an extension can declare.
///
/// The category drives eligibility, not just reporting: only reconstructable
/// classes are removal candidates. `ReleaseAsset` exists so an extension can
/// make deployment-bearing output *visible* to cleanup review while keeping it
/// permanently protected from removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCleanupCategory {
    /// Installed dependency trees, reconstructable from a committed lockfile.
    Dependencies,
    /// Generated build output, reconstructable by re-running the build.
    BuildOutput,
    /// Tool caches, reconstructable on demand and safe to lose.
    BuildCache,
    /// Packaged output required for deployment. Never a removal candidate.
    ReleaseAsset,
}

impl ArtifactCleanupCategory {
    /// Whether artifacts in this category can be rebuilt from tracked sources
    /// and are therefore eligible for removal.
    pub fn is_reconstructable(&self) -> bool {
        !matches!(self, Self::ReleaseAsset)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::BuildOutput => "build_output",
            Self::BuildCache => "build_cache",
            Self::ReleaseAsset => "release_asset",
        }
    }
}

/// An install scope a declared artifact path may resolve beside.
///
/// A scope is a directory that carries every file in `manifest_files`. Root-only
/// scopes keep resolution at the worktree root; nested scopes additionally
/// resolve beside supported nested install manifests, bounded by `max_depth`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCleanupScope {
    /// Files that must all exist in a directory for it to be an install scope.
    /// An empty list resolves the worktree root unconditionally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest_files: Vec<String>,

    /// Resolve nested install scopes below the worktree root, not just the root.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nested: bool,

    /// Depth bound for nested discovery, relative to the worktree root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

impl ArtifactCleanupScope {
    pub fn depth_bound(&self) -> usize {
        if !self.nested {
            return 0;
        }
        self.max_depth.unwrap_or(DEFAULT_NESTED_SCOPE_MAX_DEPTH)
    }
}

/// One extension-owned artifact cleanup declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCleanupDeclaration {
    /// Stable identifier, unique within the extension. Reported so operators
    /// can trace a candidate back to the rule that produced it.
    pub id: String,

    /// Artifact class. Drives removal eligibility.
    pub category: ArtifactCleanupCategory,

    /// Artifact path, relative to each resolved install scope.
    pub path: String,

    /// Install scopes this artifact resolves beside. Defaults to the worktree
    /// root when omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<ArtifactCleanupScope>,

    /// Operator-facing command that reinstalls or regenerates the artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rehydrate_command: Option<String>,

    /// Minimum artifact age before removal is allowed. Composes with the
    /// caller's age gate; the stricter of the two wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_age_days: Option<u64>,

    /// Human-readable retention/readiness tradeoff for this declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ArtifactCleanupDeclaration {
    /// Scopes to resolve, defaulting to a root-only scope so a declaration that
    /// omits `scopes` still has one well-defined resolution site.
    pub fn resolved_scopes(&self) -> Vec<ArtifactCleanupScope> {
        if self.scopes.is_empty() {
            return vec![ArtifactCleanupScope::default()];
        }
        self.scopes.clone()
    }
}

/// The `artifact_cleanup` manifest capability group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCleanupConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declarations: Vec<ArtifactCleanupDeclaration>,
}

impl ArtifactCleanupConfig {
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_assets_are_never_reconstructable() {
        assert!(ArtifactCleanupCategory::Dependencies.is_reconstructable());
        assert!(ArtifactCleanupCategory::BuildOutput.is_reconstructable());
        assert!(ArtifactCleanupCategory::BuildCache.is_reconstructable());
        assert!(!ArtifactCleanupCategory::ReleaseAsset.is_reconstructable());
    }

    #[test]
    fn declaration_without_scopes_resolves_one_root_scope() {
        let declaration = ArtifactCleanupDeclaration {
            id: "generated".to_string(),
            category: ArtifactCleanupCategory::BuildOutput,
            path: "generated".to_string(),
            scopes: Vec::new(),
            rehydrate_command: None,
            min_age_days: None,
            description: None,
        };

        let scopes = declaration.resolved_scopes();

        assert_eq!(scopes.len(), 1);
        assert!(scopes[0].manifest_files.is_empty());
        assert!(!scopes[0].nested);
        assert_eq!(scopes[0].depth_bound(), 0);
    }

    #[test]
    fn nested_scope_depth_defaults_when_unbounded() {
        let scope = ArtifactCleanupScope {
            manifest_files: vec!["scope.marker".to_string()],
            nested: true,
            max_depth: None,
        };

        assert_eq!(scope.depth_bound(), DEFAULT_NESTED_SCOPE_MAX_DEPTH);
        assert_eq!(
            ArtifactCleanupScope {
                max_depth: Some(2),
                ..scope
            }
            .depth_bound(),
            2
        );
    }

    #[test]
    fn config_round_trips_through_manifest_json() {
        let raw = serde_json::json!({
            "declarations": [
                {
                    "id": "dependency-tree",
                    "category": "dependencies",
                    "path": "deps",
                    "scopes": [
                        { "manifest_files": ["scope.marker"], "nested": true, "max_depth": 3 }
                    ],
                    "rehydrate_command": "fixture install",
                    "min_age_days": 7,
                    "description": "Reinstallable from the committed lockfile."
                },
                {
                    "id": "packaged-output",
                    "category": "release_asset",
                    "path": "packaged"
                }
            ]
        });

        let parsed: ArtifactCleanupConfig = serde_json::from_value(raw).expect("config parses");

        assert_eq!(parsed.declarations.len(), 2);
        assert_eq!(
            parsed.declarations[0].category,
            ArtifactCleanupCategory::Dependencies
        );
        assert_eq!(parsed.declarations[0].scopes[0].depth_bound(), 3);
        assert_eq!(parsed.declarations[0].min_age_days, Some(7));
        assert!(!parsed.declarations[1].category.is_reconstructable());
        assert!(parsed.declarations[1].scopes.is_empty());
    }

    #[test]
    fn unknown_declaration_fields_are_rejected() {
        let raw = serde_json::json!({
            "declarations": [
                { "id": "x", "category": "build_output", "path": "out", "glob": "**/*" }
            ]
        });

        assert!(serde_json::from_value::<ArtifactCleanupConfig>(raw).is_err());
    }
}
