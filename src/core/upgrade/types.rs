use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionUpgradeEntry {
    pub extension_id: String,
    pub old_version: String,
    pub new_version: String,
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
