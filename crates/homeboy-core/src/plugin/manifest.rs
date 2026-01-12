use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default)]
    pub default_pinned_files: Vec<String>,
    #[serde(default)]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(skip)]
    pub plugin_path: Option<String>,
}

impl PluginManifest {
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<DatabaseCliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_table_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_detection_suffixes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_suffixes: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_suffixes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_grouping: Option<GroupingTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multisite_grouping: Option<MultisiteGroupingTemplate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCliConfig {
    pub tables_command: String,
    pub describe_command: String,
    pub query_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingTemplate {
    pub id: String,
    pub name: String,
    pub pattern_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultisiteGroupingTemplate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkGroupingTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteGroupingTemplate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkGroupingTemplate {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteGroupingTemplate {
    pub id_template: String,
    pub name_template: String,
    pub pattern_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliConfig {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_injections: Vec<ArgInjectionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgInjectionConfig {
    pub setting_key: String,
    pub arg_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryConfig {
    pub find_command: String,
    pub base_path_transform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name_command: Option<String>,
}

impl DiscoveryConfig {
    pub fn transform_to_base_path(&self, path: &str) -> String {
        match self.base_path_transform.as_str() {
            "dirname" => std::path::Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string()),
            _ => path.to_string(),
        }
    }
}
