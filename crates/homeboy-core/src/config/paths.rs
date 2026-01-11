use crate::Result;
use std::path::PathBuf;

pub struct AppPaths;

impl AppPaths {
    pub fn homeboy() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join("Homeboy")
    }

    pub fn config() -> PathBuf {
        Self::homeboy().join("config.json")
    }

    pub fn projects() -> PathBuf {
        Self::homeboy().join("projects")
    }

    pub fn servers() -> PathBuf {
        Self::homeboy().join("servers")
    }

    pub fn components() -> PathBuf {
        Self::homeboy().join("components")
    }

    pub fn modules() -> PathBuf {
        Self::homeboy().join("modules")
    }

    pub fn keys() -> PathBuf {
        Self::homeboy().join("keys")
    }

    pub fn backups() -> PathBuf {
        Self::homeboy().join("backups")
    }

    pub fn project_types() -> PathBuf {
        Self::homeboy().join("project-types")
    }

    pub fn playwright_browsers() -> PathBuf {
        Self::homeboy().join("playwright-browsers")
    }

    pub fn docs() -> PathBuf {
        Self::homeboy().join("docs")
    }

    pub fn project(id: &str) -> PathBuf {
        Self::projects().join(format!("{}.json", id))
    }

    pub fn server(id: &str) -> PathBuf {
        Self::servers().join(format!("{}.json", id))
    }

    pub fn component(id: &str) -> PathBuf {
        Self::components().join(format!("{}.json", id))
    }

    pub fn module(id: &str) -> PathBuf {
        Self::modules().join(id)
    }

    pub fn key(server_id: &str) -> PathBuf {
        Self::keys().join(format!("{}_id_rsa", server_id))
    }

    pub fn ensure_directories() -> Result<()> {
        let dirs = [
            Self::homeboy(),
            Self::projects(),
            Self::servers(),
            Self::components(),
            Self::modules(),
            Self::keys(),
            Self::backups(),
            Self::project_types(),
        ];
        for dir in dirs {
            if !dir.exists() {
                std::fs::create_dir_all(&dir)?;
            }
        }
        Ok(())
    }
}
