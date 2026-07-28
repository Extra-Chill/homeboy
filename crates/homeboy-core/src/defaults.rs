use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

mod builtins;
mod io;
mod policy;

pub use io::{config_exists, config_path, load_config, reset_config, save_config};
pub use policy::resolve_release_gate_local_hot_policy_from;
pub use policy::{
    resolve_release_gate_local_hot_policy, BenchConfig, BenchLocalExecutionPolicy,
    ReleaseGateConfig, ReleaseGateLocalHotPolicy, RELEASE_GATE_LOCAL_HOT_ENV,
};

#[cfg(any(test, feature = "test-support"))]
pub use io::reset_config_cache_for_test;

pub use builtins::deploy_generated_build_dir;
pub use builtins::extension_provided_direct_test_file_suffixes;
pub use builtins::extension_provided_test_drift_config;

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

    /// Notification delivery policy. Transports are discovered from installed
    /// extension manifests; this only selects the route-less operations default.
    #[serde(default)]
    pub notifications: NotificationConfig,

    /// Identity applied to commits homeboy creates on its own behalf, such as
    /// an unattended harvest recovery.
    #[serde(default)]
    pub automation: AutomationConfig,

    /// External worktree lifecycle providers keyed by provider id.
    ///
    /// Providers are command-backed integration points owned by the local
    /// environment. Homeboy only selects, gates, executes argv arrays, and
    /// captures structured output; provider-specific semantics live outside
    /// core.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub worktree_providers: HashMap<String, WorktreeProviderConfig>,

    /// Host-scoped environment for GitHub CLI subprocesses keyed by hostname.
    ///
    /// Values are applied whenever Homeboy runs `gh` for a repository whose
    /// remote URL resolves to that host. Component-level `github.hosts` entries
    /// override these global defaults.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub github_hosts: HashMap<String, crate::component::GithubHostConfig>,

    /// Expected repository-local Git commit identity keyed by remote hostname.
    ///
    /// Homeboy checks this policy immediately before publication mutations. The
    /// hostname comes from the repository's origin URL, keeping this provider-agnostic.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub git_hosts: HashMap<String, GitHostConfig>,

    /// Extension and executor settings addressed through `/settings/...`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, Value>,

    /// Release-gate routing safety policy.
    ///
    /// Controls whether release-gate hot commands (lint/test/audit) may be
    /// bypassed to local execution through explicit placement or a
    /// stale-runner local fallback when a default Lab runner is configured.
    /// See issues #4603 / #4605.
    #[serde(default)]
    pub release_gate: ReleaseGateConfig,

    /// Bounded retention policy shared by terminal run evidence and runtime
    /// resources. Individual commands may request a narrower scope.
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Directory where persisted run artifacts are copied.
    ///
    /// Defaults to the machine-local product data directory under
    /// `artifacts/`. Override with CLI, environment, or config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_root: Option<String>,

    /// Controller-owned public origin for canonical run artifact routes.
    ///
    /// Runner environment is not configuration for this origin. The legacy
    /// `HOMEBOY_PUBLIC_ARTIFACT_BASE_URL` is read only as a controller-process
    /// compatibility input when this setting is absent.
    #[serde(default)]
    pub artifact_origin: ArtifactOriginConfig,

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
            notifications: NotificationConfig::default(),
            automation: AutomationConfig::default(),
            worktree_providers: HashMap::new(),
            github_hosts: HashMap::new(),
            git_hosts: HashMap::new(),
            settings: HashMap::new(),
            release_gate: ReleaseGateConfig::default(),
            retention: RetentionConfig::default(),
            artifact_root: None,
            artifact_origin: ArtifactOriginConfig::default(),
            update_check: true,
            resident_services: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ArtifactOriginConfig {
    /// Public HTTPS base URL owned by this controller, for example
    /// `https://artifacts.example.test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GitHostConfig {
    pub name: String,
    pub email: String,
}

