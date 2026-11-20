//! Atomic single-file output helpers shared across subsystems.
//!
//! These helpers write a file by staging contents in a sibling temp file and
//! renaming it into place, so readers never observe a partially written file.
//! The write semantics (parent-dir creation, trailing-newline handling) are
//! controlled via [`OutputWriteOptions`]. This is generic reusable I/O
//! infrastructure and lives in core so the command layer stays a thin adapter.

use crate::{Error, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingNewline {
    Preserve,
    Ensure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputWriteOptions {
    pub create_parent_dirs: bool,
    pub trailing_newline: TrailingNewline,
}

impl OutputWriteOptions {
    pub const fn file() -> Self {
        Self {
            create_parent_dirs: false,
            trailing_newline: TrailingNewline::Preserve,
        }
    }

    pub const fn artifact() -> Self {
        Self {
            create_parent_dirs: true,
            trailing_newline: TrailingNewline::Preserve,
        }
    }

    pub const fn json_output() -> Self {
        Self {
            create_parent_dirs: false,
            trailing_newline: TrailingNewline::Ensure,
        }
    }
}

pub fn write_output_file_atomically(
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
    options: OutputWriteOptions,
) -> std::io::Result<()> {
    let target = path.as_ref();
    if options.create_parent_dirs {
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                crate::engine::local_files::create_dir_all_durably(parent)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
        }
    }

    let contents = contents.as_ref();
    let mut terminated;
    let contents = if options.trailing_newline == TrailingNewline::Ensure
        && !contents.ends_with(b"\n")
    {
        terminated = contents.to_vec();
        terminated.push(b'\n');
        terminated.as_slice()
    } else {
        contents
    };
    crate::engine::local_files::write_file_atomic(target, contents, "write output file")
        .map_err(|error| std::io::Error::other(error.to_string()))
}

pub fn write_output_file(path: &str, contents: &str) -> Result<()> {
    write_output_file_atomically(path, contents, OutputWriteOptions::file())
        .map_err(|err| Error::internal_io(err.to_string(), Some(format!("write {path}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_writer_replaces_existing_file_and_removes_temp() {
        let dir = tempfile::tempdir().expect("temp dir");
        let output_path = dir.path().join("output.txt");
        std::fs::write(&output_path, "old").expect("seed output");

        write_output_file_atomically(&output_path, "new", OutputWriteOptions::file())
            .expect("write output");

        assert_eq!(std::fs::read_to_string(&output_path).unwrap(), "new");
        assert!(std::fs::read_dir(dir.path())
            .expect("read dir")
            .all(|entry| !entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn atomic_writer_can_create_parent_dirs_and_ensure_newline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let output_path = dir.path().join("nested").join("output.json");

        write_output_file_atomically(
            &output_path,
            "{}",
            OutputWriteOptions {
                create_parent_dirs: true,
                trailing_newline: TrailingNewline::Ensure,
            },
        )
        .expect("write output");

        assert_eq!(std::fs::read_to_string(&output_path).unwrap(), "{}\n");
    }

    #[test]
    fn output_preserves_binary_contents_and_existing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output");
        for contents in [b"\xff\0".as_slice(), b"already terminated\n", b""] {
            write_output_file_atomically(&path, contents, OutputWriteOptions::file()).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), contents);

            write_output_file_atomically(&path, contents, OutputWriteOptions::json_output())
                .unwrap();
            let mut expected = contents.to_vec();
            if !expected.ends_with(b"\n") {
                expected.push(b'\n');
            }
            assert_eq!(std::fs::read(&path).unwrap(), expected);
        }
    }

    #[test]
    fn output_does_not_create_parents_unless_requested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/output");
        assert!(write_output_file_atomically(&path, "data", OutputWriteOptions::file()).is_err());
        assert!(!path.parent().unwrap().exists());
    }
}
