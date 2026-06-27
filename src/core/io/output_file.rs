//! Atomic single-file output helpers shared across subsystems.
//!
//! These helpers write a file by staging contents in a sibling temp file and
//! renaming it into place, so readers never observe a partially written file.
//! The write semantics (parent-dir creation, trailing-newline handling) are
//! controlled via [`OutputWriteOptions`]. This is generic reusable I/O
//! infrastructure and lives in core so the command layer stays a thin adapter.

use homeboy::core::{Error, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

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
                std::fs::create_dir_all(parent)?;
            }
        }
    }

    let temp = atomic_output_temp_path(target);
    let mut file = std::fs::File::create(&temp)?;
    let contents = contents.as_ref();
    file.write_all(contents)?;
    if options.trailing_newline == TrailingNewline::Ensure && !contents.ends_with(b"\n") {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    drop(file);

    match std::fs::rename(&temp, target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err)
        }
    }
}

pub fn write_output_file(path: &str, contents: &str) -> Result<()> {
    write_output_file_atomically(path, contents, OutputWriteOptions::file())
        .map_err(|err| Error::internal_io(err.to_string(), Some(format!("write {path}"))))
}

fn atomic_output_temp_path(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    let temp_name = format!(".{file_name}.{}.tmp", std::process::id());
    target.with_file_name(temp_name)
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
}
