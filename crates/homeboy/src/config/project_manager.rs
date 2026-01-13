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
        modules: Vec<String>,
        server_id: Option<String>,
        base_path: Option<String>,
        table_prefix: Option<String>,
    ) -> Result<(String, ProjectConfiguration)> {
        let id = slugify_id(name)?;
        let path = AppPaths::project(&id)?;
        if path.exists() {
            return Err(Error::validation_invalid_argument(
                "project.name",
                format!("Project '{id}' already exists"),
                Some(id.clone()),
                None,
            ));
        }

        let project = ProjectConfiguration {
            name: name.to_string(),
            domain: domain.to_string(),
            modules,
            scoped_modules: None,
            server_id,
            base_path,
            table_prefix,
            remote_files: Default::default(),
            remote_logs: Default::default(),
            database: Default::default(),
            tools: Default::default(),
            api: Default::default(),
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            sub_targets: Default::default(),
            shared_tables: Default::default(),
            component_ids: Default::default(),
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

        let old_path = AppPaths::project(id)?;
        let new_path = AppPaths::project(&new_id)?;

        if new_path.exists() {
            return Err(Error::validation_invalid_argument(
                "project.name",
                format!("Cannot rename project '{id}' to '{new_id}': destination already exists"),
                Some(new_id.clone()),
                None,
            ));
        }

        AppPaths::ensure_directories()?;
        fs::rename(&old_path, &new_path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("rename project".to_string())))?;

        if let Err(error) = ConfigManager::save_project(&new_id, &config) {
            let _ = fs::rename(&new_path, &old_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("rollback project rename".to_string()))
            });
            return Err(error);
        }

        Ok(RenameResult {
            old_id: id.to_string(),
            new_id,
            config,
        })
    }

    pub fn repair_project(id: &str) -> Result<RenameResult> {
        let path = AppPaths::project(id)?;
        if !path.exists() {
            return Err(Error::project_not_found(id.to_string()));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read project".to_string())))?;
        let config: ProjectConfiguration = serde_json::from_str(&content)
            .map_err(|e| Error::config_invalid_json(path.to_string_lossy().to_string(), e))?;
        let expected_id = slugify_id(&config.name)?;

        if expected_id == id {
            return Ok(RenameResult {
                old_id: id.to_string(),
                new_id: id.to_string(),
                config,
            });
        }

        let new_path = AppPaths::project(&expected_id)?;
        if new_path.exists() {
            return Err(Error::validation_invalid_argument(
                "project.name",
                format!(
                    "Cannot repair project '{id}' to '{expected_id}': destination already exists"
                ),
                Some(expected_id.clone()),
                None,
            ));
        }

        AppPaths::ensure_directories()?;
        fs::rename(&path, &new_path).map_err(|e| {
            Error::internal_io(e.to_string(), Some("repair project rename".to_string()))
        })?;

        Ok(RenameResult {
            old_id: id.to_string(),
            new_id: expected_id,
            config,
        })
    }
}
