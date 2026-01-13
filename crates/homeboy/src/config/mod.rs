mod component;
mod identifiable;
mod importable;
mod module_config;
mod paths;
mod project;
mod project_create_payload;
mod project_id;
mod project_manager;
mod record;
mod scoped_module;
mod server;

pub use component::*;
pub use identifiable::{slugify_id, SetName, SlugIdentifiable};
pub use importable::{
    create_from_json, ConfigImportable, CreateAction, CreateResult, CreateSummary,
};
pub use module_config::*;
pub use paths::AppPaths;
pub use project::*;
pub use project_create_payload::*;
pub use project_id::slugify_project_id;
pub use project_manager::*;
pub use record::*;
pub use scoped_module::*;
pub use server::*;

use crate::json::scan_json_dir;
use crate::{Error, Result};
use std::fs;

pub struct ConfigManager;

impl ConfigManager {
    pub fn load_project(id: &str) -> Result<ProjectConfiguration> {
        Ok(Self::load_project_record(id)?.config)
    }

    pub fn load_project_record(id: &str) -> Result<ProjectRecord> {
        let path = AppPaths::project(id)?;
        if !path.exists() {
            return Err(Error::project_not_found(id.to_string()));
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read project".to_string())))?;
        let config: ProjectConfiguration = serde_json::from_str(&content)
            .map_err(|e| Error::config_invalid_json(path.to_string_lossy().to_string(), e))?;

        let expected_id = slugify_id(&config.name)?;
        if expected_id != id {
            return Err(Error::config_invalid_value(
                "project.id",
                Some(id.to_string()),
                format!(
                    "Project configuration mismatch: file '{}' implies id '{}', but name '{}' implies id '{}'. Run `homeboy project repair {}`.",
                    path.display(),
                    id,
                    config.name,
                    expected_id,
                    id
                ),
            ));
        }

        Ok(ProjectRecord {
            id: id.to_string(),
            config,
        })
    }

    pub fn save_project(id: &str, project: &ProjectConfiguration) -> Result<()> {
        let expected_id = slugify_id(&project.name)?;
        if expected_id != id {
            return Err(Error::config_invalid_value(
                "project.id",
                Some(id.to_string()),
                format!(
                    "Project id '{}' must match slug(name) '{}'. Use `homeboy project set {id} --name \"{}\"` to rename.",
                    id, expected_id, project.name
                ),
            ));
        }

        let path = AppPaths::project(id)?;
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(project).map_err(|e| {
            Error::internal_json(e.to_string(), Some("serialize project".to_string()))
        })?;
        fs::write(&path, content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write project".to_string())))?;
        Ok(())
    }

    pub fn list_projects() -> Result<Vec<ProjectRecord>> {
        let dir = AppPaths::projects()?;
        let mut projects: Vec<ProjectRecord> = scan_json_dir::<ProjectConfiguration>(&dir)
            .into_iter()
            .filter_map(|(path, config)| {
                let id = path.file_stem()?.to_string_lossy().to_string();
                let expected_id = slugify_id(&config.name).ok()?;
                if expected_id != id {
                    return None;
                }
                Some(ProjectRecord { id, config })
            })
            .collect();
        projects.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        Ok(projects)
    }

    pub fn load_server(id: &str) -> Result<ServerConfig> {
        let path = AppPaths::server(id)?;
        if !path.exists() {
            return Err(Error::server_not_found(id.to_string()));
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read server".to_string())))?;
        serde_json::from_str(&content)
            .map_err(|e| Error::config_invalid_json(path.to_string_lossy().to_string(), e))
    }

    pub fn list_servers() -> Result<Vec<ServerConfig>> {
        let dir = AppPaths::servers()?;
        let mut servers: Vec<ServerConfig> = scan_json_dir(&dir)
            .into_iter()
            .map(|(_, server)| server)
            .collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(servers)
    }

    pub fn save_server(id: &str, server: &ServerConfig) -> Result<()> {
        let expected_id = server.slug_id()?;
        if expected_id != id {
            return Err(Error::config_invalid_value(
                "server.id",
                Some(id.to_string()),
                format!(
                    "Server id '{}' must match slug(name) '{}'. Use rename to change.",
                    id, expected_id
                ),
            ));
        }

        let path = AppPaths::server(id)?;
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(server).map_err(|e| {
            Error::internal_json(e.to_string(), Some("serialize server".to_string()))
        })?;
        fs::write(&path, content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write server".to_string())))?;
        Ok(())
    }

    pub fn delete_server(id: &str) -> Result<()> {
        let path = AppPaths::server(id)?;
        if !path.exists() {
            return Err(Error::server_not_found(id.to_string()));
        }
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("delete server".to_string())))?;
        Ok(())
    }

    pub fn delete_project(id: &str) -> Result<()> {
        let path = AppPaths::project(id)?;
        if !path.exists() {
            return Err(Error::project_not_found(id.to_string()));
        }
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("delete project".to_string())))?;
        Ok(())
    }

    pub fn load_component(id: &str) -> Result<ComponentConfiguration> {
        let path = AppPaths::component(id)?;
        if !path.exists() {
            return Err(Error::component_not_found(id.to_string()));
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read component".to_string())))?;
        serde_json::from_str(&content)
            .map_err(|e| Error::config_invalid_json(path.to_string_lossy().to_string(), e))
    }

    pub fn save_component(id: &str, component: &ComponentConfiguration) -> Result<()> {
        let expected_id = component.slug_id()?;
        if expected_id != id {
            return Err(Error::config_invalid_value(
                "component.id",
                Some(id.to_string()),
                format!(
                    "Component id '{}' must match slug(name) '{}'. Use rename to change.",
                    id, expected_id
                ),
            ));
        }

        let path = AppPaths::component(id)?;
        AppPaths::ensure_directories()?;
        let content = serde_json::to_string_pretty(component).map_err(|e| {
            Error::internal_json(e.to_string(), Some("serialize component".to_string()))
        })?;
        fs::write(&path, content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write component".to_string())))?;
        Ok(())
    }

    pub fn delete_component(id: &str) -> Result<()> {
        let path = AppPaths::component(id)?;
        if !path.exists() {
            return Err(Error::component_not_found(id.to_string()));
        }
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("delete component".to_string())))?;
        Ok(())
    }

    pub fn list_components() -> Result<Vec<ComponentConfiguration>> {
        let dir = AppPaths::components()?;
        let mut components: Vec<ComponentConfiguration> = scan_json_dir(&dir)
            .into_iter()
            .map(|(_, comp)| comp)
            .collect();
        components.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(components)
    }

    pub fn list_component_ids() -> Result<Vec<String>> {
        let dir = AppPaths::components()?;
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| {
            Error::internal_io(e.to_string(), Some("read components dir".to_string()))
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(e.to_string(), Some("read components dir entry".to_string()))
            })?;
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
