use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

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

/// Trait for file system operations - local or remote
pub trait FileSystem {
    fn read(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, content: &str) -> Result<()>;
    fn list(&self, dir: &Path) -> Result<Vec<Entry>>;
    fn delete(&self, path: &Path) -> Result<()>;
    fn ensure_dir(&self, dir: &Path) -> Result<()>;
}

/// Local filesystem implementation
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalFs {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystem for LocalFs {
    fn read(&self, path: &Path) -> Result<String> {
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

    fn write(&self, path: &Path, content: &str) -> Result<()> {
        // Atomic write: write to temp file, then rename
        let parent = path.parent().ok_or_else(|| {
            Error::internal_io(
                format!("Invalid path: {}", path.display()),
                Some("write file".to_string()),
            )
        })?;

        let filename = path.file_name().ok_or_else(|| {
            Error::internal_io(
                format!("Invalid path: {}", path.display()),
                Some("write file".to_string()),
            )
        })?;

        let tmp_path = parent.join(format!("{}.tmp", filename.to_string_lossy()));

        fs::write(&tmp_path, content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write temp file".to_string())))?;

        fs::rename(&tmp_path, path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("rename temp file".to_string())))?;

        Ok(())
    }

    fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
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

    fn delete(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Err(Error::internal_io(
                format!("File not found: {}", path.display()),
                Some("delete file".to_string()),
            ));
        }

        fs::remove_file(path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("delete file".to_string())))
    }

    fn ensure_dir(&self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| {
                Error::internal_io(e.to_string(), Some("create directory".to_string()))
            })?;
        }
        Ok(())
    }
}

/// Convenience function to get local filesystem
pub fn local() -> LocalFs {
    LocalFs::new()
}

/// Ensure all app directories exist
pub fn ensure_app_dirs() -> Result<()> {
    use crate::paths;

    let dirs = [
        paths::homeboy()?,
        paths::projects()?,
        paths::servers()?,
        paths::components()?,
        paths::modules()?,
        paths::keys()?,
        paths::backups()?,
    ];

    let fs = local();
    for dir in dirs {
        fs.ensure_dir(&dir)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
}
