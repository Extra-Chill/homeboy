//! project — extracted from mod.rs.

use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::paths;
use crate::server;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use crate::engine::local_files::{self, FileSystem};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::core::project::ApiConfig;
use crate::core::project::ProjectComponentOverrides;
use crate::core::project::RemoteLogConfig;
use crate::core::project::ProjectComponentAttachment;
use crate::core::project::ToolsConfig;
use crate::core::project::RemoteFileConfig;
use crate::core::project::SubTarget;


#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct Project {
    #[serde(skip)]
    pub id: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ProjectComponentAttachment>,
    /// Per-component field overrides applied when a component runs in this project.
    ///
    /// Example: `{"data-machine": {"extract_command": "...", "remote_owner": "opencode:opencode"}}`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub component_overrides: HashMap<String, ProjectComponentOverrides>,

    /// Service names to check in fleet health status (e.g. ["kimaki", "php8.4-fpm", "nginx"]).
    /// These are checked via `systemctl is-active <name>` on the remote server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
}

impl ConfigEntity for Project {
    const ENTITY_TYPE: &'static str = "project";
    const DIR_NAME: &'static str = "projects";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::project_not_found(id, suggestions)
    }

    /// Directory-based config: `~/.config/homeboy/projects/{id}/{id}.json`.
    ///
    /// Falls back to legacy flat file `~/.config/homeboy/projects/{id}.json`
    /// if the directory-based path doesn't exist yet. This allows transparent
    /// migration — existing projects keep working, new projects use directories.
    fn config_path(id: &str) -> Result<PathBuf> {
        let dir_path = paths::project_config(id)?;
        if dir_path.exists() {
            return Ok(dir_path);
        }

        // Check for legacy flat file
        let flat_path = Self::config_dir()?.join(format!("{}.json", id));
        if flat_path.exists() {
            return Ok(flat_path);
        }

        // Default to directory-based for new projects
        Ok(dir_path)
    }

    fn validate(&self) -> Result<()> {
        if let Some(ref sid) = self.server_id {
            if !server::exists(sid) {
                let suggestions = config::find_similar_ids::<server::Server>(sid);
                return Err(Error::server_not_found(sid.clone(), suggestions));
            }
        }
        Ok(())
    }
    fn aliases(&self) -> &[String] {
        &self.aliases
    }
}
