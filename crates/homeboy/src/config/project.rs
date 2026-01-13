use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{slugify_id, AppPaths, ConfigImportable, ConfigManager, SetName, SlugIdentifiable};
use crate::Result;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub config: ProjectConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfiguration {
    pub name: String,
    pub domain: String,
    #[serde(default)]
    pub modules: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_modules: Option<std::collections::HashMap<String, super::ScopedModuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_prefix: Option<String>,

    #[serde(default)]
    pub remote_files: RemoteFileConfig,
    #[serde(default)]
    pub remote_logs: RemoteLogConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub api: ApiConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_aliases: Option<Vec<String>>,

    #[serde(default)]
    pub sub_targets: Vec<SubTarget>,
    #[serde(default)]
    pub shared_tables: Vec<String>,
    #[serde(default)]
    pub component_ids: Vec<String>,
}

impl SlugIdentifiable for ProjectConfiguration {
    fn name(&self) -> &str {
        &self.name
    }
}

impl SetName for ProjectConfiguration {
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
}

impl ConfigImportable for ProjectConfiguration {
    fn op_name() -> &'static str {
        "project.create"
    }

    fn type_name() -> &'static str {
        "project"
    }

    fn config_id(&self) -> Result<String> {
        slugify_id(&self.name)
    }

    fn exists(id: &str) -> bool {
        AppPaths::project(id).map(|p| p.exists()).unwrap_or(false)
    }

    fn load(id: &str) -> Result<Self> {
        ConfigManager::load_project(id)
    }

    fn save(id: &str, config: &Self) -> Result<()> {
        ConfigManager::save_project(id, config)
    }
}

impl ProjectConfiguration {
    pub fn has_module(&self, module_id: &str) -> bool {
        self.modules.contains(&module_id.to_string())
    }

    pub fn has_sub_targets(&self) -> bool {
        !self.sub_targets.is_empty()
    }

    pub fn default_sub_target(&self) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| t.is_default)
            .or_else(|| self.sub_targets.first())
    }

    pub fn find_sub_target(&self, id: &str) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| t.slug_id().ok().as_deref() == Some(id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileConfig {
    #[serde(default)]
    pub pinned_files: Vec<PinnedRemoteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRemoteFile {
    pub id: Uuid,
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
#[serde(rename_all = "camelCase")]
pub struct RemoteLogConfig {
    #[serde(default)]
    pub pinned_logs: Vec<PinnedRemoteLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRemoteLog {
    pub id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

/// Generic authentication configuration.
/// Homeboy doesn't know about specific auth types - it just templates strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    /// Header template, e.g., "Authorization: Bearer {{access_token}}"
    pub header: String,
    /// Variable sources - where to get values for {{variables}}
    #[serde(default)]
    pub variables: std::collections::HashMap<String, VariableSource>,
    /// Optional login flow to obtain tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<AuthFlowConfig>,
    /// Optional refresh flow to renew tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<AuthFlowConfig>,
}

/// Source for a template variable
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableSource {
    /// Where to get the value: "keychain", "config", "env"
    pub source: String,
    /// For "config" source: the literal value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// For "env" source: the environment variable name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

/// Configuration for login or refresh flow
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowConfig {
    /// Endpoint path (appended to base_url)
    pub endpoint: String,
    /// HTTP method (defaults to POST)
    #[serde(default = "default_post_method")]
    pub method: String,
    /// Request body template - values are {{variable}} templates
    #[serde(default)]
    pub body: std::collections::HashMap<String, String>,
    /// Response field mapping - keys are variable names to store, values are JSON paths
    #[serde(default)]
    pub store: std::collections::HashMap<String, String>,
}

fn default_post_method() -> String {
    "POST".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubTarget {
    pub name: String,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(default)]
    pub is_default: bool,
}

impl SlugIdentifiable for SubTarget {
    fn name(&self) -> &str {
        &self.name
    }
}

impl SetName for SubTarget {
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
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
#[serde(rename_all = "camelCase")]
pub struct ToolsConfig {
    #[serde(default)]
    pub bandcamp_scraper: BandcampScraperConfig,
    #[serde(default)]
    pub newsletter: NewsletterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BandcampScraperConfig {
    #[serde(default)]
    pub default_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterConfig {
    #[serde(default)]
    pub sendy_list_id: String,
}
