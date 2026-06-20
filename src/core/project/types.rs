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

/// Post-deploy smoke check configuration.
///
/// Opt-in, config-driven front-end health check that runs after a successful
/// real deploy. It fetches a configured URL as a fresh (cookie-less) visitor
/// and asserts the HTTP status (and optionally a content substring), failing
/// the deploy when the smoke fails.
///
/// This is deliberately generic: core only knows "fetch a URL, assert a
/// status/content". The concrete front-end URL (e.g. a WordPress site home
/// page) belongs in the project config, not in core, so the smoke step stays
/// stack-agnostic. It exists to catch runtime-fataling releases that pass
/// syntax-only checks (`php -l`) and never get exercised by a real page load
/// — see homeboy#5471.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeCheckConfig {
    /// Whether the post-deploy smoke check runs. Defaults to false (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// URL to fetch after deploy. Required when `enabled` is true.
    #[serde(default)]
    pub url: String,
    /// HTTP status code that counts as healthy. Defaults to 200.
    #[serde(default = "default_smoke_expected_status")]
    pub expected_status: u16,
    /// Optional substring that must appear in the response body. When set, the
    /// smoke also fetches the body and fails if the substring is absent — a
    /// cheap way to assert "real page rendered", not just "server answered".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_content: Option<String>,
    /// Request timeout in seconds. Defaults to 15.
    #[serde(default = "default_smoke_timeout_secs")]
    pub timeout_secs: u64,
    /// When true, a failed smoke check only warns instead of failing the deploy.
    /// Defaults to false: a failing smoke fails the deploy so runtime-fataling
    /// releases are flagged for rollback rather than left live.
    #[serde(default)]
    pub warn_only: bool,
}

fn default_smoke_expected_status() -> u16 {
    200
}

fn default_smoke_timeout_secs() -> u64 {
    15
}

impl Default for SmokeCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            expected_status: default_smoke_expected_status(),
            expect_content: None,
            timeout_secs: default_smoke_timeout_secs(),
            warn_only: false,
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
