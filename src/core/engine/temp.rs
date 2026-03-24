use crate::error::{Error, Result};
use crate::paths;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const HOMEBOY_RUNTIME_TMPDIR_ENV: &str = "HOMEBOY_RUNTIME_TMPDIR";

/// Maximum age for legacy sandbox directories before they are pruned (1 hour).
/// These were created by the old refactor sandbox (removed in v0.87). The pruner
/// stays to clean up any stragglers left from previous versions.
const STALE_SANDBOX_MAX_AGE: Duration = Duration::from_secs(3600);

/// Prefix used by legacy refactor sandbox directories.
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

pub(crate) fn ensure_runtime_tmp_dir() -> Result<PathBuf> {
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

/// Remove legacy sandbox directories older than `STALE_SANDBOX_MAX_AGE`.
///
/// The refactor sandbox was removed in v0.87 — refactoring now operates
/// directly on the working tree with git as the rollback mechanism.
/// This sweeper stays to clean up any leftover sandbox directories from
/// previous versions.
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
    fn test_ensure_runtime_tmp_dir_default_path() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_some_create_homeboy_runtime_tmp_directory_to_string() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_default_path_2() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_ok_runtime_dir() {

        let result = ensure_runtime_tmp_dir();
        assert!(result.is_ok(), "expected Ok for: Ok(runtime_dir)");
    }

    #[test]
    fn test_runtime_temp_file_ok_ensure_runtime_tmp_dir_join_unique_name_prefix_suffix() {
        let prefix = "";
        let suffix = "";
        let result = runtime_temp_file(&prefix, &suffix);
        assert!(result.is_ok(), "expected Ok for: Ok(ensure_runtime_tmp_dir()?.join(unique_name(prefix, suffix)))");
    }

    #[test]
    fn test_runtime_temp_dir_error_internal_io_e_to_string_some_format_create_temp_dir_pr() {
        let prefix = "";
        let _result = runtime_temp_dir(&prefix);
    }

    #[test]
    fn test_runtime_temp_dir_default_path() {
        let prefix = "";
        let _result = runtime_temp_dir(&prefix);
    }

    #[test]
    fn test_runtime_temp_dir_ok_path() {
        let prefix = "";
        let result = runtime_temp_dir(&prefix);
        assert!(result.is_ok(), "expected Ok for: Ok(path)");
    }


    #[test]
    fn test_ensure_runtime_tmp_dir_default_path() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_some_create_homeboy_runtime_tmp_directory_to_string() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_default_path_2() {

        let _result = ensure_runtime_tmp_dir();
    }

    #[test]
    fn test_ensure_runtime_tmp_dir_ok_runtime_dir() {

        let result = ensure_runtime_tmp_dir();
        assert!(result.is_ok(), "expected Ok for: Ok(runtime_dir)");
    }

    #[test]
    fn test_runtime_temp_file_ok_ensure_runtime_tmp_dir_join_unique_name_prefix_suffix() {
        let prefix = "";
        let suffix = "";
        let result = runtime_temp_file(&prefix, &suffix);
        assert!(result.is_ok(), "expected Ok for: Ok(ensure_runtime_tmp_dir()?.join(unique_name(prefix, suffix)))");
    }

    #[test]
    fn test_runtime_temp_dir_error_internal_io_e_to_string_some_format_create_temp_dir_pr() {
        let prefix = "";
        let _result = runtime_temp_dir(&prefix);
    }

    #[test]
    fn test_runtime_temp_dir_default_path() {
        let prefix = "";
        let _result = runtime_temp_dir(&prefix);
    }

    #[test]
    fn test_runtime_temp_dir_ok_path() {
        let prefix = "";
        let result = runtime_temp_dir(&prefix);
        assert!(result.is_ok(), "expected Ok for: Ok(path)");
    }

}