/// Safe default retention windows for resources created by Homeboy commands.
///
/// This is deliberately resource-oriented: controller scratch ownership has a
/// separate lifecycle contract and is not inferred from these paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionConfig {
    #[serde(default = "default_terminal_run_retention_days")]
    pub terminal_run_days: i64,
    #[serde(default = "default_runtime_tmp_retention_days")]
    pub runtime_tmp_days: u64,
    #[serde(default = "default_runtime_run_max_bytes")]
    pub runtime_run_max_bytes: u64,
    #[serde(default = "default_runtime_run_max_count")]
    pub runtime_run_max_count: usize,
    #[serde(default = "default_controller_runtime_retention_days")]
    pub controller_runtime_days: u64,
    #[serde(default = "default_controller_runtime_max_bytes")]
    pub controller_runtime_max_bytes: u64,
    #[serde(default = "default_retention_limit")]
    pub limit: i64,
    #[serde(default = "default_shared_store_retention_days")]
    pub shared_store_days: u64,
    #[serde(default = "default_shared_store_max_bytes")]
    pub shared_store_max_bytes: u64,
    #[serde(default = "default_shared_store_lease_seconds")]
    pub shared_store_lease_seconds: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            terminal_run_days: default_terminal_run_retention_days(),
            runtime_tmp_days: default_runtime_tmp_retention_days(),
            runtime_run_max_bytes: default_runtime_run_max_bytes(),
            runtime_run_max_count: default_runtime_run_max_count(),
            controller_runtime_days: default_controller_runtime_retention_days(),
            controller_runtime_max_bytes: default_controller_runtime_max_bytes(),
            limit: default_retention_limit(),
            shared_store_days: default_shared_store_retention_days(),
            shared_store_max_bytes: default_shared_store_max_bytes(),
            shared_store_lease_seconds: default_shared_store_lease_seconds(),
        }
    }
}

fn default_terminal_run_retention_days() -> i64 {
    30
}

fn default_runtime_tmp_retention_days() -> u64 {
    7
}

fn default_runtime_run_max_bytes() -> u64 {
    1024 * 1024 * 1024
}

fn default_runtime_run_max_count() -> usize {
    100
}

fn default_controller_runtime_retention_days() -> u64 {
    30
}

fn default_controller_runtime_max_bytes() -> u64 {
    2 * 1024 * 1024 * 1024
}

fn default_retention_limit() -> i64 {
    1000
}

fn default_shared_store_retention_days() -> u64 {
    30
}

fn default_shared_store_max_bytes() -> u64 {
    20 * 1024 * 1024 * 1024
}

fn default_shared_store_lease_seconds() -> u64 {
    6 * 60 * 60
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationConfig {
    /// Installed extension transport ID used only when a completed operation has
    /// no route bound to its persisted run record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_transport: Option<String>,
}

/// Identity used for commits homeboy authors itself.
///
/// A one-off recovery can pass `--author`, but an unattended harvest invents an
/// identity at the call site on every run, so nothing records what that string
/// is supposed to mean. Configuring it once makes automated commits
/// attributable to a real, intentional actor (#10221).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationConfig {
    /// `Name <email>` applied to automated commits when no author is passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub kind: WorktreeProviderKind,
    #[serde(default)]
    pub apply_enabled: bool,
    #[serde(default)]
    pub commands: WorktreeProviderCommands,
    /// Maximum time allowed for a provider's read-only `list` or `resolve`
    /// command. This is intentionally separate from mutating provider actions.
    #[serde(
        default = "default_worktree_provider_lookup_timeout_ms",
        deserialize_with = "deserialize_worktree_provider_lookup_timeout_ms"
    )]
    pub lookup_timeout_ms: u64,
    /// Maximum output retained from a provider's read-only `list` or `resolve`
    /// command. The configured result mapping receives the complete JSON payload.
    #[serde(
        default = "default_worktree_provider_lookup_output_limit_bytes",
        deserialize_with = "deserialize_worktree_provider_lookup_output_limit_bytes"
    )]
    pub lookup_output_limit_bytes: usize,
    /// Explicit projection of a command provider's list result into Homeboy's
    /// generic worktree safety contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_result_mapping: Option<WorktreeProviderListResultMapping>,
}

