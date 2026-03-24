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

fn ensure_runtime_tmp_dir() -> Result<PathBuf> {
    let runtime_dir = runtime_root()?;
    fs::create_dir_all(&runtime_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create homeboy runtime tmp directory".to_string()),
        )
    })?;
    Ok(runtime_dir)
}

/// Create a temporary directory under the runtime temp root.
///
/// Used by `RunDir::create()` for pipeline run directories and by
/// `deploy/release_download.rs` for ephemeral download artifacts.
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

        let path = runtime_temp_dir("homeboy-test-dir").expect("temp dir path");
        assert!(path.starts_with(dir.path()));
        assert!(path.is_dir());

        unsafe {
            env::remove_var(HOMEBOY_RUNTIME_TMPDIR_ENV);
        }
    }

    #[test]
    fn runtime_temp_dir_creates_dir() {
        let result = runtime_temp_dir("test-dir");
        assert!(result.is_ok());
        if let Ok(path) = result {
            assert!(path.is_dir());
        }
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
