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
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_unresolvable_paths_report_why() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("build-1.0.0.zip")).unwrap();
        for (pattern, expected) in [
            (
                "/nonexistent/path/artifact.zip".to_string(),
                "Artifact not found",
            ),
            (
                dir.path().join("nonexistent-*.zip").display().to_string(),
                "No files match pattern",
            ),
            // Directories matching the glob are not artifacts.
            (
                dir.path().join("build-*.zip").display().to_string(),
                "No files match pattern",
            ),
        ] {
            let err = resolve_artifact_path(&pattern).expect_err(&pattern);
            let details = err.details.to_string();
            assert!(details.contains(expected), "{pattern}: {details}");
        }
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
        assert_eq!(result.unwrap(), new_file);
    }

    #[test]
    fn test_relative_literal_and_glob_resolve_from_root() {
        let dir = TempDir::new().unwrap();
        let artifact_dir = dir.path().join("target").join("distrib");
        fs::create_dir_all(&artifact_dir).unwrap();
        let file_path = artifact_dir.join("build-1.0.1.zip");
        File::create(&file_path).unwrap();

        for pattern in [
            "target/distrib/build-1.0.1.zip",
            "target/distrib/build-*.zip",
        ] {
            let result = resolve_artifact_path_from_root(pattern, Some(dir.path()));
            assert_eq!(result.unwrap(), file_path, "{pattern}");
        }
    }
}
