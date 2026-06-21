use std::fs;
use std::sync::{OnceLock, RwLock};

use crate::core::engine::local_files;
use crate::core::paths;

use super::HomeboyConfig;

/// Load the full product config, falling back to defaults on any error.
/// Warns to stderr if the file exists but fails to parse, so the user knows
/// their config is being ignored rather than silently resetting to defaults.
pub fn load_config() -> HomeboyConfig {
    if let Some(config) = cached_config() {
        return config;
    }

    let config = load_config_uncached();
    store_cached_config(&config);
    config
}

fn load_config_uncached() -> HomeboyConfig {
    match load_config_from_file() {
        Ok(config) => config,
        Err(err) => {
            // Only warn if the file actually exists — missing file is expected
            if config_exists() {
                log_status!(
                    "config",
                    "Warning: failed to load {} ({}), using defaults",
                    crate::core::product_identity::PRODUCT_IDENTITY.config_filename,
                    err.message
                );
            }
            HomeboyConfig::default()
        }
    }
}

fn config_cache() -> &'static RwLock<Option<HomeboyConfig>> {
    static CONFIG: OnceLock<RwLock<Option<HomeboyConfig>>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(None))
}

fn cached_config() -> Option<HomeboyConfig> {
    match config_cache().read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn store_cached_config(config: &HomeboyConfig) {
    match config_cache().write() {
        Ok(mut slot) => *slot = Some(config.clone()),
        Err(poisoned) => *poisoned.into_inner() = Some(config.clone()),
    }
}

fn clear_config_cache() {
    match config_cache().write() {
        Ok(mut slot) => *slot = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

#[cfg(test)]
pub(crate) fn reset_config_cache_for_test() {
    clear_config_cache();
}

/// Attempt to load config from the product config file.
fn load_config_from_file() -> crate::core::Result<HomeboyConfig> {
    let path = paths::homeboy_json()?;

    if !path.exists() {
        return Err(crate::core::Error::internal_io(
            format!(
                "{} not found",
                crate::core::product_identity::PRODUCT_IDENTITY.config_filename
            ),
            Some(path.display().to_string()),
        ));
    }

    let content = local_files::read_file(&path, &format!("read {}", path.display()))?;

    let config: HomeboyConfig = serde_json::from_str(&content).map_err(|e| {
        crate::core::Error::validation_invalid_json(
            e,
            Some(format!(
                "parse {}",
                crate::core::product_identity::PRODUCT_IDENTITY.config_filename
            )),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(config)
}

/// Save config to the product config file (creates if missing).
pub fn save_config(config: &HomeboyConfig) -> crate::core::Result<()> {
    let path = paths::homeboy_json()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::core::Error::internal_io(
                e.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }

    let content = crate::core::config::to_string_pretty(config)?;

    local_files::write_file_atomic(&path, &content, &format!("write {}", path.display()))?;
    store_cached_config(config);

    Ok(())
}

/// Check if the product config file exists.
pub fn config_exists() -> bool {
    paths::homeboy_json().map(|p| p.exists()).unwrap_or(false)
}

/// Delete the product config file (reset to defaults).
pub fn reset_config() -> crate::core::Result<bool> {
    let path = paths::homeboy_json()?;

    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            crate::core::Error::internal_io(
                e.to_string(),
                Some(format!("delete {}", path.display())),
            )
        })?;
        clear_config_cache();
        Ok(true)
    } else {
        clear_config_cache();
        Ok(false)
    }
}

/// Get the product config path (for display purposes).
pub fn config_path() -> crate::core::Result<String> {
    Ok(paths::homeboy_json()?.display().to_string())
}
