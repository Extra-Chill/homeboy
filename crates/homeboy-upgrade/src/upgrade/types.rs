use serde::{Deserialize, Serialize};

use homeboy_core::extension::ExtensionSourceUpdate;

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
    /// The release an upgrade would actually install. For asset-installed
    /// methods this is the newest release carrying an artifact for
    /// [`Self::target`] — not merely the newest tag, which may be
    /// uninstallable here (#11750). `None` when the check could not reach
    /// GitHub, or when no published release ships this target's artifact.
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub install_method: InstallMethod,
    /// Running target triple, or `None` when the OS/architecture pair is not
    /// one Homeboy publishes assets for. `None` means asset availability was
    /// *not* verified, which is a different claim from "no asset exists".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Newest published release, whatever its asset inventory. Equal to
    /// `latest_version` on a healthy release train; they diverge exactly when
    /// the newest release cannot be installed on this target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_version: Option<String>,
    /// Releases newer than `latest_version` that were passed over because they
    /// ship no artifact for `target`, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uninstallable_versions: Vec<String>,
    /// Plain-language explanation when `latest_version` is not the newest
    /// release, or when nothing installable was found at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
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
    /// Machine-readable outcome for controller and runner ordering semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Read-only admission findings that prevented controller mutation. Kept
    /// bounded to one entry and recovery command per extension blocker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<UpgradePreflight>,
    /// Independent controller mutation outcome. Additive so persisted and
    /// older callers can continue using `upgraded` and `partial` unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<UpgradeComponentStatus>,
    /// Independent extension refresh outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<UpgradeComponentStatus>,
    /// Independent configured-runner convergence outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runners: Option<UpgradeComponentStatus>,
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
    /// Per-extension reason for each entry in `extensions_skipped`, alongside
    /// the bare ids, so a `partial` extension outcome carries *why* the
    /// extension was skipped instead of burying the reason in a log line
    /// (#12181).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_skips: Vec<ExtensionUpgradeSkip>,
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
    /// Durable observation-run id for this upgrade. Inspect with
    /// `homeboy upgrade status <id>` after a client timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// Compact named outcome for one upgrade dimension. Detailed build output stays
/// in the existing per-runner evidence fields rather than obscuring the result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeComponentStatus {
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradePreflight {
    pub candidate_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_blockers: Vec<ExtensionPreflightBlocker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionPreflightBlocker {
    pub extension_id: String,
    pub classification: String,
    pub detail: String,
    pub recovery_command: String,
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

/// Why one extension was skipped during the upgrade, mirroring the per-runner
/// `RunnerExtensionSyncEntry` detail pattern so a `partial` extension outcome
/// carries its failure reason in the structured result rather than only in a
/// log line (#12181).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionUpgradeSkip {
    /// Extension id (e.g. `wordpress`).
    pub extension_id: String,
    /// The error message for why the extension was skipped.
    pub reason: String,
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
    /// True when the clone holds uncommitted changes (ignoring the generated
    /// `.source-url`/`.source-revision` metadata) that would make the recovery
    /// command fail. Lets a machine consumer distinguish "stale but
    /// refreshable" from "stale and blocked". Omitted (`false`) for clean
    /// clones so existing consumers and fixtures keep their shape (#12181).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
    /// Repo-relative paths with the uncommitted changes blocking refresh. Only
    /// populated when `dirty`, so it is omitted for clean clones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirty_paths: Vec<String>,
    /// The exact command the user should run to bring the clone current. When
    /// the clone is clean this is `git pull --ff-only`; a dirty clone cannot be
    /// refreshed by any single command, so this names a read-only status
    /// inspection and `dirty_paths` names what the user must resolve first.
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
        assert!(result.extension_skips.is_empty());
    }

    #[test]
    fn extension_skip_reason_reaches_serialized_result() {
        let mut result = UpgradeResult {
            command: "upgrade".to_string(),
            install_method: InstallMethod::Binary,
            previous_version: "0.327.5".to_string(),
            new_version: Some("0.338.0".to_string()),
            previous_build_identity: None,
            new_build_identity: None,
            source_revision: None,
            upgraded: true,
            outcome: Some("controller_updated".to_string()),
            preflight: None,
            controller: Some(UpgradeComponentStatus {
                status: "updated".to_string(),
                summary: "controller installation completed".to_string(),
            }),
            extensions: Some(UpgradeComponentStatus {
                status: "partial".to_string(),
                summary: "0 updated, 1 skipped (wordpress: Linked extension source repo has uncommitted changes)".to_string(),
            }),
            runners: None,
            partial: false,
            runner_convergence: None,
            message: "Controller upgraded to 0.338.0".to_string(),
            restart_required: false,
            extensions_updated: Vec::new(),
            extensions_skipped: vec!["wordpress".to_string()],
            extension_skips: vec![ExtensionUpgradeSkip {
                extension_id: "wordpress".to_string(),
                reason: "Linked extension source repo has uncommitted changes".to_string(),
            }],
            runners_updated: Vec::new(),
            runners_skipped: Vec::new(),
            extensions_unrefreshed: Vec::new(),
            services_restarted: Vec::new(),
            services_pending_restart: Vec::new(),
            operation_id: None,
        };

        let json = serde_json::to_value(&result).expect("upgrade result serializes");
        assert_eq!(json["extension_skips"][0]["extension_id"], "wordpress");
        assert_eq!(
            json["extension_skips"][0]["reason"],
            "Linked extension source repo has uncommitted changes"
        );

        result.extension_skips = Vec::new();
        let json = serde_json::to_value(&result).expect("upgrade result serializes");
        assert!(
            json.get("extension_skips").is_none(),
            "empty extension_skips is omitted for existing consumers"
        );
    }

    #[test]
    fn unrefreshed_warning_omits_dirty_fields_when_clean() {
        let warning = UnrefreshedExtensionWarning {
            extension_id: "wordpress".to_string(),
            invoking_user: "opencode".to_string(),
            symlink_path: "/home/opencode/.config/homeboy/extensions/wordpress".to_string(),
            source_path: "/home/opencode/homeboy-extensions/source/wordpress".to_string(),
            behind: Some(145),
            dirty: false,
            dirty_paths: Vec::new(),
            recovery_command: "sudo -u opencode git -C /home/opencode/homeboy-extensions/source/wordpress pull --ff-only".to_string(),
        };

        let json = serde_json::to_value(&warning).expect("warning serializes");
        assert!(json.get("dirty").is_none(), "{json}");
        assert!(json.get("dirty_paths").is_none(), "{json}");
        assert_eq!(
            json["recovery_command"],
            "sudo -u opencode git -C /home/opencode/homeboy-extensions/source/wordpress pull --ff-only"
        );

        let dirty = UnrefreshedExtensionWarning {
            dirty: true,
            dirty_paths: vec!["package-lock.json".to_string()],
            ..warning
        };
        let json = serde_json::to_value(&dirty).expect("warning serializes");
        assert_eq!(json["dirty"], true);
        assert_eq!(json["dirty_paths"][0], "package-lock.json");
    }
}

#[derive(Deserialize)]
pub(super) struct GitHubRelease {
    pub(super) tag_name: String,
    #[serde(default)]
    pub(super) body: String,
}
