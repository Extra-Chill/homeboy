//! Build artifact path resolution with glob pattern support.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Resolve a potentially glob-patterned artifact path to an actual file.
///
/// - If path contains no glob chars (`*`, `?`, `[`, `]`), returns it unchanged after existence check
/// - If path is a glob, expands and returns most recently modified match
/// - Returns error if no files match or path doesn't exist
pub fn resolve_artifact_path(pattern: &str) -> Result<PathBuf> {
    resolve_artifact_path_from_root(pattern, None)
}

/// Resolve an artifact path relative to a component root when provided.
///
/// Relative paths/globs are interpreted from `root`, which avoids stale sibling
/// artifacts from other components when deploy runs outside the component cwd.
pub fn resolve_artifact_path_from_root(pattern: &str, root: Option<&Path>) -> Result<PathBuf> {
    let resolved_pattern = resolve_pattern(pattern, root);

    if !contains_glob_chars(pattern) {
        let path = resolved_pattern;
        if path.exists() {
            return Ok(path);
        }
        return Err(Error::validation_invalid_argument(
            "build_artifact",
            format!("Artifact not found: {}", pattern),
            Some(pattern.to_string()),
            None,
        ));
    }

    let entries: Vec<PathBuf> = glob::glob(&resolved_pattern.to_string_lossy())
        .map_err(|e| {
            Error::validation_invalid_argument(
                "build_artifact",
                format!(
                    "Invalid glob pattern '{}': {}",
                    resolved_pattern.display(),
                    e
                ),
                Some(pattern.to_string()),
                None,
            )
        })?
        .filter_map(|entry| entry.ok())
        .filter(|p| p.is_file())
        .collect();

    if entries.is_empty() {
        return Err(Error::validation_invalid_argument(
            "build_artifact",
            format!("No files match pattern: {}", pattern),
            Some(pattern.to_string()),
            None,
        ));
    }

    let newest = entries
        .into_iter()
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());

    match newest {
        Some(path) => {
            log_status!("deploy", "Resolved '{}' -> '{}'", pattern, path.display());
            Ok(path)
        }
        None => Err(Error::validation_invalid_argument(
            "build_artifact",
            format!("No files match pattern: {}", pattern),
            Some(pattern.to_string()),
            None,
        )),
    }
}

fn resolve_pattern(pattern: &str, root: Option<&Path>) -> PathBuf {
    let candidate = PathBuf::from(pattern);
    if candidate.is_absolute() {
        candidate
    } else if let Some(root) = root {
        root.join(candidate)
    } else {
        candidate
    }
}

fn contains_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains(']')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_literal_path_exists() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("artifact.zip");
        File::create(&file_path).unwrap();

        let result = resolve_artifact_path(file_path.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_literal_path_not_exists() {
        let result = resolve_artifact_path("/nonexistent/path/artifact.zip");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let details = err.details.to_string();
        assert!(
            details.contains("Artifact not found"),
            "Expected error details to contain 'Artifact not found', got: {}",
            details
        );
    }

    #[test]
    fn test_glob_pattern_single_match() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("build-1.0.0.zip");
        File::create(&file_path).unwrap();

        let pattern = dir.path().join("build-*.zip");
        let result = resolve_artifact_path(pattern.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_glob_pattern_multiple_matches_returns_newest() {
        let dir = TempDir::new().unwrap();

        let old_file = dir.path().join("build-1.0.0.zip");
        let mut f = File::create(&old_file).unwrap();
        f.write_all(b"old").unwrap();
        drop(f);

        thread::sleep(Duration::from_millis(50));

        let new_file = dir.path().join("build-1.0.1.zip");
        let mut f = File::create(&new_file).unwrap();
        f.write_all(b"new").unwrap();
        drop(f);

        let pattern = dir.path().join("build-*.zip");
        let result = resolve_artifact_path(pattern.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), new_file);
    }

    #[test]
    fn test_glob_pattern_no_matches() {
        let dir = TempDir::new().unwrap();
        let pattern = dir.path().join("nonexistent-*.zip");
        let result = resolve_artifact_path(pattern.to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err();
        let details = err.details.to_string();
        assert!(
            details.contains("No files match pattern"),
            "Expected error details to contain 'No files match pattern', got: {}",
            details
        );
    }

    #[test]
    fn test_glob_pattern_ignores_directories() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("build-1.0.0.zip");
        fs::create_dir(&subdir).unwrap();

        let pattern = dir.path().join("build-*.zip");
        let result = resolve_artifact_path(pattern.to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_contains_glob_chars() {
        assert!(contains_glob_chars("dist/*.zip"));
        assert!(contains_glob_chars("build-?.tar.gz"));
        assert!(contains_glob_chars("file[0-9].txt"));
        assert!(!contains_glob_chars("dist/artifact.zip"));
        assert!(!contains_glob_chars("/path/to/file.tar.gz"));
    }

    #[test]
    fn test_relative_literal_resolves_from_root() {
        let dir = TempDir::new().unwrap();
        let artifact_dir = dir.path().join("target").join("distrib");
        fs::create_dir_all(&artifact_dir).unwrap();
        let file_path = artifact_dir.join("artifact.zip");
        File::create(&file_path).unwrap();

        let result =
            resolve_artifact_path_from_root("target/distrib/artifact.zip", Some(dir.path()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_relative_glob_resolves_from_root() {
        let dir = TempDir::new().unwrap();
        let artifact_dir = dir.path().join("target").join("distrib");
        fs::create_dir_all(&artifact_dir).unwrap();
        let file_path = artifact_dir.join("build-1.0.1.zip");
        File::create(&file_path).unwrap();

        let result =
            resolve_artifact_path_from_root("target/distrib/build-*.zip", Some(dir.path()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path);
    }
}
