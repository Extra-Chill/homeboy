use serde::{Deserialize, Serialize};

use crate::core::extension::ExtensionSourceUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Secondary,
    Source,
    /// Downloaded release binary (e.g. ~/bin/homeboy, /usr/local/bin/homeboy)
    Binary,
    Unknown,
}

impl Serialize for InstallMethod {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for InstallMethod {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let secondary = crate::core::defaults::secondary_install_method_key();
        match value.as_str() {
            "homebrew" => Ok(Self::Homebrew),
            "source" => Ok(Self::Source),
            "binary" => Ok(Self::Binary),
            "unknown" => Ok(Self::Unknown),
            other if other == secondary => Ok(Self::Secondary),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["homebrew", "source", "binary", "unknown"],
            )),
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_build_identity: Option<String>,
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
    /// Symlinked extension clones owned by the invoking (sudo) user that this
    /// upgrade could not refresh because it ran under a different `$HOME`.
    /// Each entry carries the exact recovery command to bring the clone current.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_unrefreshed: Vec<UnrefreshedExtensionWarning>,
}

/// A symlinked extension in the invoking user's config dir that a privileged
/// (sudo) upgrade left stale, because extension resolution is `$HOME`-scoped
/// and the privileged run only ever sees root's own extension copies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnrefreshedExtensionWarning {
    /// Extension id (e.g. `example-extension`).
    pub extension_id: String,
    /// The invoking user (value of `SUDO_USER`).
    pub invoking_user: String,
    /// The symlink path in the invoking user's config dir.
    pub symlink_path: String,
    /// The resolved git working tree the symlink points at.
    pub source_path: String,
    /// How many commits the clone is behind its upstream, if determinable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    /// The exact command the user should run to refresh the clone.
    pub recovery_command: String,
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
