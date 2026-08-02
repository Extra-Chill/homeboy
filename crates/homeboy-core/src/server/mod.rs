pub mod api;
pub mod auth;
pub mod auth_profiles;
pub mod client;
mod connection;
pub mod health;
pub mod http;
mod keys;
mod process_cleanup;
mod session;
pub mod ssh_args;
pub mod transfer;

pub use client::DELEGATED_RUN_STATUS_FILE_ENV;
pub use client::{
    execute_local_command, execute_local_command_in_dir, execute_local_command_in_dir_with_timeout,
    execute_local_command_interactive, execute_local_command_passthrough, is_transient_ssh_error,
    CommandOutput, SshClient, CHILD_SECRET_ENV_NAMES_ENV, TRANSIENT_SSH_STDERR_PATTERNS,
};
pub use client::{
    execute_local_command_passthrough_with_timeout, execute_local_command_stderr_passthrough,
    execute_local_command_stderr_passthrough_with_timeout,
};
pub use connection::{resolve_context, SshResolveArgs, SshResolveResult};
pub use keys::{
    generate_key, get_public_key, import_key, unset_key, use_key, KeyGenerateResult,
    KeyImportResult,
};
pub use session::{ManagedSshSession, ManagedSshSessionOutput};

use std::collections::HashMap;

use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::paths;
use crate::project;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct Server {
    #[serde(skip_deserializing, default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ServerAuth>,
    /// Environment variables to set before executing commands on this server.
    /// Values support `$PATH`-style expansion — the shell handles it.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<ServerRunner>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homeboy_path: Option<String>,
    #[serde(default)]
    pub daemon: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_limit: Option<usize>,
    /// Bounds a child which emits only runner-wrapper heartbeats. Zero disables
    /// this safeguard for workloads whose semantic progress cannot be observed.
    #[serde(default, skip_serializing_if = "HeartbeatOnlyStallPolicy::is_default")]
    pub heartbeat_only_stall: HeartbeatOnlyStallPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_policy: Option<String>,
    /// When `true`, the Lab offload controller↔runner version gate requires a
    /// byte-identical Homeboy version on the runner. Default (unset/`false`) is
    /// compatibility-aware: patch drift within the same MAJOR.MINOR is allowed
    /// with a warning, and only MAJOR/MINOR drift hard-refuses. The
    /// `HOMEBOY_REQUIRE_EXACT_RUNNER_VERSION` env var forces strict mode for a
    /// single run regardless of this setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_exact_homeboy_version: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatOnlyStallPolicy {
    #[serde(default = "default_heartbeat_only_stall_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_heartbeat_only_stall_timeout_seconds() -> u64 {
    15 * 60
}

impl Default for HeartbeatOnlyStallPolicy {
    fn default() -> Self {
        Self {
            timeout_seconds: default_heartbeat_only_stall_timeout_seconds(),
        }
    }
}

impl HeartbeatOnlyStallPolicy {
    pub fn timeout(&self) -> Option<std::time::Duration> {
        (self.timeout_seconds > 0).then(|| std::time::Duration::from_secs(self.timeout_seconds))
    }

    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSecretEnvRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_peer_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_peer_fingerprints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_projects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_raw_exec: Option<bool>,
    /// Explicitly permits controller-driven Homeboy binary convergence. This
    /// is separate from arbitrary command execution and defaults to deny.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_homeboy_convergence: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_extensions: Vec<String>,
}

/// Secret-env + policy security configuration shared by runner-shaped structs.
/// Flattened so the on-wire JSON (`secret_env`, `policy`, each with its
/// `skip_serializing_if`) is identical to the previously inlined fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSecurityConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secret_env: HashMap<String, RunnerSecretEnvRef>,
    #[serde(default, skip_serializing_if = "RunnerPolicy::is_empty")]
    pub policy: RunnerPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerRunner {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(flatten)]
    pub settings: RunnerSettings,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resources: HashMap<String, serde_json::Value>,
    #[serde(flatten)]
    pub security: RunnerSecurityConfig,
}

