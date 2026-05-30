//! Short, platform-aware invocation runtime root.
//!
//! Downstream workloads place UNIX domain sockets, FIFOs, and other
//! path-length-sensitive primitives under `HOMEBOY_INVOCATION_STATE_DIR`.
//! macOS `sockaddr_un` only accepts socket paths up to 104 bytes (108 on
//! Linux), so the prefix that Homeboy hands out must stay short enough for
//! a realistic workload-relative socket name to fit underneath without any
//! per-workload defense.
//!
//! ## Root selection
//!
//! In priority order:
//! 1. `HOMEBOY_INVOCATION_RUNTIME_DIR` env override (tests, advanced users).
//! 2. `/tmp/hb` on macOS / Linux when `/tmp` is a writable directory. macOS
//!    apps that respect `$TMPDIR` get per-user isolation under
//!    `/var/folders/<14>/T/...` which is already ~50 bytes — anchoring the
//!    runtime root to `/tmp` saves ~35 bytes of `sockaddr_un` budget per
//!    invocation, the difference between "fits" and "EINVAL on bind".
//! 3. `$XDG_RUNTIME_DIR/hb` on Linux when set and `/tmp` is not usable.
//! 4. `$TMPDIR/hb` on macOS as a last resort before falling back to the
//!    user cache directory. Only reached when `/tmp` is missing, which is
//!    not a real macOS configuration.
//! 5. `~/.cache/homeboy/inv` fallback on every platform.
//!
//! Each invocation owns one short id; the directories are siblings of that
//! id under the chosen root:
//!
//! - `<root>/<short-id>`     → `HOMEBOY_INVOCATION_STATE_DIR` (the leaf the
//!   workload owns; downstream sockets bind directly here).
//! - `<root>/<short-id>.a`   → `HOMEBOY_INVOCATION_ARTIFACT_DIR`
//! - `<root>/<short-id>.t`   → `HOMEBOY_INVOCATION_TMP_DIR`
//!
//! There is no `s/a/t` subdir layer — that would burn `sockaddr_un` budget
//! for no isolation gain since the invocation is already 1:1 with a single
//! workload run. Workloads that need internal subdirs under STATE_DIR can
//! still create them, but they own the path-length budget at that point.
//!
//! ## Path budget
//!
//! [`enforce_path_budget`] checks that the longest hand-out path leaves at
//! least [`SOCKET_HEADROOM_BYTES`] bytes of headroom under the platform
//! `sockaddr_un` limit, and fails fast with a clear error when an unusually
//! long `$HOME` or `$TMPDIR` would otherwise hand out a directory that no
//! UDS-using workload can use. macOS reserves more headroom (48 bytes) than
//! Linux (32 bytes) because the macOS limit is 4 bytes shorter and
//! workloads commonly nest a workload-id segment plus a socket filename
//! (`<workload-id>/daemon/daemon.sock` ≈ 40 bytes) underneath.

use crate::core::error::{Error, Result};
#[cfg(windows)]
use crate::core::paths;
use std::env;
use std::path::{Path, PathBuf};

/// Override env var that pins the invocation runtime root to a specific path.
///
/// Primarily for tests and unusual host configurations. When set and
/// non-empty, it bypasses the platform detection ladder entirely.
pub const HOMEBOY_INVOCATION_RUNTIME_DIR_ENV: &str = "HOMEBOY_INVOCATION_RUNTIME_DIR";

/// Bytes of headroom reserved beneath `sockaddr_un` so workloads can append
/// a realistic workload-relative socket name (e.g.
/// `<workload-id>/daemon/daemon.sock`, ≈40 bytes) without overflowing.
///
/// macOS gets a larger reserve (48 bytes) than Linux (32 bytes) because the
/// macOS `sockaddr_un` limit is 4 bytes shorter (104 vs 108) *and*
/// workloads typically nest a workload-id segment under STATE_DIR. The
/// previous 32-byte reserve was insufficient under realistic Apple
/// `$TMPDIR` configurations and caused Studio's daemon to hit `EINVAL` on
/// bind despite the contract.
#[cfg(target_os = "macos")]
pub const SOCKET_HEADROOM_BYTES: usize = 48;
#[cfg(not(target_os = "macos"))]
pub const SOCKET_HEADROOM_BYTES: usize = 32;