const DEFAULT_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS: u64 = 10_000;
const MIN_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS: u64 = 1;
const MAX_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const MIN_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES: usize = 1;
const MAX_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES: usize = 64 * 1024 * 1024;

fn default_worktree_provider_lookup_timeout_ms() -> u64 {
    DEFAULT_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS
}

fn default_worktree_provider_lookup_output_limit_bytes() -> usize {
    DEFAULT_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES
}

fn deserialize_worktree_provider_lookup_timeout_ms<'de, D>(
    deserializer: D,
) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let timeout_ms = u64::deserialize(deserializer)?;
    validate_worktree_provider_lookup_timeout_ms(timeout_ms).map_err(serde::de::Error::custom)?;
    Ok(timeout_ms)
}

pub fn validate_worktree_provider_lookup_timeout_ms(
    timeout_ms: u64,
) -> std::result::Result<(), String> {
    if (MIN_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS..=MAX_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS)
        .contains(&timeout_ms)
    {
        Ok(())
    } else {
        Err(format!(
            "lookup_timeout_ms must be between {MIN_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS} and {MAX_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS} milliseconds"
        ))
    }
}

fn deserialize_worktree_provider_lookup_output_limit_bytes<'de, D>(
    deserializer: D,
) -> std::result::Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let output_limit_bytes = usize::deserialize(deserializer)?;
    validate_worktree_provider_lookup_output_limit_bytes(output_limit_bytes)
        .map_err(serde::de::Error::custom)?;
    Ok(output_limit_bytes)
}

