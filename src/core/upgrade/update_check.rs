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
use crate::core::extension::update_check::now_unix;
use crate::core::extension::update_check::write_cache;
use crate::core::extension::update_check::cache_path;
use crate::core::extension::update_check::read_cache;
use crate::core::extension::update_check::is_disabled_by_config;

const CACHE_FILENAME: &str = "update_check.json";
const CHECK_INTERVAL_SECS: u64 = 86400;
const ENV_VAR_DISABLE: &str = "HOMEBOY_NO_UPDATE_CHECK";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckCache {
    pub latest_version: String,
    pub current_version: String,
    pub update_available: bool,
    pub checked_at: u64,
}

fn is_cache_fresh(cache: &UpdateCheckCache) -> bool {
    let elapsed = now_unix().saturating_sub(cache.checked_at);
    elapsed < CHECK_INTERVAL_SECS && cache.current_version == upgrade::current_version()
}

fn is_disabled_by_env() -> bool {
    std::env::var(ENV_VAR_DISABLE)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn print_hint(latest: &str, current: &str) {
    log_status!(
        "update",
        "Homeboy {} is available (current: {}). Run `homeboy upgrade` to update.",
        latest,
        current
    );
}

pub fn run_startup_check() {
    if is_disabled_by_env() || is_disabled_by_config() {
        return;
    }

    let mut already_printed = false;
    let cached = read_cache();

    if let Some(ref cache) = cached {
        if cache.update_available && cache.current_version == upgrade::current_version() {
            print_hint(&cache.latest_version, upgrade::current_version());
            already_printed = true;
        }

        if is_cache_fresh(cache) {
            return;
        }
    }

    let check = match upgrade::check_for_updates() {
        Ok(check) => check,
        Err(_) => return,
    };

    write_cache(&UpdateCheckCache {
        latest_version: check.latest_version.clone().unwrap_or_default(),
        current_version: check.current_version.clone(),
        update_available: check.update_available,
        checked_at: now_unix(),
    });

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
    fn test_env_var_disable() {
        std::env::remove_var(ENV_VAR_DISABLE);
        assert!(!is_disabled_by_env());

        std::env::set_var(ENV_VAR_DISABLE, "1");
        assert!(is_disabled_by_env());

        std::env::set_var(ENV_VAR_DISABLE, "True");
        assert!(is_disabled_by_env());

        std::env::set_var(ENV_VAR_DISABLE, "0");
        assert!(!is_disabled_by_env());

        std::env::remove_var(ENV_VAR_DISABLE);
    }
}
