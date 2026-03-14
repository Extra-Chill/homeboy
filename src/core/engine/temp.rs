use crate::error::{Error, Result};
use crate::paths;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const HOMEBOY_RUNTIME_TMPDIR_ENV: &str = "HOMEBOY_RUNTIME_TMPDIR";

/// Maximum age for sandbox directories before they are pruned (1 hour).
const STALE_SANDBOX_MAX_AGE: Duration = Duration::from_secs(3600);

/// Prefix used by refactor sandbox directories.
const SANDBOX_PREFIX: &str = "homeboy-refactor-ci-";

fn runtime_root() -> Result<PathBuf> {
    if let Ok(override_dir) = env::var(HOMEBOY_RUNTIME_TMPDIR_ENV) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    Ok(paths::homeboy()?.join("runtime").join("tmp"))
}

pub fn ensure_runtime_tmp_dir() -> Result<PathBuf> {
    let runtime_dir = runtime_root()?;
    fs::create_dir_all(&runtime_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create homeboy runtime tmp directory".to_string()),
        )
    })?;

    // Prune stale sandbox directories left behind by killed processes.
    prune_stale_sandboxes(&runtime_dir);

    Ok(runtime_dir)
}

/// Remove sandbox directories older than `STALE_SANDBOX_MAX_AGE`.
///
/// Sandboxes are created by `SandboxDir` (refactor pipeline) and normally
/// cleaned up by its `Drop` impl. When the process is killed by a signal
/// (SIGKILL, SIGTERM, Ctrl+C), `Drop` never fires and directories accumulate.
/// This sweeper runs on the next `ensure_runtime_tmp_dir()` call and prunes
/// any orphans.
fn prune_stale_sandboxes(runtime_dir: &Path) {
    let now = SystemTime::now();

    let entries = match fs::read_dir(runtime_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.starts_with(SANDBOX_PREFIX) {
            continue;
        }

        if !entry.path().is_dir() {
            continue;
        }

        let is_stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_SANDBOX_MAX_AGE);

        if is_stale {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

pub fn runtime_temp_file(prefix: &str, suffix: &str) -> Result<PathBuf> {
    Ok(ensure_runtime_tmp_dir()?.join(unique_name(prefix, suffix)))
}

pub fn runtime_temp_dir(prefix: &str) -> Result<PathBuf> {
    let path = ensure_runtime_tmp_dir()?.join(unique_name(prefix, ""));
    fs::create_dir_all(&path).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("create temp dir {prefix}")))
    })?;
    Ok(path)
}

fn unique_name(prefix: &str, suffix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    format!("{prefix}-{}-{nanos}{suffix}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn runtime_temp_file_honors_override() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempfile::tempdir().expect("tempdir");
        unsafe {
            env::set_var(HOMEBOY_RUNTIME_TMPDIR_ENV, dir.path());
        }

        let path = runtime_temp_file("homeboy-test", ".json").expect("temp file path");
        assert!(path.starts_with(dir.path()));
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".json"));

        unsafe {
            env::remove_var(HOMEBOY_RUNTIME_TMPDIR_ENV);
        }
    }

    #[test]
    fn runtime_temp_dir_honors_override() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempfile::tempdir().expect("tempdir");
        unsafe {
            env::set_var(HOMEBOY_RUNTIME_TMPDIR_ENV, dir.path());
        }

        let path = runtime_temp_dir("homeboy-test-dir").expect("temp dir path");
        assert!(path.starts_with(dir.path()));
        assert!(path.is_dir());

        unsafe {
            env::remove_var(HOMEBOY_RUNTIME_TMPDIR_ENV);
        }
    }

    #[test]
    fn prune_removes_stale_sandbox_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a "stale" sandbox directory and backdate its mtime.
        let stale = tmp.path().join("homeboy-refactor-ci-aaaa-1111");
        fs::create_dir(&stale).expect("create stale dir");
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(two_hours_ago))
            .expect("set mtime");

        // Create a "fresh" sandbox directory (just created, mtime = now).
        let fresh = tmp.path().join("homeboy-refactor-ci-bbbb-2222");
        fs::create_dir(&fresh).expect("create fresh dir");

        // Create a non-sandbox directory that should be left alone.
        let other = tmp.path().join("some-other-dir");
        fs::create_dir(&other).expect("create other dir");

        prune_stale_sandboxes(tmp.path());

        assert!(!stale.exists(), "stale sandbox should be removed");
        assert!(fresh.exists(), "fresh sandbox should be kept");
        assert!(other.exists(), "non-sandbox dir should be untouched");
    }

    #[test]
    fn prune_ignores_non_directory_files() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a file (not a directory) with the sandbox prefix.
        let file_path = tmp.path().join("homeboy-refactor-ci-file-3333");
        fs::write(&file_path, "not a dir").expect("write file");
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(
            &file_path,
            filetime::FileTime::from_system_time(two_hours_ago),
        )
        .expect("set mtime");

        prune_stale_sandboxes(tmp.path());

        assert!(
            file_path.exists(),
            "non-directory file should not be removed"
        );
    }
}
