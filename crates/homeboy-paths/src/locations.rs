use std::path::{Path, PathBuf};

use homeboy_error::Result;

use super::homeboy;

/// Projects directory below an already-resolved config root.
pub fn projects_in_root(config_root: &Path) -> PathBuf {
    config_root.join("projects")
}

/// Projects directory
pub fn projects() -> Result<PathBuf> {
    Ok(projects_in_root(&homeboy()?))
}

/// Project directory path below an already-resolved config root.
pub fn project_dir_in_root(config_root: &Path, id: &str) -> PathBuf {
    projects_in_root(config_root).join(id)
}

/// Project directory path (e.g., ~/.config/homeboy/projects/{id}/)
pub fn project_dir(id: &str) -> Result<PathBuf> {
    Ok(project_dir_in_root(&homeboy()?, id))
}

/// Project config file path below an already-resolved config root.
pub fn project_config_in_root(config_root: &Path, id: &str) -> PathBuf {
    projects_in_root(config_root)
        .join(id)
        .join(format!("{}.json", id))
}

/// Project config file path (e.g., ~/.config/homeboy/projects/{id}/{id}.json)
pub fn project_config(id: &str) -> Result<PathBuf> {
    Ok(project_config_in_root(&homeboy()?, id))
}

/// Servers directory below an already-resolved config root.
pub fn servers_in_root(config_root: &Path) -> PathBuf {
    config_root.join("servers")
}

/// Servers directory
pub fn servers() -> Result<PathBuf> {
    Ok(servers_in_root(&homeboy()?))
}

/// Components directory below an already-resolved config root.
pub fn components_in_root(config_root: &Path) -> PathBuf {
    config_root.join("components")
}

/// Components directory
pub fn components() -> Result<PathBuf> {
    Ok(components_in_root(&homeboy()?))
}

/// Extensions directory below an already-resolved config root.
pub fn extensions_in_root(config_root: &Path) -> PathBuf {
    config_root.join("extensions")
}

/// Extensions directory
pub fn extensions() -> Result<PathBuf> {
    Ok(extensions_in_root(&homeboy()?))
}

/// Agent runtime package directory below an already-resolved config root.
///
/// This is the documented and process-stable runtime boundary. Generation
/// activation changes only runtime-generations/current behind this path.
pub fn agent_runtimes_in_root(config_root: &Path) -> PathBuf {
    config_root.join("agent-runtimes")
}

/// Agent runtime package directory
pub fn agent_runtimes() -> Result<PathBuf> {
    Ok(agent_runtimes_in_root(&homeboy()?))
}

/// Legacy runtime package directory below an already-resolved config root.
pub fn legacy_agent_runtimes_in_root(config_root: &Path) -> PathBuf {
    config_root.join("agent-runtimes")
}

/// Legacy runtime package directory, used only while migrating it into a
/// generation. Runtime consumers must use [`agent_runtimes`].
pub fn legacy_agent_runtimes() -> Result<PathBuf> {
    Ok(legacy_agent_runtimes_in_root(&homeboy()?))
}

/// Keys directory below an already-resolved config root.
pub fn keys_in_root(config_root: &Path) -> PathBuf {
    config_root.join("keys")
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(keys_in_root(&homeboy()?))
}

/// Backups directory below an already-resolved config root.
pub fn backups_in_root(config_root: &Path) -> PathBuf {
    config_root.join("backups")
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(backups_in_root(&homeboy()?))
}
