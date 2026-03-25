//! loading_functions — extracted from defaults.rs.

use std::fs;
use crate::engine::local_files;
use crate::paths;
use serde::{Deserialize, Serialize};


/// Load defaults, merging file config with built-in defaults.
/// If homeboy.json is missing or invalid, silently returns built-in defaults.
pub fn load_defaults() -> Defaults {
    load_config().defaults
}

/// Load the full homeboy.json config, falling back to defaults on any error.
/// Warns to stderr if the file exists but fails to parse, so the user knows
/// their config is being ignored rather than silently resetting to defaults.
pub fn load_config() -> HomeboyConfig {
    match load_config_from_file() {
        Ok(config) => config,
        Err(err) => {
            // Only warn if the file actually exists — missing file is expected
            if config_exists() {
                log_status!(
                    "config",
                    "Warning: failed to load homeboy.json ({}), using defaults",
                    err.message
                );
            }
            HomeboyConfig::default()
        }
    }
}

/// Attempt to load config from homeboy.json file.
pub(crate) fn load_config_from_file() -> crate::Result<HomeboyConfig> {
    let path = paths::homeboy_json()?;

    if !path.exists() {
        return Err(crate::Error::internal_io(
            "homeboy.json not found",
            Some(path.display().to_string()),
        ));
    }

    let content = local_files::read_file(&path, &format!("read {}", path.display()))?;

    let config: HomeboyConfig = serde_json::from_str(&content).map_err(|e| {
        crate::Error::validation_invalid_json(
            e,
            Some("parse homeboy.json".to_string()),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(config)
}

/// Save config to homeboy.json file (creates if missing).
pub fn save_config(config: &HomeboyConfig) -> crate::Result<()> {
    let path = paths::homeboy_json()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let content = crate::config::to_string_pretty(config)?;

    local_files::write_file_atomic(&path, &content, &format!("write {}", path.display()))?;

    Ok(())
}

/// Check if homeboy.json file exists
pub fn config_exists() -> bool {
    paths::homeboy_json().map(|p| p.exists()).unwrap_or(false)
}

/// Delete homeboy.json file (reset to defaults)
pub fn reset_config() -> crate::Result<bool> {
    let path = paths::homeboy_json()?;

    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
        })?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get the path to homeboy.json (for display purposes)
pub fn config_path() -> crate::Result<String> {
    Ok(paths::homeboy_json()?.display().to_string())
}

/// Get built-in defaults (ignoring any file config)
pub fn builtin_defaults() -> Defaults {
    Defaults::default()
}
