use crate::error::{Error, Result};
use std::env;
use std::path::PathBuf;

/// Base homeboy config directory (universal ~/.config/homeboy/ on all platforms)
pub fn homeboy() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = env::var("APPDATA").map_err(|_| {
            Error::internal_unexpected(
                "APPDATA environment variable not set on Windows".to_string(),
            )
        })?;
        Ok(PathBuf::from(appdata).join("homeboy"))
    }

    #[cfg(not(windows))]
    {
        let home = env::var("HOME").map_err(|_| {
            Error::internal_unexpected(
                "HOME environment variable not set on Unix-like system".to_string(),
            )
        })?;
        Ok(PathBuf::from(home).join(".config").join("homeboy"))
    }
}

/// Global homeboy.json config file path
pub fn homeboy_json() -> Result<PathBuf> {
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

/// Module directory path
pub fn module(id: &str) -> Result<PathBuf> {
    Ok(modules()?.join(id))
}

/// Module manifest file path
pub fn module_manifest(id: &str) -> Result<PathBuf> {
    Ok(modules()?.join(id).join(format!("{}.json", id)))
}

/// Key file path
pub fn key(server_id: &str) -> Result<PathBuf> {
    Ok(keys()?.join(format!("{}_id_rsa", server_id)))
}
