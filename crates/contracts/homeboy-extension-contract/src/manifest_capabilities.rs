//! Extension capability contract types (deploy, executable, platform,
//! agent-task policy).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::manifest_action_config::{InputConfig, RuntimeConfig};
use crate::manifest_deploy_config::DeployArchiveInstallPolicy;
use crate::manifest_toolchain_config::{
    DatabaseConfig, DeployOverride, DeployOwnerHint, DeployVerification, DiscoveryConfig,
    RemotePathInferenceRule, RemotePathRootRule, SinceTagConfig, VersionPatternConfig,
};

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
