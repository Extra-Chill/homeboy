use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    pub local_path: String,
    /// Project-specific deploy target for this attached component.
    ///
    /// Repo-owned `homeboy.json` is portable component metadata, while the
    /// install path can vary by project layout. Keeping this optional field on
    /// the attachment lets one component deploy to multiple projects without
    /// rewriting the repo-tracked `remote_path` for each environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_deploy: Option<crate::core::component::GitDeployConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<crate::core::component::ScopeConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<crate::core::component::ArtifactInput>,
    /// Override the CLI path used by extension deploy install steps.
    /// For example, Studio sites need "studio wp" instead of the default "wp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteFileConfig {
    #[serde(default)]
    pub pinned_files: Vec<PinnedRemoteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinnedRemoteFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PinnedRemoteFile {
    pub fn display_name(&self) -> &str {
        self.label
            .as_deref()
            .unwrap_or_else(|| self.path.rsplit('/').next().unwrap_or(&self.path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteLogConfig {
    #[serde(default)]
    pub pinned_logs: Vec<PinnedRemoteLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinnedRemoteLog {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_tail_lines")]
    pub tail_lines: u32,
}

fn default_tail_lines() -> u32 {
    100
}

impl PinnedRemoteLog {
    pub fn display_name(&self) -> &str {
        self.label
            .as_deref()
            .unwrap_or_else(|| self.path.rsplit('/').next().unwrap_or(&self.path))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinType {
    File,
    Log,
}

pub struct PinOptions {
    pub label: Option<String>,
    pub tail_lines: u32,
}

impl Default for PinOptions {
    fn default() -> Self {
        Self {
            label: None,
            tail_lines: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_host")]
    pub host: String,
    #[serde(default = "default_db_port")]
    pub port: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub user: String,
    #[serde(default = "default_true")]
    pub use_ssh_tunnel: bool,
}

fn default_db_host() -> String {
    "localhost".to_string()
}

fn default_db_port() -> u16 {
    3306
}

fn default_true() -> bool {
    true
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            host: default_db_host(),
            port: default_db_port(),
            name: String::new(),
            user: String::new(),
            use_ssh_tunnel: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub header: String,
    #[serde(default)]
    pub variables: HashMap<String, VariableSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<AuthFlowConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<AuthFlowConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableSource {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFlowConfig {
    pub endpoint: String,
    #[serde(default = "default_post_method")]
    pub method: String,
    #[serde(default)]
    pub body: HashMap<String, String>,
    #[serde(default)]
    pub store: HashMap<String, String>,
}

fn default_post_method() -> String {
    "POST".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTarget {
    pub name: String,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(default)]
    pub is_default: bool,
}

impl SubTarget {
    pub fn table_prefix(&self, base_prefix: &str) -> String {
        match self.number {
            Some(n) if n > 1 => format!("{}{}_", base_prefix, n),
            _ => base_prefix.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub bandcamp_scraper: BandcampScraperConfig,
    #[serde(default)]
    pub newsletter: NewsletterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BandcampScraperConfig {
    #[serde(default)]
    pub default_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewsletterConfig {
    #[serde(default)]
    pub sendy_list_id: String,
}
