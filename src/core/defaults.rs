use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{OnceLock, RwLock};

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

    /// Extension and executor settings addressed through `/settings/...`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, Value>,

    /// Release-gate routing safety policy.
    ///
    /// Controls whether release-gate hot commands (lint/test/audit) may be
    /// bypassed to local execution via `--force-hot --allow-local-hot` or a
    /// stale-runner local fallback when a default Lab runner is configured.
    /// See issues #4603 / #4605.
    #[serde(default)]
    pub release_gate: ReleaseGateConfig,

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

    /// Long-running services that keep an in-memory copy of the Homeboy binary
    /// resident and therefore must be restarted after `homeboy upgrade` swaps
    /// the on-disk binary. These are declared per host/environment in config —
    /// core ships none by default and hardcodes no service name, unit, or host.
    ///
    /// `homeboy upgrade` restarts each declared service after a successful
    /// binary swap (unless `--no-restart-services` is passed) and reports the
    /// outcome via `services_restarted` / `services_pending_restart`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resident_services: Vec<ResidentServiceConfig>,
}

/// A long-running, binary-resident service that must be restarted to pick up a
/// newly-swapped Homeboy binary.
///
/// Intentionally generic and config-driven: a descriptor either names a
/// `systemd_unit` (restarted with `systemctl restart <unit>`) or supplies an
/// explicit `restart_command` shell line. No service name, unit, or host is
/// hardcoded in core — every value comes from the host's own config, keeping
/// the upgrade flow org/host-agnostic (see #5118).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentServiceConfig {
    /// Stable identifier for the service, used in upgrade result reporting.
    pub id: String,

    /// systemd unit name (e.g. `homeboy-preview-ingress`). When set and no
    /// `restart_command` is given, the service is restarted with
    /// `systemctl restart <unit>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub systemd_unit: Option<String>,

    /// Explicit restart command (shell line) overriding the systemd default.
    /// Use this for non-systemd supervisors or custom restart logic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_command: Option<String>,
}

