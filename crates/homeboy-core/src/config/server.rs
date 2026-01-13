use serde::{Deserialize, Serialize};

use super::{AppPaths, ConfigImportable, ConfigManager, SlugIdentifiable};
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub identity_file: Option<String>,
}

fn default_port() -> u16 {
    22
}

impl SlugIdentifiable for ServerConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

impl ConfigImportable for ServerConfig {
    fn op_name() -> &'static str {
        "server.create"
    }

    fn type_name() -> &'static str {
        "server"
    }

    fn config_id(&self) -> Result<String> {
        self.slug_id()
    }

    fn exists(id: &str) -> bool {
        AppPaths::server(id).map(|p| p.exists()).unwrap_or(false)
    }

    fn load(id: &str) -> Result<Self> {
        ConfigManager::load_server(id)
    }

    fn save(id: &str, config: &Self) -> Result<()> {
        ConfigManager::save_server(id, config)
    }
}

impl ServerConfig {
    pub fn keychain_service_name(&self, prefix: &str) -> String {
        format!("{}.{}", prefix, self.id)
    }

    pub fn is_valid(&self) -> bool {
        !self.host.is_empty() && !self.user.is_empty()
    }

    pub fn generate_id(host: &str) -> String {
        format!("server-{}", host.replace('.', "-"))
    }
}
