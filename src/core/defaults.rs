use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

use crate::core::engine::local_files;
use crate::core::paths;

#[path = "defaults/builtins.rs"]
mod builtins;

pub(crate) use builtins::deploy_generated_build_dir;
pub(crate) use builtins::extension_provided_direct_test_file_suffixes;
pub(crate) use builtins::extension_provided_test_drift_config;

/// Root configuration structure for the product config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyConfig {
    #[serde(default)]
    pub defaults: Defaults,

    #[serde(default)]
    pub bench: BenchConfig,

    #[serde(default)]
    pub lab: LabConfig,

    #[serde(default)]
    pub triage: TriageConfig,

    #[serde(default)]
    pub agent_task: AgentTaskConfig,

    /// Directory where persisted run artifacts are copied.
    ///
    /// Defaults to the machine-local product data directory under
    /// `artifacts/`. Override with CLI, environment, or config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_root: Option<String>,

    /// Enable automatic update check on startup (default: true).
    /// Disable with `homeboy config set /update_check false`
    /// or set HOMEBOY_NO_UPDATE_CHECK=1.
    #[serde(default = "default_true")]
    pub update_check: bool,
}

impl Default for HomeboyConfig {
    fn default() -> Self {
        Self {
            defaults: Defaults::default(),
            bench: BenchConfig::default(),
            lab: LabConfig::default(),
            triage: TriageConfig::default(),
            agent_task: AgentTaskConfig::default(),
            artifact_root: None,
            update_check: true,
        }
    }
}

