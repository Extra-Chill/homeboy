pub mod api;
pub mod auth;
pub mod auth_profiles;
mod client;
mod connection;
pub mod health;
pub(crate) mod http;
mod keys;
mod process_cleanup;
mod session;
pub mod transfer;

pub(crate) use client::execute_local_command_stderr_passthrough;
pub use client::{
    execute_local_command, execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput, SshClient,
};
pub use connection::{resolve_context, SshResolveArgs, SshResolveResult};
pub use keys::{
    generate_key, get_public_key, import_key, unset_key, use_key, KeyGenerateResult,
    KeyImportResult,
};
pub use session::{ManagedSshSession, ManagedSshSessionOutput};

use std::collections::HashMap;

use crate::core::config::{self, ConfigEntity};
use crate::core::error::{Error, Result};
use crate::core::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::core::paths;
use crate::core::project;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_policy: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_excludes: Vec<String>,
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
    #[serde(default, skip_serializing_if = "RunnerPolicy::is_empty")]
    pub policy: RunnerPolicy,
}

impl RunnerPolicy {
    pub fn is_empty(&self) -> bool {
        self.accepted_peer_ids.is_empty()
            && self.accepted_peer_fingerprints.is_empty()
            && self.allowed_projects.is_empty()
            && self.allowed_commands.is_empty()
            && self.allow_raw_exec.is_none()
            && self.workspace_roots.is_empty()
            && self.artifact_policy.is_none()
            && self.snapshot_excludes.is_empty()
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
        if self
            .runner
            .as_ref()
            .and_then(|runner| runner.settings.concurrency_limit)
            == Some(0)
        {
            return Err(Error::validation_invalid_argument(
                "runner.concurrency_limit",
                "runner.concurrency_limit must be greater than zero",
                None,
                None,
            ));
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
