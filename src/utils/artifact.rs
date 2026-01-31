//! Artifact path resolution with glob pattern support.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// Resolve a potentially glob-patterned artifact path to an actual file.
///
/// - If path contains no glob chars (`*`, `?`, `[`, `]`), returns it unchanged after existence check
/// - If path is a glob, expands and returns most recently modified match
/// - Returns error if no files match or path doesn't exist
pub fn resolve_artifact_path(pattern: &str) -> Result<PathBuf> {
    if !contains_glob_chars(pattern) {
        let path = PathBuf::from(pattern);
        if path.exists() {
            return Ok(path);
        }
        return Err(Error::other(format!("Artifact not found: {}", pattern)));
    }

    let entries: Vec<PathBuf> = glob::glob(pattern)
        .map_err(|e| Error::other(format!("Invalid glob pattern '{}': {}", pattern, e)))?
        .filter_map(|entry| entry.ok())
        .filter(|p| p.is_file())
        .collect();

    if entries.is_empty() {
        return Err(Error::other(format!(
            "No files match pattern: {}",
            pattern
        )));
    }

    let newest = entries
        .into_iter()
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());

    match newest {
        Some(path) => {
            eprintln!(
                "[deploy] Resolved '{}' -> '{}'",
                pattern,
                path.display()
            );
            Ok(path)
        }
        None => Err(Error::other(format!(
            "No files match pattern: {}",
            pattern
        ))),
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
        assert!(result.unwrap_err().to_string().contains("Artifact not found"));
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
        assert!(result.unwrap_err().to_string().contains("No files match pattern"));
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
}