pub fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchConfig {
    #[serde(default)]
    pub local_execution: BenchLocalExecutionPolicy,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            local_execution: BenchLocalExecutionPolicy::Allowed,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BenchLocalExecutionPolicy {
    #[default]
    Allowed,
    Denied,
}

impl BenchLocalExecutionPolicy {
    pub fn is_denied(self) -> bool {
        matches!(self, Self::Denied)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LabConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_runner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TriageConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, AgentTaskSecretSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskSecretSource {
    #[serde(default = "default_agent_task_secret_source")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

fn default_agent_task_secret_source() -> String {
    "env".to_string()
}

/// All configurable defaults that can be overridden via the product config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default = "builtins::default_install_methods")]
    pub install_methods: InstallMethodsConfig,

    #[serde(default = "builtins::default_version_candidates")]
    pub version_candidates: Vec<VersionCandidateConfig>,

    #[serde(default = "builtins::default_deploy")]
    pub deploy: DeployConfig,

    #[serde(default = "builtins::default_permissions")]
    pub permissions: PermissionsConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            install_methods: builtins::default_install_methods(),
            version_candidates: builtins::default_version_candidates(),
            deploy: builtins::default_deploy(),
            permissions: builtins::default_permissions(),
        }
    }
}

/// Configuration for install method detection and upgrade commands
#[derive(Debug, Clone)]
pub struct InstallMethodsConfig {
    pub homebrew: InstallMethodConfig,
    pub secondary: InstallMethodConfig,
    pub source: InstallMethodConfig,
    pub binary: InstallMethodConfig,
}

pub(crate) fn secondary_install_method_key() -> String {
    ["car", "go"].concat()
}

impl Serialize for InstallMethodsConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("homebrew", &self.homebrew)?;
        map.serialize_entry(&secondary_install_method_key(), &self.secondary)?;
        map.serialize_entry("source", &self.source)?;
        map.serialize_entry("binary", &self.binary)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for InstallMethodsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut values = HashMap::<String, InstallMethodConfig>::deserialize(deserializer)?;
        let secondary_key = secondary_install_method_key();

        Ok(Self {
            homebrew: values
                .remove("homebrew")
                .unwrap_or_else(builtins::default_homebrew_config),
            secondary: values
                .remove(&secondary_key)
                .unwrap_or_else(builtins::default_secondary_install_config),
            source: values
                .remove("source")
                .unwrap_or_else(builtins::default_source_config),
            binary: values
                .remove("binary")
                .unwrap_or_else(builtins::default_binary_config),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallMethodConfig {
    pub path_patterns: Vec<String>,
    pub upgrade_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_command: Option<String>,
}

/// Configuration for version file detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionCandidateConfig {
    pub file: String,
    pub pattern: String,
}

/// Configuration for deploy operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployConfig {
    #[serde(default = "builtins::default_scp_flags")]
    pub scp_flags: Vec<String>,

    #[serde(default = "builtins::default_artifact_prefix")]
    pub artifact_prefix: String,

    #[serde(default = "builtins::default_ssh_port")]
    pub default_ssh_port: u16,
}

/// Configuration for file permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default = "builtins::default_local_permissions")]
    pub local: PermissionModes,

    #[serde(default = "builtins::default_remote_permissions")]
    pub remote: PermissionModes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionModes {
    pub file_mode: String,
    pub dir_mode: String,
}

// =============================================================================
// Loading functions
// =============================================================================

/// Load defaults, merging file config with built-in defaults.
/// If the product config file is missing or invalid, silently returns built-in defaults.
pub fn load_defaults() -> Defaults {
    load_config().defaults
}

/// Load the full product config, falling back to defaults on any error.
/// Warns to stderr if the file exists but fails to parse, so the user knows
/// their config is being ignored rather than silently resetting to defaults.
pub fn load_config() -> HomeboyConfig {
    match load_config_from_file() {
        Ok(config) => config,
        Err(err) => {
            // Only warn if the file actually exists — missing file is expected
            if config_exists() {
                log_status!(
                    "config",
                    "Warning: failed to load {} ({}), using defaults",
                    crate::core::product_identity::PRODUCT_IDENTITY.config_filename,
                    err.message
                );
            }
            HomeboyConfig::default()
        }
    }
}

/// Attempt to load config from the product config file.
fn load_config_from_file() -> crate::core::Result<HomeboyConfig> {
    let path = paths::homeboy_json()?;

    if !path.exists() {
        return Err(crate::core::Error::internal_io(
            format!(
                "{} not found",
                crate::core::product_identity::PRODUCT_IDENTITY.config_filename
            ),
            Some(path.display().to_string()),
        ));
    }

    let content = local_files::read_file(&path, &format!("read {}", path.display()))?;

    let config: HomeboyConfig = serde_json::from_str(&content).map_err(|e| {
        crate::core::Error::validation_invalid_json(
            e,
            Some(format!(
                "parse {}",
                crate::core::product_identity::PRODUCT_IDENTITY.config_filename
            )),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(config)
}

/// Save config to the product config file (creates if missing).
pub fn save_config(config: &HomeboyConfig) -> crate::core::Result<()> {
    let path = paths::homeboy_json()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::core::Error::internal_io(
                e.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }

    let content = crate::core::config::to_string_pretty(config)?;

    local_files::write_file_atomic(&path, &content, &format!("write {}", path.display()))?;

    Ok(())
}

/// Check if the product config file exists.
pub fn config_exists() -> bool {
    paths::homeboy_json().map(|p| p.exists()).unwrap_or(false)
}

/// Delete the product config file (reset to defaults).
pub fn reset_config() -> crate::core::Result<bool> {
    let path = paths::homeboy_json()?;

    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            crate::core::Error::internal_io(
                e.to_string(),
                Some(format!("delete {}", path.display())),
            )
        })?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get the product config path (for display purposes).
pub fn config_path() -> crate::core::Result<String> {
    Ok(paths::homeboy_json()?.display().to_string())
}

/// Get built-in defaults (ignoring any file config)
pub fn builtin_defaults() -> Defaults {
    Defaults::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homeboy_config_parses_triage_priority_labels() {
        let config: HomeboyConfig = serde_json::from_str(
            r#"{
                "triage": {
                    "priority_labels": ["security", "urgent"]
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.triage.priority_labels,
            Some(vec!["security".to_string(), "urgent".to_string()])
        );
    }

    #[test]
    fn homeboy_config_parses_lab_preferred_runner() {
        let config: HomeboyConfig = serde_json::from_str(
            r#"{
                "lab": {
                    "preferred_runner": "homeboy-lab"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(config.lab.preferred_runner.as_deref(), Some("homeboy-lab"));
    }

    #[test]
    fn homeboy_config_parses_agent_task_config_secret() {
        let config: HomeboyConfig = serde_json::from_str(
            r#"{
                "agent_task": {
                    "secrets": {
                        "TOKEN": {
                            "source": "config",
                            "value": "redacted-test-token"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let secret = config.agent_task.secrets.get("TOKEN").unwrap();
        assert_eq!(secret.source, "config");
        assert_eq!(secret.value.as_deref(), Some("redacted-test-token"));
    }

    #[test]
    fn homeboy_config_leaves_triage_priority_labels_unset_by_default() {
        let config = HomeboyConfig::default();

        assert!(config.triage.priority_labels.is_none());
    }

    #[test]
    fn homeboy_config_leaves_lab_preferred_runner_unset_by_default() {
        let config = HomeboyConfig::default();

        assert!(config.lab.preferred_runner.is_none());
    }
}
