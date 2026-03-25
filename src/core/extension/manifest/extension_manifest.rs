//! extension_manifest — extracted from manifest.rs.

use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use super::ScriptsConfig;
use super::ActionConfig;
use super::RequirementsConfig;
use super::AuditCapability;
use super::TestConfig;
use super::ExecutableCapability;
use super::PlatformCapability;
use super::BuildConfig;
use super::ProvidesConfig;
use super::SettingConfig;
use super::DeployCapability;
use super::LintConfig;
use super::CliConfig;


/// Unified extension manifest decomposed into capability groups.
///
/// Extension JSON files use nested capability groups that map directly to these fields.
/// Convenience methods provide ergonomic access to nested data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    // Identity
    #[serde(default, skip_serializing)]
    pub id: String,
    pub name: String,
    pub version: String,

    // What this extension provides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provides: Option<ProvidesConfig>,

    // Capability scripts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ScriptsConfig>,

    // Optional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,

    // Capability groups
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<PlatformCapability>,

    // Standalone capabilities (already self-contained structs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint: Option<LintConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<TestConfig>,

    // Actions (cross-cutting: used by both platform and executable extensions)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionConfig>,

    // Lifecycle hooks: event name -> list of shell commands.
    // Extension hooks run before component hooks at each event.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<String, Vec<String>>,

    // Shared
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<SettingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequirementsConfig>,

    // Extensibility: preserve unknown fields for external consumers (GUI, workflows)
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,

    // Internal path (not serialized)
    #[serde(skip)]
    pub extension_path: Option<String>,
}

impl ConfigEntity for ExtensionManifest {
    const ENTITY_TYPE: &'static str = "extension";
    const DIR_NAME: &'static str = "extensions";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::extension_not_found(id, suggestions)
    }

    /// Override: extensions use `{dir}/{id}/{id}.json` pattern.
    fn config_path(id: &str) -> Result<PathBuf> {
        paths::extension_manifest(id)
    }
}
