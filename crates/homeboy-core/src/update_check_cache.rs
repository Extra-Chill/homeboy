//! Shared on-disk cache primitives for daily update-check results.
//!
//! Both the CLI update check (`core/upgrade/update_check.rs`) and the
//! extension update check (`core/extension/update_check.rs`) cache the
//! same shape: a JSON blob plus a `checked_at` unix timestamp, written
//! to a file under [`paths::homeboy()`]. Each caller picks the cache
//! filename and the payload type it wants to persist.
//!
//! The on-disk filenames (`update_check.json`, `extension_update_check.json`)
//! and JSON schemas are owned by the callers and are intentionally not
//! changed here so existing user caches keep working.

use crate::paths;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix timestamp in seconds. Returns `0` if the system clock is
/// before the epoch (matching the prior per-module behavior).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Resolve the cache file under the homeboy data directory. Returns
/// `None` when the homeboy directory cannot be resolved (matching the
/// prior per-module behavior — callers treat this as "no cache").
pub fn cache_path(filename: &str) -> Option<PathBuf> {
    paths::homeboy().ok().map(|path| path.join(filename))
}

/// Read and deserialize a cache payload from `filename` under the
/// homeboy data directory. Returns `None` on any I/O or JSON error so
/// the caller falls through to a fresh fetch.
pub fn read_cache<T: DeserializeOwned>(filename: &str) -> Option<T> {
    let path = cache_path(filename)?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Serialize and write a cache payload to `filename` under the homeboy
/// data directory. Errors are swallowed: the update check is best-effort
/// and must never fail a user command.
pub fn write_cache<T: Serialize>(filename: &str, payload: &T) {
    let Some(path) = cache_path(filename) else {
        return;
    };
    let Ok(content) = serde_json::to_string_pretty(payload) else {
        return;
    };
    let _ = std::fs::write(&path, content);
}

/// Pure time-based freshness check: `now - checked_at < interval_secs`.
/// Saturating subtraction protects against clocks moving backwards.
pub fn is_cache_fresh(checked_at: u64, interval_secs: u64) -> bool {
    now_unix().saturating_sub(checked_at) < interval_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now_unix() {
        assert!(now_unix() > 0);
    }

    #[test]
    fn test_cache_path() {
        let path = cache_path("example_update_check.json").expect("homeboy path resolves");

        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("example_update_check.json")
        );
    }

    #[test]
    fn fresh_when_recent() {
        assert!(is_cache_fresh(now_unix().saturating_sub(10), 100));
    }

    #[test]
    fn stale_when_outside_interval() {
        assert!(!is_cache_fresh(now_unix().saturating_sub(200), 100));
    }

    #[test]
    fn stale_when_clock_skew() {
        // checked_at in the future — saturating_sub yields 0, treated as fresh.
        assert!(is_cache_fresh(now_unix() + 1_000, 100));
    }
}