impl RunnerPolicy {
    pub fn is_empty(&self) -> bool {
        self.accepted_peer_ids.is_empty()
            && self.accepted_peer_fingerprints.is_empty()
            && self.allowed_projects.is_empty()
            && self.allowed_commands.is_empty()
            && self.allow_raw_exec.is_none()
            && self.allow_homeboy_convergence.is_none()
            && self.workspace_roots.is_empty()
            && self.artifact_policy.is_none()
            && self.snapshot_excludes.is_empty()
            && self.snapshot_includes.is_empty()
            && self.supported_extensions.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerAuth {
    pub mode: ServerAuthMode,
    #[serde(flatten)]
    pub session: ServerSessionConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerSessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerAuthMode {
    KeyPlusPasswordControlmaster,
}

fn default_port() -> u16 {
    22
}

impl Server {
    pub fn is_valid(&self) -> bool {
        !self.host.is_empty() && !self.user.is_empty()
    }
}

pub fn validate_runner_settings(
    settings: &RunnerSettings,
    concurrency_field: &str,
    id: Option<String>,
) -> Result<()> {
    if settings.concurrency_limit == Some(0) {
        return Err(Error::validation_invalid_argument(
            concurrency_field,
            format!("{concurrency_field} must be greater than zero"),
            id,
            None,
        ));
    }

    Ok(())
}

/// Refuse likely credentials in durable, printable runner environment maps.
/// `secret_env` is the explicit sensitivity metadata for names that cannot be
/// safely inferred from their spelling.
pub fn validate_runner_env(env: &HashMap<String, String>, field: &str) -> Result<()> {
    for key in env.keys() {
        if is_likely_secret_env_key(key) {
            return Err(Error::validation_invalid_argument(
                format!("{field}.{key}"),
                format!(
                    "likely secret `{key}` cannot be persisted in printable runner env; use secret_env.{key} with an env, file, or keychain reference"
                ),
                None,
                Some(vec![format!(
                    "For a non-obvious secret name, add an explicit secret_env.{key} reference."
                )]),
            ));
        }
    }
    Ok(())
}

/// The redaction policy is deliberately broad for output safety (for example,
/// it redacts `monkey`). Persistence rejection needs narrower token boundaries
/// so public names are not false positives.
pub fn is_likely_secret_env_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    let segments = normalized.split('_').collect::<Vec<_>>();
    matches!(
        normalized.as_str(),
        "apikey" | "api_key" | "access_token" | "refresh_token" | "client_secret"
    ) || segments.iter().any(|segment| {
        matches!(
            *segment,
            "auth"
                | "authorization"
                | "bearer"
                | "cookie"
                | "credential"
                | "nonce"
                | "passwd"
                | "password"
                | "secret"
                | "session"
                | "sid"
                | "token"
        )
    }) || normalized.ends_with("_key")
}

impl ConfigEntity for Server {
    const ENTITY_TYPE: &'static str = "server";
    const DIR_NAME: &'static str = "servers";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::server_not_found(id, suggestions)
    }
    fn aliases(&self) -> &[String] {
        &self.aliases
    }
    fn dependents(id: &str) -> Result<Vec<String>> {
        let projects = project::list().unwrap_or_default();
        Ok(projects
            .iter()
            .filter(|p| p.server_id.as_deref() == Some(id))
            .map(|p| p.id.clone())
            .collect())
    }

    fn validate(&self) -> Result<()> {
        if let Some(runner) = self.runner.as_ref() {
            validate_runner_settings(&runner.settings, "runner.concurrency_limit", None)?;
            validate_runner_env(&runner.env, "runner.env")?;
        }

        Ok(())
    }
}

// ============================================================================
// Core CRUD - Generated by entity_crud! macro
// ============================================================================

entity_crud!(Server; merge);

pub fn find_by_host(host: &str) -> Option<Server> {
    list().ok()?.into_iter().find(|s| s.host == host)
}

pub fn key_path(id: &str) -> Result<std::path::PathBuf> {
    paths::key(id)
}

pub fn set_identity_file(id: &str, identity_file: Option<String>) -> Result<Server> {
    let mut server = load(id)?;
    server.identity_file = identity_file;
    save(&server)?;
    Ok(server)
}
