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
//!
//! Cache I/O primitives are shared with the extension update check via
//! [`homeboy_core::update_check_cache`]. The on-disk filename and JSON
//! schema live here and are unchanged.

use crate::upgrade;
use homeboy_core::update_check_cache;
use serde::{Deserialize, Serialize};

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

fn read_cache() -> Option<UpdateCheckCache> {
    update_check_cache::read_cache(CACHE_FILENAME)
}

/// Return the latest stable version already admitted by the normal update
/// check. Durable admission deliberately never performs network I/O: an
/// unavailable cache is explicit offline evidence, not a reason to guess.
pub fn latest_allowed_stable() -> Option<String> {
    if is_disabled() {
        return None;
    }
    read_cache().and_then(|cache| {
        (is_cache_fresh(&cache) && cache.update_available && !cache.latest_version.is_empty())
            .then_some(cache.latest_version)
    })
}

/// The latest published release recorded by the daily update check, whatever
/// the running binary's relation to it, plus when that check ran.
///
/// [`latest_allowed_stable`] deliberately answers a narrower question — "is
/// there a newer stable release this controller is allowed to move to" — and
/// therefore withholds the version when no update is available. Controller
/// staleness reporting needs the recorded release unconditionally: reporting
/// `current` requires knowing what the latest release *is*, not merely that it
/// is not newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedLatestRelease {
    pub latest_version: String,
    pub checked_at: u64,
}

/// Read the cached latest release. Pure cache read — never performs network
/// I/O, so a caller on every command costs one small file read and the daily
/// refresh stays the only network call.
///
/// A stale cache is returned rather than discarded: an offline host's day-old
/// release list is still the best evidence available, and `checked_at` lets the
/// caller say how old the verdict is instead of silently reporting nothing.
pub fn cached_latest_release() -> Option<CachedLatestRelease> {
    let cache = read_cache()?;
    let latest_version = cache.latest_version.trim().to_string();
    (!latest_version.is_empty()).then_some(CachedLatestRelease {
        latest_version,
        checked_at: cache.checked_at,
    })
}

/// Whether the operator has turned the update check off, by environment or by
/// config. Callers that report freshness must treat this as "not established"
/// rather than "current".
pub fn is_disabled() -> bool {
    is_disabled_by_env() || is_disabled_by_config()
}

fn write_cache(cache: &UpdateCheckCache) {
    update_check_cache::write_cache(CACHE_FILENAME, cache);
}

fn is_cache_fresh(cache: &UpdateCheckCache) -> bool {
    update_check_cache::is_cache_fresh(cache.checked_at, CHECK_INTERVAL_SECS)
        && cache.current_version == upgrade::current_version()
}

