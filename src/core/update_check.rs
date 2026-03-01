//! Startup update check — warns users when a newer Homeboy version is available.
//!
//! On every command invocation, reads a local cache file. If the cache indicates
//! an update is available, prints a one-line hint to stderr. If the cache is stale
//! (older than 24 hours) or missing, fetches the latest version from the network
//! and refreshes the cache.
//!
//! Disable via:
//! - Environment variable: `HOMEBOY_NO_UPDATE_CHECK=1`
//! - Config: `homeboy config set /update_check false`

use crate::paths;
use crate::upgrade;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FILENAME: &str = "update_check.json";
const CHECK_INTERVAL_SECS: u64 = 86400; // 24 hours
const ENV_VAR_DISABLE: &str = "HOMEBOY_NO_UPDATE_CHECK";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckCache {
    pub latest_version: String,
    pub current_version: String,
    pub update_available: bool,
    pub checked_at: u64,
}

pub(crate) fn cache_path() -> Option<std::path::PathBuf> {
    paths::homeboy().ok().map(|p| p.join(CACHE_FILENAME))
}

fn read_cache() -> Option<UpdateCheckCache> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(cache: &UpdateCheckCache) {
    let Some(path) = cache_path() else { return };
    let Ok(content) = serde_json::to_string_pretty(cache) else {
        return;
    };
    let _ = std::fs::write(&path, content);
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_cache_fresh(cache: &UpdateCheckCache) -> bool {
    let elapsed = now_unix().saturating_sub(cache.checked_at);
    elapsed < CHECK_INTERVAL_SECS && cache.current_version == upgrade::current_version()
}

fn is_disabled_by_env() -> bool {
    std::env::var(ENV_VAR_DISABLE)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub(crate) fn is_disabled_by_config() -> bool {
    !crate::defaults::load_config().update_check
}

fn print_hint(latest: &str, current: &str) {
    log_status!(
        "update",
        "Homeboy {} is available (current: {}). Run `homeboy upgrade` to update.",
        latest,
        current
    );
}

/// Run the startup update check. Prints a hint to stderr if an update is available.
///
/// Silently returns on any error (network failure, parse error, config missing, etc.).
/// Call this from main.rs after arg parsing, skipping for `upgrade`/`update` commands.
pub fn run_startup_check() {
    if is_disabled_by_env() || is_disabled_by_config() {
        return;
    }

    let mut already_printed = false;

    // Read cache — if it has an update for our current version, print hint immediately
    let cached = read_cache();

    if let Some(ref cache) = cached {
        if cache.update_available && cache.current_version == upgrade::current_version() {
            print_hint(&cache.latest_version, upgrade::current_version());
            already_printed = true;
        }

        // If cache is fresh, no need to re-fetch
        if is_cache_fresh(cache) {
            return;
        }
    }

    // Cache is stale or missing — fetch latest version (may take a moment)
    let check = match upgrade::check_for_updates() {
        Ok(c) => c,
        Err(_) => return, // Network error — silently skip
    };

    // Write refreshed cache
    write_cache(&UpdateCheckCache {
        latest_version: check.latest_version.clone().unwrap_or_default(),
        current_version: check.current_version.clone(),
        update_available: check.update_available,
        checked_at: now_unix(),
    });

    // Print hint if update available and we haven't already printed from cache
    if !already_printed && check.update_available {
        if let Some(latest) = &check.latest_version {
            print_hint(latest, &check.current_version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_freshness_within_24h() {
        let cache = UpdateCheckCache {
            latest_version: "0.50.0".to_string(),
            current_version: upgrade::current_version().to_string(),
            update_available: true,
            checked_at: now_unix() - 100, // 100 seconds ago
        };
        assert!(is_cache_fresh(&cache));
    }

    #[test]
    fn test_cache_stale_after_24h() {
        let cache = UpdateCheckCache {
            latest_version: "0.50.0".to_string(),
            current_version: upgrade::current_version().to_string(),
            update_available: true,
            checked_at: now_unix() - CHECK_INTERVAL_SECS - 1,
        };
        assert!(!is_cache_fresh(&cache));
    }

    #[test]
    fn test_cache_stale_after_version_change() {
        let cache = UpdateCheckCache {
            latest_version: "0.50.0".to_string(),
            current_version: "0.40.0".to_string(), // Different from current binary
            update_available: true,
            checked_at: now_unix() - 100,
        };
        assert!(!is_cache_fresh(&cache));
    }

    #[test]
    fn test_cache_roundtrip() {
        let cache = UpdateCheckCache {
            latest_version: "1.0.0".to_string(),
            current_version: "0.47.0".to_string(),
            update_available: true,
            checked_at: 1700000000,
        };
        let json = serde_json::to_string(&cache).unwrap();
        let parsed: UpdateCheckCache = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.latest_version, "1.0.0");
        assert_eq!(parsed.current_version, "0.47.0");
        assert!(parsed.update_available);
        assert_eq!(parsed.checked_at, 1700000000);
    }

    #[test]
    fn test_env_var_disable() {
        // When not set, should not be disabled
        std::env::remove_var(ENV_VAR_DISABLE);
        assert!(!is_disabled_by_env());

        // When set to "1", should be disabled
        std::env::set_var(ENV_VAR_DISABLE, "1");
        assert!(is_disabled_by_env());

        // When set to "true" (case-insensitive), should be disabled
        std::env::set_var(ENV_VAR_DISABLE, "True");
        assert!(is_disabled_by_env());

        // When set to other values, should not be disabled
        std::env::set_var(ENV_VAR_DISABLE, "0");
        assert!(!is_disabled_by_env());

        // Clean up
        std::env::remove_var(ENV_VAR_DISABLE);
    }
}
