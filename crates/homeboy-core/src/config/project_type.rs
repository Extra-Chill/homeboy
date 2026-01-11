use crate::config::AppPaths;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTypeDefinition {
    pub id: String,
    pub display_name: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default)]
    pub default_pinned_files: Vec<String>,
    #[serde(default)]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseSchemaDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
}

impl ProjectTypeDefinition {
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
    }

    pub fn is_wordpress(&self) -> bool {
        self.config_schema.as_deref() == Some("wordpress")
    }

    pub fn fallback_generic() -> Self {
        Self {
            id: "generic".to_string(),
            display_name: "Generic".to_string(),
            icon: "server.rack".to_string(),
            config_schema: None,
            default_pinned_files: vec![],
            default_pinned_logs: vec![],
            database: None,
            cli: None,
            discovery: None,
        }
    }

    pub fn builtin_wordpress() -> Self {
        Self {
            id: "wordpress".to_string(),
            display_name: "WordPress".to_string(),
            icon: "w.square".to_string(),
            config_schema: Some("wordpress".to_string()),
            default_pinned_files: vec!["wp-config.php".to_string()],
            default_pinned_logs: vec!["logs/error.log".to_string(), "logs/debug.log".to_string()],
            database: Some(DatabaseSchemaDefinition {
                cli: Some(DatabaseCliConfig {
                    tables_command: "cd {{sitePath}} && {{cliPath}} db tables --format=json".to_string(),
                    describe_command: "cd {{sitePath}} && {{cliPath}} db columns {{table}} --format=json".to_string(),
                    query_command: "cd {{sitePath}} && {{cliPath}} db query '{{query}}' --format={{format}}".to_string(),
                }),
                default_table_prefix: Some("wp_".to_string()),
                prefix_detection_suffixes: Some(vec!["options".to_string(), "posts".to_string(), "users".to_string()]),
                table_suffixes: None,
                protected_suffixes: Some(vec!["users".to_string(), "usermeta".to_string()]),
                default_grouping: None,
                multisite_grouping: None,
            }),
            cli: Some(CliConfig {
                tool: "wp".to_string(),
                display_name: "WP-CLI".to_string(),
                command_template: "cd {{sitePath}} && {{cliPath}} --url={{domain}} {{args}}".to_string(),
                default_cli_path: Some("wp".to_string()),
            }),
            discovery: Some(DiscoveryConfig {
                find_command: "find /home -name 'wp-config.php' -type f 2>/dev/null | head -20".to_string(),
                base_path_transform: "dirname".to_string(),
                display_name_command: Some("cd {{basePath}} && wp option get blogname 2>/dev/null || basename {{basePath}}".to_string()),
            }),
        }
    }

    pub fn builtin_nodejs() -> Self {
        Self {
            id: "nodejs".to_string(),
            display_name: "Node.js".to_string(),
            icon: "n.square".to_string(),
            config_schema: None,
            default_pinned_files: vec!["package.json".to_string(), ".env".to_string()],
            default_pinned_logs: vec!["logs/app.log".to_string()],
            database: None,
            cli: Some(CliConfig {
                tool: "pm2".to_string(),
                display_name: "PM2".to_string(),
                command_template: "cd {{sitePath}} && pm2 {{args}}".to_string(),
                default_cli_path: Some("pm2".to_string()),
            }),
            discovery: Some(DiscoveryConfig {
                find_command: "find /home -name 'package.json' -type f 2>/dev/null | head -20"
                    .to_string(),
                base_path_transform: "dirname".to_string(),
                display_name_command: None,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSchemaDefinition {
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

pub struct ProjectTypeManager;

impl ProjectTypeManager {
    pub fn resolve(type_id: &str) -> ProjectTypeDefinition {
        // First try to load from user-defined JSON
        let path = AppPaths::project_types().join(format!("{}.json", type_id));
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(def) = serde_json::from_str::<ProjectTypeDefinition>(&content) {
                    return def;
                }
            }
        }

        // Fall back to built-in types
        match type_id {
            "wordpress" => ProjectTypeDefinition::builtin_wordpress(),
            "nodejs" => ProjectTypeDefinition::builtin_nodejs(),
            _ => ProjectTypeDefinition::fallback_generic(),
        }
    }
}
