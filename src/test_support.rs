use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

pub(crate) struct HomeGuard {
    prior: Option<String>,
    dir: TempDir,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditGuard {
    _guard: MutexGuard<'static, ()>,
}

impl AuditGuard {
    pub(crate) fn new() -> Self {
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self { _guard: guard }
    }
}

impl HomeGuard {
    pub(crate) fn new() -> Self {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("HOME").ok();
        let dir = TempDir::new().expect("home tempdir");
        std::env::set_var("HOME", dir.path());
        Self {
            prior,
            dir,
            _guard: guard,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

pub(crate) fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    body(&home.dir)
}

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn audit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
