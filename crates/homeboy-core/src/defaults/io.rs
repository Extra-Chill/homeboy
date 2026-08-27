use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::Value;

use crate::engine::local_files;
use crate::paths;

use super::HomeboyConfig;

/// The product config file below an explicitly resolved config root.
///
/// This is exactly what [`paths::homeboy_json`] computes, minus the ambient
/// resolution of the root itself.
fn config_file_path_in_root(config_root: &Path) -> PathBuf {
    paths::homeboy_json_in_root(config_root)
}

/// Load the full product config, falling back to defaults on any error.
/// Warns to stderr if the file exists but fails to parse, so the user knows
/// their config is being ignored rather than silently resetting to defaults.
///
/// The memoized value is process-global and is deliberately keyed on nothing:
/// it caches the *ambient* configuration only. The rooted siblings below never
/// read or write it, so injecting a root cannot be answered out of another
/// root's cache.
pub fn load_config() -> HomeboyConfig {
    if let Some(config) = cached_config() {
        return config;
    }

    let config = match paths::homeboy() {
        Ok(config_root) => load_config_uncached_in_root(&config_root),
        // An unresolvable config root is exactly what `homeboy_json()` used to
        // fail on here: `load_config_from_file` surfaced it as an error, and
        // `config_exists()` was false, so this degraded to defaults silently.
        Err(_) => HomeboyConfig::default(),
    };
    store_cached_config(&config);
    config
}

/// Load the ambient config under a shared, bounded lock for an interactive
/// read. A stale lock file is harmless: `flock` releases its lease when the
/// former holder exits, so the next reader safely reclaims access. Unlike a
/// mutation, a read remains bounded even when its timeout override is `0`.
pub fn load_config_for_read() -> crate::Result<HomeboyConfig> {
    let config_root = paths::homeboy()?;
    crate::config::with_config_read_lock_at(&config_root, || {
        Ok(load_config_uncached_in_root(&config_root))
    })
}

/// Load the effective config and one raw file-backed value under the same
/// bounded shared lock, avoiding a provenance race with a concurrent writer.
pub fn load_config_and_file_value_for_read(
    pointer: &str,
) -> crate::Result<(HomeboyConfig, Option<Value>)> {
    let config_root = paths::homeboy()?;
    crate::config::with_config_read_lock_at(&config_root, || {
        Ok((
            load_config_uncached_in_root(&config_root),
            config_file_value_in_root(&config_root, pointer),
        ))
    })
}

/// [`load_config`] against an explicitly injected config root, without the
/// process-global memoization.
pub fn load_config_uncached_in_root(config_root: &Path) -> HomeboyConfig {
    match load_config_from_file_in_root(config_root) {
        Ok(config) => config,
        Err(err) => {
            // Only warn if the file actually exists — missing file is expected
            if config_exists_in_root(config_root) {
                log_status!(
                    "config",
                    "Warning: failed to load {} ({}), using defaults",
                    crate::product_identity::PRODUCT_IDENTITY.config_filename,
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

#[cfg(any(test, feature = "test-support"))]
pub fn reset_config_cache_for_test() {
    clear_config_cache();
}

/// Attempt to load config from the product config file.
fn load_config_from_file_in_root(config_root: &Path) -> crate::Result<HomeboyConfig> {
    let path = config_file_path_in_root(config_root);

    if !path.exists() {
        return Err(crate::Error::internal_io(
            format!(
                "{} not found",
                crate::product_identity::PRODUCT_IDENTITY.config_filename
            ),
            Some(path.display().to_string()),
        ));
    }

    let content = local_files::read_file(&path, &format!("read {}", path.display()))?;

    let config: HomeboyConfig = serde_json::from_str(&content).map_err(|e| {
        crate::Error::validation_invalid_json(
            e,
            Some(format!(
                "parse {}",
                crate::product_identity::PRODUCT_IDENTITY.config_filename
            )),
            Some(content.chars().take(200).collect::<String>()),
        )
    })?;

    Ok(config)
}

/// Save config to the product config file (creates if missing).
pub fn save_config(config: &HomeboyConfig) -> crate::Result<()> {
    let config_root = paths::homeboy()?;
    save_config_in_root(&config_root, config)?;
    // Priming the memo stays on the ambient path deliberately. The memo is not
    // keyed by root, so a rooted write that primed it would make a later
    // ambient `load_config()` answer out of a foreign installation.
    store_cached_config(config);
    Ok(())
}

/// [`save_config`] against an explicitly injected config root.
pub fn save_config_in_root(config_root: &Path, config: &HomeboyConfig) -> crate::Result<()> {
    let path = config_file_path_in_root(config_root);

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

/// Check if the product config file exists.
pub fn config_exists() -> bool {
    paths::homeboy()
        .map(|root| config_exists_in_root(&root))
        .unwrap_or(false)
}

/// [`config_exists`] against an explicitly injected config root.
pub fn config_exists_in_root(config_root: &Path) -> bool {
    config_file_path_in_root(config_root).exists()
}

/// Return an explicitly configured value from the on-disk file.
///
/// The effective configuration adds serde defaults, so callers that need to
/// report ownership must distinguish a file override from a built-in value.
pub fn config_file_value(pointer: &str) -> Option<Value> {
    let config_root = paths::homeboy().ok()?;
    config_file_value_in_root(&config_root, pointer)
}

/// [`config_file_value`] against an explicitly injected config root.
pub fn config_file_value_in_root(config_root: &Path, pointer: &str) -> Option<Value> {
    let path = config_file_path_in_root(config_root);
    let content = local_files::read_file(&path, &format!("read {}", path.display())).ok()?;
    serde_json::from_str::<HomeboyConfig>(&content).ok()?;
    let value = serde_json::from_str::<Value>(&content).ok()?;
    crate::config::get_json_pointer(&value, pointer)
        .ok()?
        .cloned()
}

/// Delete the product config file (reset to defaults).
pub fn reset_config() -> crate::Result<bool> {
    let config_root = paths::homeboy()?;
    reset_config_in_root(&config_root)
}

/// [`reset_config`] against an explicitly injected config root.
///
/// Invalidating the unkeyed memo from a rooted call is conservative: it can
/// only force a re-read, never serve another root's value.
pub fn reset_config_in_root(config_root: &Path) -> crate::Result<bool> {
    let path = config_file_path_in_root(config_root);

    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
        })?;
        clear_config_cache();
        Ok(true)
    } else {
        clear_config_cache();
        Ok(false)
    }
}

/// Get the product config path (for display purposes).
pub fn config_path() -> crate::Result<String> {
    let config_root = paths::homeboy()?;
    Ok(config_path_in_root(&config_root))
}

/// [`config_path`] against an explicitly injected config root.
pub fn config_path_in_root(config_root: &Path) -> String {
    config_file_path_in_root(config_root).display().to_string()
}