fn is_disabled_by_env() -> bool {
    std::env::var(ENV_VAR_DISABLE)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub(crate) fn is_disabled_by_config() -> bool {
    !homeboy_core::defaults::load_config().update_check
}

/// One-line update hint.
///
/// The wording is derived from the shared controller-staleness assessment, so a
/// controller more than one minor release behind reads as a warning instead of
/// a neutral "an update is available" note (#11483). Because this runs at the
/// start of every non-upgrade command, it is also how a long-running command
/// such as `cook` carries the same verdict `homeboy status` reports.
///
/// When the assessment does not establish staleness — an unparseable version,
/// or a "newer" release that semver does not agree is newer — the original
/// neutral hint is emitted rather than nothing, so this can only add signal.
fn print_hint(latest: &str, checked_at: Option<u64>) {
    let staleness = crate::controller_staleness::assess(
        &homeboy_core::build_identity::current(),
        Some(latest),
        checked_at,
        update_check_cache::now_unix(),
    );

    match staleness.warning_line() {
        Some(line) => homeboy_core::log_status!("update", "{}", line),
        None => homeboy_core::log_status!(
            "update",
            "Homeboy {} is available (current: {}). Run `homeboy upgrade` to update.",
            latest,
            upgrade::current_version()
        ),
    }
}

pub fn run_startup_check() {
    if is_disabled() {
        return;
    }

    let mut already_printed = false;
    let cached = read_cache();

    if let Some(ref cache) = cached {
        if cache.update_available && cache.current_version == upgrade::current_version() {
            print_hint(&cache.latest_version, Some(cache.checked_at));
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

    let checked_at = update_check_cache::now_unix();
    write_cache(&UpdateCheckCache {
        latest_version: check.latest_version.clone().unwrap_or_default(),
        current_version: check.current_version.clone(),
        update_available: check.update_available,
        checked_at,
    });

    if !already_printed && check.update_available {
        if let Some(latest) = &check.latest_version {
            print_hint(latest, Some(checked_at));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use homeboy_core::test_support::with_isolated_home;

    /// Write a cache fixture the way the daily check would.
    ///
    /// `write_cache` swallows I/O errors by design (the update check must never
    /// fail a command), so the fixture asserts the file landed — otherwise a
    /// missing directory would present as a silently empty cache and every
    /// assertion below would fail for the wrong reason.
    fn seed_cache(latest_version: &str, update_available: bool, checked_at: u64) {
        let path = update_check_cache::cache_path(CACHE_FILENAME).expect("cache path resolves");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create cache directory");
        }

        write_cache(&UpdateCheckCache {
            latest_version: latest_version.to_string(),
            current_version: upgrade::current_version().to_string(),
            update_available,
            checked_at,
        });

        assert!(path.is_file(), "cache fixture was written to {path:?}");
    }

    /// The staleness surface reads this cache on every command, so the read must
    /// be a pure file read that round-trips what the daily check wrote.
    #[test]
    fn cached_latest_release_round_trips_the_daily_cache() {
        with_isolated_home(|_home| {
            let checked_at = update_check_cache::now_unix();
            seed_cache("0.329.1", true, checked_at);

            let cached = cached_latest_release().expect("cache is readable");
            assert_eq!(cached.latest_version, "0.329.1");
            assert_eq!(cached.checked_at, checked_at);
        });
    }

    #[test]
    fn cached_latest_release_is_absent_without_a_cache_file() {
        with_isolated_home(|_home| {
            assert!(cached_latest_release().is_none());
        });
    }

    /// A cache written by a failed network check carries an empty version. That
    /// is not a release to compare against, so it must read as absent rather
    /// than as an empty "latest".
    #[test]
    fn cached_latest_release_rejects_an_empty_version() {
        with_isolated_home(|_home| {
            seed_cache("   ", false, update_check_cache::now_unix());

            assert!(cached_latest_release().is_none());
        });
    }

    /// A day-old cache on an offline host is still the best evidence available.
    /// It is returned with its timestamp so the caller can report the age,
    /// rather than discarded — which would report "unknown" on every command
    /// once the host loses network.
    #[test]
    fn cached_latest_release_returns_a_stale_cache_with_its_timestamp() {
        with_isolated_home(|_home| {
            let old = update_check_cache::now_unix() - (CHECK_INTERVAL_SECS * 3);
            seed_cache("0.329.1", true, old);

            let cache = read_cache().expect("cache is readable");
            assert!(!is_cache_fresh(&cache), "fixture must be past the interval");

            let cached = cached_latest_release().expect("stale cache is still returned");
            assert_eq!(cached.latest_version, "0.329.1");
            assert_eq!(cached.checked_at, old);
        });
    }

    /// The reason this accessor exists: `latest_allowed_stable` withholds the
    /// version when no update is available, but reporting a controller as
    /// *current* requires knowing which release it is current with.
    #[test]
    fn cached_latest_release_is_returned_when_no_update_is_available() {
        with_isolated_home(|_home| {
            // The hermetic home disables the update check; re-enable it so this
            // exercises the enabled path. The guard restores the variable.
            std::env::remove_var(ENV_VAR_DISABLE);
            seed_cache("0.329.1", false, update_check_cache::now_unix());

            assert!(latest_allowed_stable().is_none());
            assert_eq!(
                cached_latest_release().map(|cached| cached.latest_version),
                Some("0.329.1".to_string())
            );
        });
    }

    #[test]
    fn disabled_by_env_reports_disabled() {
        with_isolated_home(|_home| {
            std::env::remove_var(ENV_VAR_DISABLE);
            assert!(!is_disabled());

            std::env::set_var(ENV_VAR_DISABLE, "1");
            assert!(is_disabled());
        });
    }

    #[test]
    fn test_env_var_disable() {
        // Serialized against the other env-mutating tests in this module by the
        // hermetic home guard, which also restores `HOMEBOY_NO_UPDATE_CHECK`.
        with_isolated_home(|_home| {
            std::env::remove_var(ENV_VAR_DISABLE);
            assert!(!is_disabled_by_env());

            std::env::set_var(ENV_VAR_DISABLE, "1");
            assert!(is_disabled_by_env());

            std::env::set_var(ENV_VAR_DISABLE, "True");
            assert!(is_disabled_by_env());

            std::env::set_var(ENV_VAR_DISABLE, "0");
            assert!(!is_disabled_by_env());

            std::env::remove_var(ENV_VAR_DISABLE);
        });
    }
}