/// Platform `sockaddr_un` `sun_path` capacity in bytes (excluding NUL).
#[cfg(target_os = "macos")]
pub const SUN_PATH_CAPACITY: usize = 104;
#[cfg(all(unix, not(target_os = "macos")))]
pub const SUN_PATH_CAPACITY: usize = 108;
#[cfg(not(unix))]
pub const SUN_PATH_CAPACITY: usize = 108;

/// Resolve the short, platform-aware root that holds invocation directories.
///
/// The root is *not* created on this call. Callers that allocate an
/// invocation directory under it are responsible for `create_dir_all`.
pub fn invocation_runtime_root() -> Result<PathBuf> {
    if let Some(override_root) = override_root() {
        return Ok(override_root);
    }

    #[cfg(unix)]
    {
        // Prefer `/tmp/hb` on every Unix host. On macOS the per-user
        // `$TMPDIR` (`/var/folders/<14>/T/...`) is already ~50 bytes long,
        // which leaves no realistic `sockaddr_un` budget after appending a
        // short id and a typical workload-relative socket name. `/tmp` is
        // ~5 bytes, gives us ~35 extra bytes of headroom, and is writable
        // on every standard macOS / Linux configuration.
        if Path::new("/tmp").is_dir() {
            return Ok(PathBuf::from("/tmp/hb"));
        }

        // Fallbacks for unusual hosts where `/tmp` is missing or not a
        // directory (containers with stripped layouts, etc.).
        #[cfg(target_os = "linux")]
        {
            if let Some(xdg) = non_empty_env("XDG_RUNTIME_DIR") {
                return Ok(PathBuf::from(xdg).join("hb"));
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(tmpdir) = non_empty_env("TMPDIR") {
                return Ok(PathBuf::from(tmpdir).join("hb"));
            }
        }
    }

    // Generic fallback: ~/.cache/homeboy/inv (also Windows).
    cache_fallback_root()
}

fn override_root() -> Option<PathBuf> {
    let raw = env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg_attr(windows, allow(dead_code))]
fn non_empty_env(key: &str) -> Option<String> {
    let raw = env::var(key).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn cache_fallback_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        // Reuse the existing homeboy() resolver on Windows; it lands under
        // %APPDATA%\homeboy which is the closest analog to a per-user cache
        // root and keeps the implementation simple.
        return Ok(paths::homeboy()?.join("inv"));
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg_cache) = non_empty_env("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(xdg_cache).join("homeboy").join("inv"));
        }
        let home = env::var("HOME").map_err(|_| {
            Error::internal_unexpected(
                "HOME environment variable not set on Unix-like system".to_string(),
            )
        })?;
        Ok(PathBuf::from(home)
            .join(".cache")
            .join("homeboy")
            .join("inv"))
    }
}

/// Verify that handing out `path` leaves room for a realistic
/// workload-relative socket name within the `sockaddr_un` budget.
///
/// Fails fast with a clear error message that names `sockaddr_un`, the
/// platform-specific limit, the actual headroom available, and the override
/// env var when the platform budget cannot accommodate at least
/// [`SOCKET_HEADROOM_BYTES`] beyond `path`'s length.
pub fn enforce_path_budget(path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    let path_len = path_str.len();
    // +1 reserves the path-separator byte before the workload's filename.
    let needed = path_len
        .saturating_add(1)
        .saturating_add(SOCKET_HEADROOM_BYTES);
    if needed > SUN_PATH_CAPACITY {
        let headroom = SUN_PATH_CAPACITY.saturating_sub(path_len).saturating_sub(1);
        return Err(Error::internal_unexpected(format!(
            "Homeboy invocation runtime path exceeds the platform sockaddr_un budget: \
             path is {path_len} bytes, leaving {headroom} bytes of headroom for a downstream \
             socket name, but Homeboy guarantees at least {SOCKET_HEADROOM_BYTES} bytes of \
             headroom under the {SUN_PATH_CAPACITY}-byte sun_path capacity. Set \
             {HOMEBOY_INVOCATION_RUNTIME_DIR_ENV} to a shorter root (path: {path_str})."
        )));
    }
    Ok(())
}

