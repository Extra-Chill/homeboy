use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{AppPaths, ConfigImportable, ConfigManager};
use crate::Result;

fn default_database_host() -> String {
    "127.0.0.1".to_string()
}

fn default_local_db_port() -> u16 {
    33306
}

fn is_default_database_host(v: &String) -> bool {
    v == "127.0.0.1"
}

fn is_default_local_db_port(v: &u16) -> bool {
    *v == 33306
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_changelog_next_section_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_changelog_next_section_aliases: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_modules: Option<HashMap<String, InstalledModuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,

    #[serde(
        default = "default_database_host",
        skip_serializing_if = "is_default_database_host"
    )]
    pub default_database_host: String,

    #[serde(
        default = "default_local_db_port",
        skip_serializing_if = "is_default_local_db_port"
    )]
    pub default_local_db_port: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_changelog_next_section_label: None,
            default_changelog_next_section_aliases: None,
            installed_modules: None,
            default_cli_path: None,
            default_database_host: default_database_host(),
            default_local_db_port: default_local_db_port(),
        }
    }
}

impl ConfigImportable for AppConfig {
    fn op_name() -> &'static str {
        "config.create"
    }

    fn type_name() -> &'static str {
        "config"
    }

    fn config_id(&self) -> Result<String> {
        Ok("global".to_string())
    }

    fn exists(_id: &str) -> bool {
        AppPaths::config().map(|p| p.exists()).unwrap_or(false)
    }

    fn load(_id: &str) -> Result<Self> {
        ConfigManager::load_app_config()
    }

    fn save(_id: &str, config: &Self) -> Result<()> {
        ConfigManager::save_app_config(config)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModuleConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}