impl Default for HomeboyConfig {
    fn default() -> Self {
        Self {
            defaults: Defaults::default(),
            bench: BenchConfig::default(),
            lab: LabConfig::default(),
            triage: TriageConfig::default(),
            agent_task: AgentTaskConfig::default(),
            settings: HashMap::new(),
            release_gate: ReleaseGateConfig::default(),
            artifact_root: None,
            update_check: true,
            resident_services: Vec::new(),
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

/// Release-gate routing safety policy.
///
/// Release gates are the quality-check hot commands (lint/test/audit) whose
/// routing fidelity matters for validating a release. When a default Lab
/// runner is configured, silently bypassing Lab routing to run these gates
/// locally (via `--force-hot --allow-local-hot` or a stale-runner fallback)
/// produces a gate result that is not faithful to the configured runner
/// policy. This config makes such bypasses fail closed with a clear
/// diagnostic instead of silently executing locally. See issues #4603 / #4605.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReleaseGateConfig {
    /// Whether release-gate hot commands may be bypassed to local execution
    /// when a default Lab runner is configured.
    ///
    /// - `fail_closed` (default): the bypass is rejected with a diagnostic.
    /// - `allowed`: the bypass runs locally and is recorded in the offload
    ///   metadata (the operator-only override).
    #[serde(default)]
    pub local_hot: ReleaseGateLocalHotPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ReleaseGateLocalHotPolicy {
    /// Reject force-local bypass and stale-runner local fallback for release
    /// gates when a default Lab runner is configured.
    #[default]
    FailClosed,
    /// Allow release gates to run locally; recorded in offload metadata.
    Allowed,
}

impl ReleaseGateLocalHotPolicy {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Environment variable override for `/release_gate/local_hot`.
///
/// Takes precedence over the config file so operators can re-enable a
/// local-hot bypass for a single invocation without editing config. This is
/// the explicit operator-only override: it must be set in the environment, not
/// via a convenience CLI flag, so it cannot become a habit bypass.
pub const RELEASE_GATE_LOCAL_HOT_ENV: &str = "HOMEBOY_RELEASE_GATE_LOCAL_HOT";

/// Resolve the effective release-gate local-hot policy from the environment
/// override (precedence) then the config file, falling back to the default.
pub fn resolve_release_gate_local_hot_policy() -> ReleaseGateLocalHotPolicy {
    resolve_release_gate_local_hot_policy_from(&load_config())
}

pub(crate) fn resolve_release_gate_local_hot_policy_from(
    config: &HomeboyConfig,
) -> ReleaseGateLocalHotPolicy {
    if let Ok(raw) = std::env::var(RELEASE_GATE_LOCAL_HOT_ENV) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allowed" | "allow" | "true" | "1" => return ReleaseGateLocalHotPolicy::Allowed,
            "fail_closed" | "fail-closed" | "denied" | "false" | "0" => {
                return ReleaseGateLocalHotPolicy::FailClosed;
            }
            _ => {}
        }
    }
    config.release_gate.local_hot
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
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
    pub path: Option<String>,
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

pub fn secondary_install_method_key() -> String {
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
    if let Some(config) = cached_config() {
        return config;
    }

    let config = load_config_uncached();
    store_cached_config(&config);
    config
}

fn load_config_uncached() -> HomeboyConfig {
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

fn config_cache() -> &'static RwLock<Option<HomeboyConfig>> {
    static CONFIG: OnceLock<RwLock<Option<HomeboyConfig>>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(None))
}

fn cached_config() -> Option<HomeboyConfig> {
    match config_cache().read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn store_cached_config(config: &HomeboyConfig) {
    match config_cache().write() {
        Ok(mut slot) => *slot = Some(config.clone()),
        Err(poisoned) => *poisoned.into_inner() = Some(config.clone()),
    }
}

fn clear_config_cache() {
    match config_cache().write() {
        Ok(mut slot) => *slot = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

#[cfg(test)]
pub(crate) fn reset_config_cache_for_test() {
    clear_config_cache();
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
    store_cached_config(config);

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
        clear_config_cache();
        Ok(true)
    } else {
        clear_config_cache();
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
    use crate::test_support::with_isolated_home;

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
                    "default_backend": "example",
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

        assert_eq!(
            config.agent_task.default_backend.as_deref(),
            Some("example")
        );
        let secret = config.agent_task.secrets.get("TOKEN").unwrap();
        assert_eq!(secret.source, "config");
        assert_eq!(secret.value.as_deref(), Some("redacted-test-token"));
    }

    #[test]
    fn homeboy_config_preserves_global_settings() {
        let config: HomeboyConfig = serde_json::from_str(
            r#"{
                "settings": {
                    "provider": "example",
                    "provider_plugin_paths": ["/providers/openai"],
                    "runtime_overlays": [{"repo":"owner/runtime","ref":"main"}]
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.settings.get("provider"),
            Some(&Value::String("example".to_string()))
        );
        assert_eq!(
            config.settings["provider_plugin_paths"][0],
            Value::String("/providers/openai".to_string())
        );
        assert_eq!(
            config.settings["runtime_overlays"][0]["repo"],
            "owner/runtime"
        );
    }

    #[test]
    fn homeboy_config_save_load_preserves_global_settings() {
        with_isolated_home(|_| {
            save_config(&HomeboyConfig {
                settings: HashMap::from([
                    ("provider".to_string(), serde_json::json!("example")),
                    (
                        "provider_plugin_paths".to_string(),
                        serde_json::json!(["/providers/openai"]),
                    ),
                    (
                        "runtime_overlays".to_string(),
                        serde_json::json!([{ "repo": "owner/runtime", "ref": "main" }]),
                    ),
                ]),
                ..HomeboyConfig::default()
            })
            .expect("save config");

            let loaded = load_config();

            assert_eq!(loaded.settings["provider"], "example");
            assert_eq!(
                loaded.settings["provider_plugin_paths"][0],
                "/providers/openai"
            );
            assert_eq!(loaded.settings["runtime_overlays"][0]["ref"], "main");
        });
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

    #[test]
    fn release_gate_local_hot_defaults_to_fail_closed() {
        let config = HomeboyConfig::default();

        assert_eq!(
            config.release_gate.local_hot,
            ReleaseGateLocalHotPolicy::FailClosed
        );
    }

    #[test]
    fn release_gate_local_hot_parses_allowed_from_config() {
        let config: HomeboyConfig =
            serde_json::from_str(r#"{"release_gate":{"local_hot":"allowed"}}"#).unwrap();

        assert_eq!(
            config.release_gate.local_hot,
            ReleaseGateLocalHotPolicy::Allowed
        );
    }

    #[test]
    fn resolve_release_gate_policy_env_overrides_config() {
        struct EnvGuard {
            previous: Option<String>,
        }
        impl EnvGuard {
            fn set(value: &str) -> Self {
                let previous = std::env::var(RELEASE_GATE_LOCAL_HOT_ENV).ok();
                std::env::set_var(RELEASE_GATE_LOCAL_HOT_ENV, value);
                Self { previous }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => std::env::set_var(RELEASE_GATE_LOCAL_HOT_ENV, value),
                    None => std::env::remove_var(RELEASE_GATE_LOCAL_HOT_ENV),
                }
            }
        }

        let _env = EnvGuard::set("allowed");
        assert_eq!(
            resolve_release_gate_local_hot_policy_from(&HomeboyConfig::default()),
            ReleaseGateLocalHotPolicy::Allowed
        );

        let _env = EnvGuard::set("fail-closed");
        assert_eq!(
            resolve_release_gate_local_hot_policy_from(&HomeboyConfig::default()),
            ReleaseGateLocalHotPolicy::FailClosed
        );
    }
}
