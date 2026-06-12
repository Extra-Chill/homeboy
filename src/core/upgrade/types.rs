use serde::{Deserialize, Serialize};

use crate::core::extension::ExtensionSourceUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Source,
    /// Downloaded release binary (e.g. ~/bin/homeboy, /usr/local/bin/homeboy)
    Binary,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionCheck {
    pub command: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub install_method: InstallMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct UpgradeResult {
    pub command: String,
    pub install_method: InstallMethod,
    pub previous_version: String,
    pub new_version: Option<String>,
    pub upgraded: bool,
    pub message: String,
    pub restart_required: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_updated: Vec<ExtensionUpgradeEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_skipped: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runners_updated: Vec<RunnerUpgradeEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runners_skipped: Vec<RunnerUpgradeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionUpgradeEntry {
    pub extension_id: String,
    pub old_version: String,
    pub new_version: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(flatten)]
    pub source_update: ExtensionSourceUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerUpgradeEntry {
    pub runner_id: String,
    pub homeboy_path: String,
    pub success: bool,
    pub upgraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bare_homeboy_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_drift: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovery_commands: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_synced: Vec<RunnerExtensionSyncEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_skipped: Vec<RunnerExtensionSyncEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_failed: Vec<RunnerExtensionSyncEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon: Option<RunnerDaemonDriftEntry>,
    pub exit_code: i32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExtensionSyncEntry {
    pub extension_id: String,
    pub source_revision: String,
    pub synced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovery_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerDaemonDriftEntry {
    pub session_homeboy_version: String,
    pub current_homeboy_version: String,
    pub recovery_commands: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct CratesIoResponse {
    #[serde(rename = "crate")]
    pub(super) crate_info: CrateInfo,
}

#[derive(Deserialize)]
pub(super) struct CrateInfo {
    pub(super) newest_version: String,
}

#[derive(Deserialize)]
pub(super) struct GitHubRelease {
    pub(super) tag_name: String,
}
