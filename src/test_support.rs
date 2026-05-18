use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

pub(crate) struct HomeGuard {
    prior: Option<String>,
    prior_xdg_data_home: Option<String>,
    prior_artifact_root: Option<String>,
    prior_invocation_runtime: Option<String>,
    dir: TempDir,
    /// Held alongside `dir` so the short invocation runtime tempdir is
    /// dropped only after the test completes. Distinct from `dir` so the
    /// invocation root can live on a short path (e.g. `/tmp/hb-XXXX`)
    /// regardless of where `$TMPDIR` lands.
    _inv_dir: Option<TempDir>,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditGuard {
    _guard: MutexGuard<'static, ()>,
    _home_guard: MutexGuard<'static, ()>,
}

impl AuditGuard {
    pub(crate) fn new() -> Self {
        let home_guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _guard: guard,
            _home_guard: home_guard,
        }
    }
}

impl HomeGuard {
    pub(crate) fn new() -> Self {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("HOME").ok();
        let prior_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let prior_artifact_root = std::env::var("HOMEBOY_ARTIFACT_ROOT").ok();
        let prior_invocation_runtime =
            std::env::var(crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        let dir = TempDir::new().expect("home tempdir");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_DATA_HOME", dir.path().join(".local").join("share"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
        crate::core::set_artifact_root_override(None);
        // Pin invocation runtime to a SHORT tempdir, isolated from `$TMPDIR`
        // and from the home tempdir (which itself can already live on a long
        // path on macOS, e.g. `/var/folders/<14>/T/.tmpXXXXXX/...`). Using
        // `/tmp` directly keeps tests within the platform `sockaddr_un`
        // budget regardless of host configuration.
        let inv_dir = short_invocation_tempdir();
        std::env::set_var(
            crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            inv_dir.path(),
        );
        Self {
            prior,
            prior_xdg_data_home,
            prior_artifact_root,
            prior_invocation_runtime,
            dir,
            _inv_dir: Some(inv_dir),
            _guard: guard,
        }
    }
}

/// Return a short-path tempdir suitable for the invocation runtime root.
///
/// On Unix-like systems we anchor to `/tmp` so the path stays well under
/// `sockaddr_un` even on macOS where `$TMPDIR` typically lives at
/// `/var/folders/<14>/T/.tmpXXXXXX/`. Falls back to the default tempdir
/// otherwise (Windows / odd hosts).
fn short_invocation_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        if std::path::Path::new("/tmp").is_dir() {
            return tempfile::Builder::new()
                .prefix("hb-test-")
                .tempdir_in("/tmp")
                .expect("invocation runtime tempdir under /tmp");
        }
    }
    TempDir::new().expect("invocation runtime tempdir")
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.prior_artifact_root {
            Some(value) => std::env::set_var("HOMEBOY_ARTIFACT_ROOT", value),
            None => std::env::remove_var("HOMEBOY_ARTIFACT_ROOT"),
        }
        crate::core::set_artifact_root_override(None);
        match &self.prior_invocation_runtime {
            Some(value) => std::env::set_var(
                crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                value,
            ),
            None => std::env::remove_var(
                crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            ),
        }
    }
}

pub(crate) fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    body(&home.dir)
}

pub(crate) fn home_env_guard() -> MutexGuard<'static, ()> {
    home_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn audit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
