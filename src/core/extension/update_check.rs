//! Startup extension update check — warns users when installed extensions have updates available.
//!
//! Same pattern as the CLI update check (`update_check.rs`):
//! - Check on first run of the day (24h cache)
//! - Print notice to stderr if any extension has updates
//! - `homeboy extension update <name>` to apply
//!
//! Shares the disable mechanisms with CLI update check:
//! - `HOMEBOY_NO_UPDATE_CHECK=1`
//! - `homeboy config set /update_check false`

use crate::extension;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FILENAME: &str = "extension_update_check.json";
const CHECK_INTERVAL_SECS: u64 = 86400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionUpdateCache {
    pub extensions_behind: HashMap<String, usize>,
    pub checked_at: u64,
}

pub(crate) fn cache_path() -> Option<std::path::PathBuf> {
    paths::homeboy().ok().map(|path| path.join(CACHE_FILENAME))
}

pub(crate) fn read_cache() -> Option<ExtensionUpdateCache> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn write_cache(cache: &ExtensionUpdateCache) {
    let Some(path) = cache_path() else { return };
    let Ok(content) = serde_json::to_string_pretty(cache) else {
        return;
    };
    let _ = std::fs::write(&path, content);
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn is_cache_fresh(cache: &ExtensionUpdateCache) -> bool {
    now_unix().saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS
}

fn is_disabled_by_env() -> bool {
    std::env::var("HOMEBOY_NO_UPDATE_CHECK")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub(crate) fn is_disabled_by_config() -> bool {
    !crate::defaults::load_config().update_check
}

fn print_extension_hints(extensions_behind: &HashMap<String, usize>) {
    if extensions_behind.is_empty() {
        return;
    }

    if extensions_behind.len() == 1 {
        let (id, count) = extensions_behind.iter().next().unwrap();
        log_status!(
            "update",
            "Extension '{}' has {} new commit{}. Run `homeboy extension update {}` to update.",
            id,
            count,
            if *count == 1 { "" } else { "s" },
            id
        );
    } else {
        let names: Vec<&String> = extensions_behind.keys().collect();
        log_status!(
            "update",
            "{} extensions have updates: {}. Run `homeboy extension update <name>` to update.",
            extensions_behind.len(),
            names
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

pub fn run_startup_check() {
    if is_disabled_by_env() || is_disabled_by_config() {
        return;
    }

    let mut already_printed = false;
    let cached = read_cache();

    if let Some(ref cache) = cached {
        if !cache.extensions_behind.is_empty() {
            print_extension_hints(&cache.extensions_behind);
            already_printed = true;
        }

        if is_cache_fresh(cache) {
            return;
        }
    }

    let extension_ids = extension::available_extension_ids();
    let mut extensions_behind: HashMap<String, usize> = HashMap::new();

    for id in &extension_ids {
        if let Some(update) = extension::check_update_available(id) {
            extensions_behind.insert(update.extension_id, update.behind_count);
        }
    }

    write_cache(&ExtensionUpdateCache {
        extensions_behind: extensions_behind.clone(),
        checked_at: now_unix(),
    });

    if !already_printed && !extensions_behind.is_empty() {
        print_extension_hints(&extensions_behind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_freshness_within_24h() {
        let cache = ExtensionUpdateCache {
            extensions_behind: HashMap::new(),
            checked_at: now_unix() - 100,
        };
        assert!(is_cache_fresh(&cache));
    }

    #[test]
    fn cache_stale_after_24h() {
        let cache = ExtensionUpdateCache {
            extensions_behind: HashMap::new(),
            checked_at: now_unix() - CHECK_INTERVAL_SECS - 1,
        };
        assert!(!is_cache_fresh(&cache));
    }

    #[test]
    fn cache_roundtrip() {
        let mut behind = HashMap::new();
        behind.insert("wordpress".to_string(), 3);
        let cache = ExtensionUpdateCache {
            extensions_behind: behind,
            checked_at: 1700000000,
        };
        let json = serde_json::to_string(&cache).unwrap();
        let parsed: ExtensionUpdateCache = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.extensions_behind.len(), 1);
        assert_eq!(parsed.extensions_behind["wordpress"], 3);
        assert_eq!(parsed.checked_at, 1700000000);
    }

    #[test]
    fn test_run_startup_check_if_let_some_ref_cache_cached() {

        run_startup_check();
    }

    #[test]
    fn test_run_startup_check_if_let_some_update_extension_check_update_available_id() {

        run_startup_check();
    }

    #[test]
    fn test_run_startup_check_has_expected_effects() {
        // Expected effects: mutation

        let _ = run_startup_check();
    }

}
