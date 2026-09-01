use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use homeboy_error::{Error, Result};

/// Entry returned from directory listing
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
}

impl Entry {
    pub fn is_json(&self) -> bool {
        self.path.extension().is_some_and(|ext| ext == "json")
    }
}

/// Local filesystem implementation
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }

    pub fn read(&self, path: &Path) -> Result<String> {
        fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::internal_io(
                    format!("File not found: {}", path.display()),
                    Some("read file".to_string()),
                )
            } else {
                Error::internal_io(e.to_string(), Some("read file".to_string()))
            }
        })
    }

    pub fn write(&self, path: &Path, content: &str) -> Result<()> {
        write_file_atomic(path, content, "write file")
    }

    pub fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("list directory".to_string())))?;

        let mut result = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = path.is_dir();
            result.push(Entry { path, is_dir });
        }

        Ok(result)
    }

    pub fn delete(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(Error::internal_io(
                format!("File not found: {}", path.display()),
                Some("delete file".to_string()),
            ));
        }

        fs::remove_file(path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("delete file".to_string())))
    }

    pub fn ensure_dir(&self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| {
                Error::internal_io(e.to_string(), Some("create directory".to_string()))
            })?;
        }
        Ok(())
    }
}

impl Default for LocalFs {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function to get local filesystem
pub fn local() -> LocalFs {
    LocalFs::new()
}

/// Ensure all app directories exist
///
/// The config root is resolved exactly once and threaded into every derived
/// location. This used to be seven independent resolutions of the same root —
/// `paths::homeboy()` plus six derived resolvers that each re-resolved it
/// internally — on the startup path. A repoint of the home root between the
/// first and the last built half a config tree in one home and half in another
/// (#7505).
pub fn ensure_app_dirs() -> Result<()> {
    ensure_app_dirs_in_root(&homeboy_paths::homeboy()?)
}

/// Ensure all app directories exist below an already-resolved config root.
///
/// Every segment name is owned by `homeboy-paths`; none is inlined here, so the
/// two cannot drift.
pub fn ensure_app_dirs_in_root(config_root: &Path) -> Result<()> {
    use homeboy_paths as paths;

    let dirs = [
        config_root.to_path_buf(),
        paths::projects_in_root(config_root),
        paths::servers_in_root(config_root),
        paths::components_in_root(config_root),
        paths::extensions_in_root(config_root),
        paths::keys_in_root(config_root),
        paths::backups_in_root(config_root),
    ];

    let fs = local();
    for dir in dirs {
        fs.ensure_dir(&dir)?;
    }

    Ok(())
}

/// Read file contents with standardized error handling.
pub fn read_file(path: &Path, operation: &str) -> Result<String> {
    fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(operation.to_string())))
}

/// Write content to file with standardized error handling.
pub fn write_file(path: &Path, content: &str, operation: &str) -> Result<()> {
    fs::write(path, content)
        .map_err(|e| Error::internal_io(e.to_string(), Some(operation.to_string())))
}

/// Write content to file atomically (write to .tmp, then rename).
pub fn write_file_atomic(path: &Path, content: &str, operation: &str) -> Result<()> {
    write_file_atomic_with_owner_only(path, content, operation, false)
}

fn write_file_atomic_with_owner_only(
    path: &Path,
    content: &str,
    operation: &str,
    owner_only: bool,
) -> Result<()> {
    write_file_atomic_with_owner_only_writer(
        path,
        content,
        operation,
        owner_only,
        write_file_owner_only,
    )
}

fn write_file_atomic_with_owner_only_writer(
    path: &Path,
    content: &str,
    operation: &str,
    owner_only: bool,
    owner_only_writer: fn(&Path, &str, &str) -> Result<()>,
) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_io(
            format!("Invalid path: {}", path.display()),
            Some(operation.to_string()),
        )
    })?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };

    let filename = path.file_name().ok_or_else(|| {
        Error::internal_io(
            format!("Invalid path: {}", path.display()),
            Some(operation.to_string()),
        )
    })?;

    let tmp_path = unique_temp_path(parent, filename.to_string_lossy().as_ref());

    if owner_only {
        if let Err(error) =
            owner_only_writer(&tmp_path, content, &format!("{} (write temp)", operation))
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(error);
        }
    } else {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("{} (create temp)", operation)),
                )
            })?;
        use std::io::Write;
        if let Err(error) = file.write_all(content.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("{} (write temp)", operation)),
            ));
        }
    }
    if let Err(error) = fs::File::open(&tmp_path).and_then(|file| file.sync_all()) {
        let _ = fs::remove_file(&tmp_path);
        return Err(Error::internal_io(
            error.to_string(),
            Some(format!("{} (sync temp)", operation)),
        ));
    }
    if let Err(error) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(Error::internal_io(
            error.to_string(),
            Some(format!("{} (rename)", operation)),
        ));
    }
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("{} (sync parent)", operation)),
            )
        })
}

/// Create parent directories and atomically persist pretty-printed JSON.
pub fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_file_with_owner_only(path, value, false)
}

/// Create parent directories and atomically persist owner-only JSON on Unix.
pub fn write_json_file_owner_only<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_file_with_owner_only(path, value, true)
}

/// Remove a durable marker and sync its directory so a crash cannot resurrect it.
pub fn remove_file_durably(path: &Path, operation: &str) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(operation.to_string()),
            ));
        }
    }
    let parent = path.parent().ok_or_else(|| {
        Error::internal_io(
            format!("Invalid path: {}", path.display()),
            Some(operation.to_string()),
        )
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| Error::internal_io(error.to_string(), Some(operation.to_string())))
}

