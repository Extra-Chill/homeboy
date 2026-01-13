use crate::error::{Error, Result};
use std::path::PathBuf;

/// Base homeboy config directory
pub fn homeboy() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        Error::internal_unexpected(
            "Unable to resolve a config directory for this OS. This likely indicates a broken environment (missing HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME) or a bug in the config path resolver."
                .to_string(),
        )
    })?;
    Ok(config_dir.join("homeboy"))
}

/// Main config file
pub fn config() -> Result<PathBuf> {
    Ok(homeboy()?.join("homeboy.json"))
}

/// Projects directory
pub fn projects() -> Result<PathBuf> {
    Ok(homeboy()?.join("projects"))
}

/// Servers directory
pub fn servers() -> Result<PathBuf> {
    Ok(homeboy()?.join("servers"))
}

/// Components directory
pub fn components() -> Result<PathBuf> {
    Ok(homeboy()?.join("components"))
}

/// Modules directory
pub fn modules() -> Result<PathBuf> {
    Ok(homeboy()?.join("modules"))
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(homeboy()?.join("keys"))
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(homeboy()?.join("backups"))
}

/// Docs directory
pub fn docs() -> Result<PathBuf> {
    Ok(homeboy()?.join("docs"))
}

/// Project file path
pub fn project(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(format!("{}.json", id)))
}

/// Server file path
pub fn server(id: &str) -> Result<PathBuf> {
    Ok(servers()?.join(format!("{}.json", id)))
}

/// Component file path
pub fn component(id: &str) -> Result<PathBuf> {
    Ok(components()?.join(format!("{}.json", id)))
}

/// Module directory path
pub fn module(id: &str) -> Result<PathBuf> {
    Ok(modules()?.join(id))
}

/// Key file path
pub fn key(server_id: &str) -> Result<PathBuf> {
    Ok(keys()?.join(format!("{}_id_rsa", server_id)))
}
