use crate::error::{Error, Result};
use crate::paths;
use std::env;
use std::fs;
use std::path::PathBuf;

const HOMEBOY_RUNTIME_TMPDIR_ENV: &str = "HOMEBOY_RUNTIME_TMPDIR";

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
    Ok(runtime_dir)
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
}