fn write_json_file_with_owner_only<T: Serialize>(
    path: &Path,
    value: &T,
    owner_only: bool,
) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_unexpected(format!("path has no parent: {}", path.display()))
    })?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    create_dir_all_durably(parent)?;
    let json = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    write_file_atomic_with_owner_only(
        path,
        &format!("{json}\n"),
        &path.display().to_string(),
        owner_only,
    )
}

pub fn create_dir_all_durably(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = path;
    while !current.exists() {
        missing.push(current.to_path_buf());
        current = current.parent().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "directory has no existing ancestor: {}",
                path.display()
            ))
        })?;
    }
    fs::create_dir_all(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    for directory in missing.iter().rev() {
        let parent = directory.parent().expect("missing directory has parent");
        fs::File::open(parent)
            .and_then(|parent| parent.sync_all())
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
    }
    Ok(())
}

/// Create a file owner-only on Unix before writing its contents.
pub fn write_file_owner_only(path: &Path, content: &str, operation: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| Error::internal_io(error.to_string(), Some(operation.to_string())))?;
        file.write_all(content.as_bytes())
            .map_err(|error| Error::internal_io(error.to_string(), Some(operation.to_string())))
    }

    #[cfg(not(unix))]
    {
        write_file(path, content, operation)
    }
}

fn unique_temp_path(parent: &Path, filename: &str) -> PathBuf {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    parent.join(format!(
        ".{}.{}.{}.{}.tmp",
        filename,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;
    use tempfile::NamedTempFile;

    fn partially_write_owner_only_then_fail(
        path: &Path,
        content: &str,
        operation: &str,
    ) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| Error::internal_io(error.to_string(), Some(operation.to_string())))?;
        file.write_all(&content.as_bytes()[..1])
            .map_err(|error| Error::internal_io(error.to_string(), Some(operation.to_string())))?;
        Err(Error::internal_io(
            "injected owner-only partial-write failure",
            Some(operation.to_string()),
        ))
    }

    #[test]
    fn test_local_fs_write_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let fs = local();

        fs.write(&path, "hello world").unwrap();
        let content = fs.read(&path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn local_fs_write_uses_unique_temp_paths_for_concurrent_writers() {
        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("config.json"));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    local()
                        .write(&path, &format!(r#"{{"writer":{index}}}"#))
                        .expect("concurrent write succeeds");
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("writer thread");
        }

        let content = fs::read_to_string(path.as_ref()).expect("read final config");
        serde_json::from_str::<serde_json::Value>(&content).expect("valid json");
        let temp_files: Vec<_> = fs::read_dir(dir.path())
            .expect("list tempdir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            temp_files.is_empty(),
            "temp files left behind: {temp_files:?}"
        );
    }

    #[test]
    fn owner_only_atomic_write_cleans_temp_after_partial_write_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "original").unwrap();

        let error = write_file_atomic_with_owner_only_writer(
            &path,
            "replacement",
            "replace owner-only config",
            true,
            partially_write_owner_only_then_fail,
        )
        .expect_err("injected partial write must fail");

        assert_eq!(error.code.as_str(), "internal.io_error");
        assert_eq!(
            error.details["error"],
            "injected owner-only partial-write failure"
        );
        assert_eq!(
            error.details["context"],
            "replace owner-only config (write temp)"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let temp_files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            temp_files.is_empty(),
            "temp files left behind: {temp_files:?}"
        );
    }

    #[test]
    fn test_local_fs_list() {
        let dir = tempdir().unwrap();
        let fs = local();

        fs.write(&dir.path().join("a.json"), "{}").unwrap();
        fs.write(&dir.path().join("b.txt"), "text").unwrap();

        let entries = fs.list(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);

        let json_entries: Vec<_> = entries.iter().filter(|e| e.is_json()).collect();
        assert_eq!(json_entries.len(), 1);
    }

    #[test]
    fn test_local_fs_delete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("delete_me.txt");
        let fs = local();

        fs.write(&path, "content").unwrap();
        assert!(path.exists());

        fs.delete(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn read_file_succeeds_for_existing_file() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(temp, "test content").unwrap();

        let content = read_file(temp.path(), "test read").unwrap();
        assert!(content.contains("test content"));
    }

    #[test]
    fn read_file_returns_error_for_missing_file() {
        let result = read_file(Path::new("/nonexistent/path.txt"), "test read");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code.as_str(), "internal.io_error");
    }

    #[test]
    fn write_file_succeeds_for_valid_path() {
        let temp = NamedTempFile::new().unwrap();
        let result = write_file(temp.path(), "new content", "test write");
        assert!(result.is_ok());

        let content = fs::read_to_string(temp.path()).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn write_file_returns_error_for_invalid_path() {
        let result = write_file(
            Path::new("/nonexistent/dir/file.txt"),
            "content",
            "test write",
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code.as_str(), "internal.io_error");
    }

    #[test]
    fn write_json_file_creates_parents_and_preserves_pretty_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/state.json");

        write_json_file(&path, &serde_json::json!({ "name": "homeboy" })).unwrap();

        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "{\n  \"name\": \"homeboy\"\n}\n"
        );
    }

    #[test]
    fn atomic_writer_accepts_a_relative_filename() {
        let filename = format!(
            ".homeboy-local-files-relative-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let path = Path::new(&filename);

        write_file_atomic(path, "relative content", "relative atomic write").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "relative content");
        fs::remove_file(path).unwrap();
    }
}
