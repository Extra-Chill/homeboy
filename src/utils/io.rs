//! File I/O primitives with consistent error handling.

use crate::error::{Error, Result};
use std::fs;
use std::path::Path;

/// Read file contents with standardized error handling.
///
/// Wraps `fs::read_to_string` with consistent `Error::internal_io` formatting.
pub fn read_file(path: &Path, operation: &str) -> Result<String> {
    fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(operation.to_string())))
}

/// Write content to file with standardized error handling.
///
/// Wraps `fs::write` with consistent `Error::internal_io` formatting.
pub fn write_file(path: &Path, content: &str, operation: &str) -> Result<()> {
    fs::write(path, content)
        .map_err(|e| Error::internal_io(e.to_string(), Some(operation.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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
        let result = write_file(Path::new("/nonexistent/dir/file.txt"), "content", "test write");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code.as_str(), "internal.io_error");
    }
}
