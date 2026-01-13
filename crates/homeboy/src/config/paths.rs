use crate::{Error, Result};
use std::path::PathBuf;

pub struct AppPaths;

impl AppPaths {
    pub fn homeboy() -> Result<PathBuf> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            Error::internal_unexpected(
                "Unable to resolve a config directory for this OS. This likely indicates a broken environment (missing HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME) or a bug in the config path resolver."
                    .to_string(),
            )
        })?;

        Ok(config_dir.join("homeboy"))
    }

    pub fn config() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("homeboy.json"))
    }

    pub fn projects() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("projects"))
    }

    pub fn servers() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("servers"))
    }

    pub fn components() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("components"))
    }

    pub fn modules() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("modules"))
    }

    pub fn keys() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("keys"))
    }

    pub fn backups() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("backups"))
    }

    pub fn docs() -> Result<PathBuf> {
        Ok(Self::homeboy()?.join("docs"))
    }

    pub fn project(id: &str) -> Result<PathBuf> {
        Ok(Self::projects()?.join(format!("{}.json", id)))
    }

    pub fn server(id: &str) -> Result<PathBuf> {
        Ok(Self::servers()?.join(format!("{}.json", id)))
    }

    pub fn component(id: &str) -> Result<PathBuf> {
        Ok(Self::components()?.join(format!("{}.json", id)))
    }

    pub fn module(id: &str) -> Result<PathBuf> {
        Ok(Self::modules()?.join(id))
    }

    pub fn key(server_id: &str) -> Result<PathBuf> {
        Ok(Self::keys()?.join(format!("{}_id_rsa", server_id)))
    }

    pub fn ensure_directories() -> Result<()> {
        let dirs = [
            Self::homeboy()?,
            Self::projects()?,
            Self::servers()?,
            Self::components()?,
            Self::modules()?,
            Self::keys()?,
            Self::backups()?,
        ];
        for dir in dirs {
            if !dir.exists() {
                std::fs::create_dir_all(&dir).map_err(|e| {
                    Error::internal_io(e.to_string(), Some("create config directory".to_string()))
                })?;
            }
        }
        Ok(())
    }
}
