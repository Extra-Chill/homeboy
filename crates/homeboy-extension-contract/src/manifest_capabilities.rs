//! Extension capability contract types (deploy, audit, executable, platform,
//! agent-task policy).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::manifest_action_config::{InputConfig, RuntimeConfig};
use crate::manifest_capability_config::{DocTarget, FeatureContextRule};
use crate::manifest_deploy_config::DeployArchiveInstallPolicy;
use crate::manifest_toolchain_config::{
    DatabaseConfig, DeployOverride, DeployOwnerHint, DeployVerification, DiscoveryConfig,
    RemotePathInferenceRule, RemotePathRootRule, SinceTagConfig, VersionPatternConfig,
};
use homeboy_audit_contract::{AuditConfig, TestMappingConfig};

/// Deploy lifecycle: verification rules, install overrides, version patterns, @since tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployCapability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<DeployVerification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<DeployOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_path_suffixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_hints: Vec<DeployOwnerHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archive_install: Vec<DeployArchiveInstallPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_path_inference: Vec<RemotePathInferenceRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_roots: Vec<RemotePathRootRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_patterns: Vec<VersionPatternConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_tag: Option<SinceTagConfig>,
}

/// Docs audit: ignore patterns and feature detection patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCapability {
    /// Shell script that resolves reference dependencies and exports
    /// `HOMEBOY_AUDIT_REFERENCE_PATHS` (newline-separated directory paths).
    /// Reference dependencies are fingerprinted for cross-reference analysis
    /// (dead code detection) but excluded from convention and duplication detection.
    /// Example: framework or package dependencies declared by an extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_references: Option<String>,
    /// Detector rules supplied by this extension for its language/framework.
    #[serde(default, skip_serializing_if = "AuditConfig::is_empty")]
    pub detector_rules: AuditConfig,
    /// Glob patterns for paths to ignore during docs audit.
    /// Uses `*` for single segment and `**` for multiple segments (e.g., `/wp-json/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_claim_patterns: Vec<String>,
    /// Regex patterns to detect feature registrations in source code.
    /// Each pattern should have a capture group for the feature name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feature_patterns: Vec<String>,
    /// Human-readable labels for feature patterns, keyed by a substring of the pattern.
    /// Used by `docs generate --from-audit` to group and label features in generated docs.
    /// Example: `{"register_post_type": "Post Types", "register_rest_route": "REST API Routes"}`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_labels: HashMap<String, String>,
    /// Doc generation targets: maps a feature label to a file path and optional heading.
    /// Used by `docs generate --from-audit` to place features in the right doc files.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub doc_targets: HashMap<String, DocTarget>,
    /// Context extraction rules for feature patterns, keyed by a substring of the pattern.
    /// Tells the audit system what additional context to extract around each detected feature.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_context: HashMap<String, FeatureContextRule>,
    /// Test mapping convention for structural test coverage gap detection.
    /// Defines how source files map to test files and how methods map to test methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_mapping: Option<TestMappingConfig>,
}

/// Executable tool: runtime, inputs, and output schema.
/// Represents a extension that can be run as a standalone tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableCapability {
    pub runtime: RuntimeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputConfig>,

    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Desktop/platform UI config: pinned files, database, discovery, commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentTaskPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
}
