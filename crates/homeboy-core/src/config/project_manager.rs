use std::fs;

use super::{slugify_id, AppPaths, ConfigManager, ProjectConfiguration};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct RenameResult {
    pub old_id: String,
    pub new_id: String,
    pub config: ProjectConfiguration,
}

pub struct ProjectManager;

impl ProjectManager {
    pub fn create_project(
        name: &str,
        domain: &str,
        project_type: &str,
        server_id: Option<String>,
        base_path: Option<String>,
        table_prefix: Option<String>,
    ) -> Result<(String, ProjectConfiguration)> {
        let id = slugify_id(name)?;
        let path = AppPaths::project(&id);
        if path.exists() {
            return Err(Error::Config(format!("Project '{id}' already exists")));
        }

        let project = ProjectConfiguration {
            name: name.to_string(),
            domain: domain.to_string(),
            project_type: project_type.to_string(),
            server_id,
            base_path,
            table_prefix,
            remote_files: Default::default(),
            remote_logs: Default::default(),
            database: Default::default(),
            local_environment: Default::default(),
            tools: Default::default(),
            api: Default::default(),
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            sub_targets: Default::default(),
            shared_tables: Default::default(),
            component_ids: Default::default(),
            table_groupings: Default::default(),
            component_groupings: Default::default(),
            protected_table_patterns: Default::default(),
            unlocked_table_patterns: Default::default(),
        };

        ConfigManager::save_project(&id, &project)?;

        Ok((id, project))
    }

    pub fn rename_project(id: &str, new_name: &str) -> Result<RenameResult> {
        let record = ConfigManager::load_project_record(id)?;
        let mut config = record.config;
        config.name = new_name.to_string();

        let new_id = slugify_id(&config.name)?;
        if new_id == id {
            ConfigManager::save_project(id, &config)?;
            return Ok(RenameResult {
                old_id: id.to_string(),
                new_id,
                config,
            });
        }

        let old_path = AppPaths::project(id);
        let new_path = AppPaths::project(&new_id);

        if new_path.exists() {
            return Err(Error::Config(format!(
                "Cannot rename project '{id}' to '{new_id}': destination already exists"
            )));
        }

        AppPaths::ensure_directories()?;
        fs::rename(&old_path, &new_path)?;

        if let Err(error) = ConfigManager::save_project(&new_id, &config) {
            let _ = fs::rename(&new_path, &old_path);
            return Err(error);
        }

        let mut app_config = ConfigManager::load_app_config()?;
        if app_config.active_project_id.as_deref() == Some(id) {
            app_config.active_project_id = Some(new_id.clone());
            ConfigManager::save_app_config(&app_config)?;
        }

        Ok(RenameResult {
            old_id: id.to_string(),
            new_id,
            config,
        })
    }

    pub fn repair_project(id: &str) -> Result<RenameResult> {
        let path = AppPaths::project(id);
        if !path.exists() {
            return Err(Error::ProjectNotFound(id.to_string()));
        }

        let content = fs::read_to_string(&path)?;
        let config: ProjectConfiguration = serde_json::from_str(&content)?;
        let expected_id = slugify_id(&config.name)?;

        if expected_id == id {
            return Ok(RenameResult {
                old_id: id.to_string(),
                new_id: id.to_string(),
                config,
            });
        }

        let new_path = AppPaths::project(&expected_id);
        if new_path.exists() {
            return Err(Error::Config(format!(
                "Cannot repair project '{id}' to '{expected_id}': destination already exists"
            )));
        }

        AppPaths::ensure_directories()?;
        fs::rename(&path, &new_path)?;

        let mut app_config = ConfigManager::load_app_config()?;
        if app_config.active_project_id.as_deref() == Some(id) {
            app_config.active_project_id = Some(expected_id.clone());
            ConfigManager::save_app_config(&app_config)?;
        }

        Ok(RenameResult {
            old_id: id.to_string(),
            new_id: expected_id,
            config,
        })
    }
}
