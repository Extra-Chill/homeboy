use serde::{Deserialize, Serialize};

use homeboy_extension::ExtensionSourceUpdate;

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
        let secondary = homeboy_core::defaults::secondary_install_method_key();
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

/// Disposition of runner convergence for an upgrade, so structured output
/// records intent and outcome rather than leaving consumers to infer it from
/// empty runner arrays (#9842).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerConvergenceDisposition {
    /// Runner convergence was explicitly skipped (e.g. `--skip-runners`); no
    /// runner state was collected or claimed.
    Skipped,
    /// No runners are configured, so there was nothing to converge.
    NoRunnersConfigured,
    /// Every selected configured runner converged to the controller build.
    Converged,
    /// One or more selected runners did not converge.
    Partial,
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
    /// Immutable Git commit used for a source build, when the source checkout
    /// is a Git worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub upgraded: bool,
    /// True when a requested controller/runner fleet upgrade could not fully
    /// converge. Omitted for successful responses so existing consumers keep
    /// their current payload shape; accepted when reading persisted responses.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// Explicit runner-convergence disposition (skipped / none configured /
    /// converged / partial), so consumers never infer convergence from empty
    /// runner arrays. Omitted for older persisted responses (#9842).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_convergence: Option<RunnerConvergenceDisposition>,
    pub message: String,
    pub restart_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions_updated: Vec<ExtensionUpgradeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions_skipped: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners_updated: Vec<RunnerUpgradeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners_skipped: Vec<RunnerUpgradeEntry>,
    /// Symlinked extension clones owned by the invoking (sudo) user that this
    /// upgrade could not refresh because it ran under a different `$HOME`.
    /// Each entry carries the exact recovery command to bring the clone current.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions_unrefreshed: Vec<UnrefreshedExtensionWarning>,
    /// Long-running, binary-resident services (declared in config) that were
    /// successfully restarted to pick up the newly-swapped binary. Distinct
    /// from `restart_required`, which only describes the CLI process itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services_restarted: Vec<ServiceRestartEntry>,
    /// Declared resident services that still hold the old binary and need a
    /// restart: either because a restart attempt failed, or because the
    /// upgrade was run with `--no-restart-services`. Each entry carries the
    /// exact recovery command so the operator can restart it manually.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services_pending_restart: Vec<ServiceRestartEntry>,
}

/// Outcome of attempting to restart one declared binary-resident service after
/// an upgrade. Used both for successful restarts (`services_restarted`) and for
/// services that still need attention (`services_pending_restart`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRestartEntry {
    /// Configured service id.
    pub service_id: String,
    /// The restart command that was run (or would need to be run).
    pub restart_command: String,
    /// Whether the restart succeeded.
    pub restarted: bool,
    /// Failure or skip detail when `restarted` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_previous_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_new_version: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_response_without_new_fields_remains_deserializable() {
        let result: UpgradeResult = serde_json::from_str(
            r#"{"command":"upgrade","install_method":"binary","previous_version":"0.301.2","new_version":"0.304.0","upgraded":true,"message":"ok","restart_required":false}"#,
        )
        .expect("pre-convergence response remains readable");

        assert!(!result.partial);
        assert!(result.runners_updated.is_empty());
        assert!(result.services_pending_restart.is_empty());
    }
}

#[derive(Deserialize)]
pub(super) struct GitHubRelease {
    pub(super) tag_name: String,
}