/// Generate a short opaque path component (10 lowercase hex chars).
///
/// 40 bits of entropy ≈ 1.1e12 unique values is far beyond the number of
/// concurrent invocations any single host will run. The lease index lock in
/// `InvocationGuard::acquire` guarantees uniqueness against the live set.
pub fn short_invocation_id() -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    uuid[..10].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::home_env_guard;

    #[test]
    fn override_env_takes_priority() {
        let _guard = home_env_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let prior = env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, dir.path());

        let root = invocation_runtime_root().expect("root resolves");
        assert_eq!(root, dir.path());

        match prior {
            Some(value) => env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, value),
            None => env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
        }
    }

    #[test]
    fn override_env_blank_falls_back_to_platform() {
        let _guard = home_env_guard();
        let prior = env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, "   ");

        let root = invocation_runtime_root().expect("root resolves");
        assert!(root.ends_with("hb") || root.ends_with("inv"));

        match prior {
            Some(value) => env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, value),
            None => env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
        }
    }

    #[test]
    fn short_id_is_ten_lowercase_hex_chars() {
        let id = short_invocation_id();
        assert_eq!(id.len(), 10, "id={id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "id should be lowercase hex: {id}"
        );
    }

    #[test]
    fn short_ids_are_unique_across_calls() {
        // 40 bits of entropy: collisions in 1000 calls are vanishingly rare.
        let mut ids = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(ids.insert(short_invocation_id()));
        }
    }

    #[test]
    fn budget_accepts_short_path() {
        let path = PathBuf::from("/tmp/hb/abc1234567/state");
        enforce_path_budget(&path).expect("short path fits");
    }

    #[test]
    fn budget_reserves_socket_headroom() {
        // Build a path that leaves <32 bytes under the platform limit.
        let mut s = String::from("/");
        // Aim for a path length that leaves headroom < SOCKET_HEADROOM_BYTES.
        let target_len = SUN_PATH_CAPACITY - SOCKET_HEADROOM_BYTES;
        while s.len() < target_len {
            s.push('a');
        }
        let path = PathBuf::from(s);
        let err = enforce_path_budget(&path).expect_err("should reject overlong path");
        let message = err.to_string();
        assert!(
            message.contains("sockaddr_un"),
            "error should mention sockaddr_un: {message}"
        );
        assert!(
            message.contains(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
            "error should suggest the override env: {message}"
        );
    }

    #[test]
    fn budget_headroom_meets_platform_minimum() {
        // For a maximum-length accepted path, the remaining bytes after
        // appending '/' + a platform-headroom socket filename must not
        // overflow the platform sun_path capacity.
        let mut s = String::from("/");
        let max_accepted = SUN_PATH_CAPACITY - SOCKET_HEADROOM_BYTES - 1;
        while s.len() < max_accepted {
            s.push('b');
        }
        let path = PathBuf::from(&s);
        enforce_path_budget(&path).expect("max-accepted path fits");

        // Verify the headroom: appending a SOCKET_HEADROOM_BYTES-sized
        // filename stays under cap.
        let appended_len = s.len() + 1 + SOCKET_HEADROOM_BYTES;
        assert!(
            appended_len <= SUN_PATH_CAPACITY,
            "appended path {appended_len} should fit in {SUN_PATH_CAPACITY}"
        );
    }

    #[test]
    fn macos_headroom_is_at_least_forty_eight_bytes() {
        // The macOS sockaddr_un limit is 4 bytes shorter than Linux and
        // workloads commonly nest a workload-id/daemon/daemon.sock segment
        // (~40 bytes) under STATE_DIR. The contract guarantees ≥48 bytes
        // of headroom on macOS so realistic workload paths always fit.
        let headroom = SOCKET_HEADROOM_BYTES;
        #[cfg(target_os = "macos")]
        assert!(
            headroom >= 48,
            "macOS headroom must be at least 48 bytes; got {headroom}"
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            headroom >= 32,
            "non-macOS headroom must be at least 32 bytes; got {headroom}"
        );
    }
}
