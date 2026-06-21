use std::path::PathBuf;

use crate::core::error::Result;

use super::homeboy;

/// Projects directory
pub fn projects() -> Result<PathBuf> {
    Ok(homeboy()?.join("projects"))
}

/// Project directory path (e.g., ~/.config/homeboy/projects/{id}/)
pub fn project_dir(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id))
}

/// Project config file path (e.g., ~/.config/homeboy/projects/{id}/{id}.json)
pub fn project_config(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id).join(format!("{}.json", id)))
}

/// Servers directory
pub fn servers() -> Result<PathBuf> {
    Ok(homeboy()?.join("servers"))
}

/// Components directory
pub fn components() -> Result<PathBuf> {
    Ok(homeboy()?.join("components"))
}

/// Extensions directory
pub fn extensions() -> Result<PathBuf> {
    Ok(homeboy()?.join("extensions"))
}

/// Agent runtime package directory
pub fn agent_runtimes() -> Result<PathBuf> {
    Ok(homeboy()?.join("agent-runtimes"))
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(homeboy()?.join("keys"))
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(homeboy()?.join("backups"))
}
