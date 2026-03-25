//! types — extracted from defaults.rs.

use serde::{Deserialize, Serialize};


/// Configuration for install method detection and upgrade commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallMethodsConfig {
    #[serde(default = "default_homebrew_config")]
    pub homebrew: InstallMethodConfig,

    #[serde(default = "default_cargo_config")]
    pub cargo: InstallMethodConfig,

    #[serde(default = "default_source_config")]
    pub source: InstallMethodConfig,

    #[serde(default = "default_binary_config")]
    pub binary: InstallMethodConfig,
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
    #[serde(default = "default_scp_flags")]
    pub scp_flags: Vec<String>,

    #[serde(default = "default_artifact_prefix")]
    pub artifact_prefix: String,

    #[serde(default = "default_ssh_port")]
    pub default_ssh_port: u16,
}

/// Configuration for file permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default = "default_local_permissions")]
    pub local: PermissionModes,

    #[serde(default = "default_remote_permissions")]
    pub remote: PermissionModes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionModes {
    pub file_mode: String,
    pub dir_mode: String,
}