pub fn validate_worktree_provider_lookup_output_limit_bytes(
    output_limit_bytes: usize,
) -> std::result::Result<(), String> {
    if (MIN_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES
        ..=MAX_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES)
        .contains(&output_limit_bytes)
    {
        Ok(())
    } else {
        Err(format!(
            "lookup_output_limit_bytes must be between {MIN_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES} and {MAX_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES} bytes"
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderListResultMapping {
    /// JSONPath resolving to exactly one array in the command result.
    pub items: String,
    /// JSONPath values resolved relative to each item in `items`.
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub dirty: String,
    pub unpushed: String,
    pub primary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderKind {
    Command,
}

impl Default for WorktreeProviderKind {
    fn default() -> Self {
        Self::Command
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderCommands {
    /// Targeted handle lookup. Each `{handle}` argument is replaced with the
    /// requested handle. The result uses `list_result_mapping` and may contain one item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve: Option<Vec<String>>,
    /// Provider-native resolve exit statuses that mean the requested handle is
    /// absent. All other non-zero statuses remain lookup failures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolve_not_found_exit_codes: Vec<i32>,
    /// Discovery command and compatibility fallback for providers without
    /// `resolve`. Exact handle lookups prefer `resolve` when it is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<Vec<String>>,
    /// Atomically return an existing managed worktree or create it. Homeboy
    /// expands `{handle}`, `{repo}`, `{base}`, `{head}`, `{task_url}`, and a
    /// stable `{idempotency_key}` from an explicit Cook destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensure: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_preview: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_apply: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_preview: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_apply: Option<Vec<String>>,
}

pub fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LabConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_workspace_ttl: Option<String>,
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
    /// Global provider rotation policy for agent-task dispatch, settable via
    /// `homeboy config set /agent_task/rotation <json> --json`. A per-plan
    /// `options.rotation` or per-task `metadata.provider_rotation` override
    /// takes precedence (#6978).
    ///
    /// Carried opaquely as JSON so core config does not depend on the agent-task
    /// subsystem; the agent-task dispatch layer deserializes it into its
    /// `AgentTaskProviderRotationPolicy` when building a plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<serde_json::Value>,
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

/// Known ecosystem key used as a secondary install-method discriminant in
/// extension-provided defaults. Pending externalization into the defaults asset;
/// allowed by `core-agnostic-source` audit allowlist (tech-debt burndown Wave A, #18).
pub fn secondary_install_method_key() -> String {
    "cargo".to_string() // audit-allow-cargo-secondary-key
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
    fn retention_shared_store_fields_deserialize_with_documented_defaults() {
        let config: HomeboyConfig = serde_json::from_str(r#"{"retention":{}}"#).unwrap();

        assert_eq!(config.retention.shared_store_days, 30);
        assert_eq!(
            config.retention.shared_store_max_bytes,
            20 * 1024 * 1024 * 1024
        );
        assert_eq!(config.retention.shared_store_lease_seconds, 6 * 60 * 60);
    }

    #[test]
    fn worktree_provider_lookup_timeout_defaults_and_validates() {
        let config: HomeboyConfig = serde_json::from_str(
            r#"{"worktree_providers":{"fixture":{"commands":{"list":["provider"]}}}}"#,
        )
        .expect("existing provider config remains valid");
        assert_eq!(
            config.worktree_providers["fixture"].lookup_timeout_ms,
            DEFAULT_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS
        );
        assert_eq!(
            config.worktree_providers["fixture"].lookup_output_limit_bytes,
            DEFAULT_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES
        );
        assert_eq!(
            serde_json::to_value(&config)
                .expect("config serializes")
                .pointer("/worktree_providers/fixture/lookup_timeout_ms"),
            Some(&serde_json::json!(
                DEFAULT_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS
            ))
        );
        assert_eq!(
            serde_json::to_value(&config)
                .expect("config serializes")
                .pointer("/worktree_providers/fixture/lookup_output_limit_bytes"),
            Some(&serde_json::json!(
                DEFAULT_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES
            ))
        );

        for timeout_ms in [0, MAX_WORKTREE_PROVIDER_LOOKUP_TIMEOUT_MS + 1] {
            let error = serde_json::from_value::<HomeboyConfig>(serde_json::json!({
                "worktree_providers": {"fixture": {"lookup_timeout_ms": timeout_ms}}
            }))
            .expect_err("out-of-range lookup timeout is invalid");
            assert!(error
                .to_string()
                .contains("lookup_timeout_ms must be between"));
        }

        for output_limit_bytes in [0, MAX_WORKTREE_PROVIDER_LOOKUP_OUTPUT_LIMIT_BYTES + 1] {
            let error = serde_json::from_value::<HomeboyConfig>(serde_json::json!({
                "worktree_providers": {"fixture": {"lookup_output_limit_bytes": output_limit_bytes}}
            }))
            .expect_err("out-of-range lookup output limit is invalid");
            assert!(error
                .to_string()
                .contains("lookup_output_limit_bytes must be between"));
        }
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
    fn load_config_cache_refreshes_after_save_config() {
        with_isolated_home(|_| {
            let cached_default = load_config();
            assert!(!cached_default.settings.contains_key("cache_marker"));

            save_config(&HomeboyConfig {
                settings: HashMap::from([("cache_marker".to_string(), serde_json::json!("saved"))]),
                ..HomeboyConfig::default()
            })
            .expect("save config");

            let loaded = load_config();
            assert_eq!(loaded.settings["cache_marker"], "saved");
        });
    }

    #[test]
    fn reset_config_clears_cached_config() {
        with_isolated_home(|_| {
            save_config(&HomeboyConfig {
                settings: HashMap::from([(
                    "cache_marker".to_string(),
                    serde_json::json!("before-reset"),
                )]),
                ..HomeboyConfig::default()
            })
            .expect("save config");

            let cached = load_config();
            assert_eq!(cached.settings["cache_marker"], "before-reset");

            assert!(reset_config().expect("reset config"));

            let loaded = load_config();
            assert!(!loaded.settings.contains_key("cache_marker"));
        });
    }

    #[test]
    fn isolated_home_guard_resets_config_cache_between_homes() {
        with_isolated_home(|_| {
            save_config(&HomeboyConfig {
                settings: HashMap::from([(
                    "cache_marker".to_string(),
                    serde_json::json!("first-home"),
                )]),
                ..HomeboyConfig::default()
            })
            .expect("save first home config");

            let loaded = load_config();
            assert_eq!(loaded.settings["cache_marker"], "first-home");
        });

        with_isolated_home(|_| {
            let loaded = load_config();
            assert!(!loaded.settings.contains_key("cache_marker"));
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
