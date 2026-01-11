mod app;
mod component;
mod config_keys;
mod identifiable;
mod paths;
mod project;
mod project_id;
mod project_manager;
mod project_type;
mod record;
mod server;

pub use app::*;
pub use component::*;
pub use config_keys::*;
pub use identifiable::{slugify_id, SetName, SlugIdentifiable};
pub use paths::AppPaths;
pub use project::*;
pub use project_id::slugify_project_id;
pub use project_manager::*;
pub use project_type::*;
pub use record::*;
pub use server::*;

use crate::{Error, Result};
use std::fs;

pub struct ConfigManager;

impl ConfigManager {
    pub fn load_app_config() -> Result<AppConfig> {
        let path = AppPaths::config();
        if !path.exists() {
            return Ok(AppConfig::default());
        }
        let content = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save_app_config(config: &AppConfig) -> Result<()> {
        let path = AppPaths::config();
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(config)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn load_project(id: &str) -> Result<ProjectConfiguration> {
        Ok(Self::load_project_record(id)?.config)
    }

    pub fn load_project_record(id: &str) -> Result<ProjectRecord> {
        let path = AppPaths::project(id);
        if !path.exists() {
            return Err(Error::ProjectNotFound(id.to_string()));
        }
        let content = fs::read_to_string(&path)?;
        let config: ProjectConfiguration = serde_json::from_str(&content)?;

        let expected_id = slugify_id(&config.name)?;
        if expected_id != id {
            return Err(Error::Config(format!(
                "Project configuration mismatch: file '{}' implies id '{}', but name '{}' implies id '{}'. Run `homeboy project repair {}`.",
                path.display(),
                id,
                config.name,
                expected_id,
                id
            )));
        }

        Ok(ProjectRecord {
            id: id.to_string(),
            config,
        })
    }

    pub fn save_project(id: &str, project: &ProjectConfiguration) -> Result<()> {
        let expected_id = slugify_id(&project.name)?;
        if expected_id != id {
            return Err(Error::Config(format!(
                "Project id '{}' must match slug(name) '{}'. Use `homeboy project set {id} --name \"{}\"` to rename.",
                id, expected_id, project.name
            )));
        }

        let path = AppPaths::project(id);
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(project)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn list_projects() -> Result<Vec<ProjectRecord>> {
        let dir = AppPaths::projects();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut projects = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "json") {
                continue;
            }

            let Some(stem) = path.file_stem() else {
                continue;
            };
            let id = stem.to_string_lossy().to_string();

            let content = fs::read_to_string(&path)?;
            let config: ProjectConfiguration = serde_json::from_str(&content)?;

            let expected_id = slugify_id(&config.name)?;
            if expected_id != id {
                return Err(Error::Config(format!(
                    "Project configuration mismatch: file '{}' implies id '{}', but name '{}' implies id '{}'. Run `homeboy project repair {}`.",
                    path.display(),
                    id,
                    config.name,
                    expected_id,
                    id
                )));
            }

            projects.push(ProjectRecord { id, config });
        }
        projects.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        Ok(projects)
    }

    pub fn load_server(id: &str) -> Result<ServerConfig> {
        let path = AppPaths::server(id);
        if !path.exists() {
            return Err(Error::ServerNotFound(id.to_string()));
        }
        let content = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn list_servers() -> Result<Vec<ServerConfig>> {
        let dir = AppPaths::servers();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut servers = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(server) = serde_json::from_str::<ServerConfig>(&content) {
                        servers.push(server);
                    }
                }
            }
        }
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(servers)
    }

    pub fn get_active_project() -> Result<ProjectRecord> {
        let app_config = Self::load_app_config()?;
        let active_id = app_config.active_project_id.ok_or(Error::NoActiveProject)?;
        Self::load_project_record(&active_id)
    }

    pub fn set_active_project(id: &str) -> Result<()> {
        let _ = Self::load_project_record(id)?;
        let mut app_config = Self::load_app_config()?;
        app_config.active_project_id = Some(id.to_string());
        Self::save_app_config(&app_config)
    }

    pub fn save_server(id: &str, server: &ServerConfig) -> Result<()> {
        let expected_id = server.slug_id()?;
        if expected_id != id {
            return Err(Error::Config(format!(
                "Server id '{}' must match slug(name) '{}'. Use rename to change.",
                id, expected_id
            )));
        }

        let path = AppPaths::server(id);
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(server)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn delete_server(id: &str) -> Result<()> {
        let path = AppPaths::server(id);
        if !path.exists() {
            return Err(Error::ServerNotFound(id.to_string()));
        }
        fs::remove_file(&path)?;
        Ok(())
    }

    pub fn delete_project(id: &str) -> Result<()> {
        let path = AppPaths::project(id);
        if !path.exists() {
            return Err(Error::ProjectNotFound(id.to_string()));
        }
        fs::remove_file(&path)?;

        let mut app_config = Self::load_app_config()?;
        if app_config.active_project_id.as_deref() == Some(id) {
            app_config.active_project_id = None;
            Self::save_app_config(&app_config)?;
        }

        Ok(())
    }

    pub fn load_component(id: &str) -> Result<ComponentConfiguration> {
        let path = AppPaths::component(id);
        if !path.exists() {
            return Err(Error::ComponentNotFound(id.to_string()));
        }
        let content = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save_component(id: &str, component: &ComponentConfiguration) -> Result<()> {
        let expected_id = component.slug_id()?;
        if expected_id != id {
            return Err(Error::Config(format!(
                "Component id '{}' must match slug(name) '{}'. Use rename to change.",
                id, expected_id
            )));
        }

        let path = AppPaths::component(id);
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(component)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn delete_component(id: &str) -> Result<()> {
        let path = AppPaths::component(id);
        if !path.exists() {
            return Err(Error::ComponentNotFound(id.to_string()));
        }
        fs::remove_file(&path)?;
        Ok(())
    }

    pub fn list_components() -> Result<Vec<ComponentConfiguration>> {
        let dir = AppPaths::components();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut components = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(comp) = serde_json::from_str::<ComponentConfiguration>(&content) {
                        components.push(comp);
                    }
                }
            }
        }
        components.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(components)
    }

    pub fn list_component_ids() -> Result<Vec<String>> {
        let dir = AppPaths::components();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Some(stem) = path.file_stem() {
                    ids.push(stem.to_string_lossy().to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}
