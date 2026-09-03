//! Atomic single-file output helpers shared across subsystems.
//!
//! These helpers write a file by staging contents in a sibling temp file and
//! renaming it into place, so readers never observe a partially written file.
//! The write semantics (parent-dir creation, trailing-newline handling) are
//! controlled via [`OutputWriteOptions`]. This is generic reusable I/O
//! infrastructure and lives in core so the command layer stays a thin adapter.

use crate::{Error, Result};
use std::fs::OpenOptions;
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
                crate::engine::local_files::create_dir_all_durably(parent)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
        }
    }

    write_output_file_atomically_with(
        target,
        contents.as_ref(),
        options,
        |path| OpenOptions::new().write(true).create_new(true).open(path),
        |file, bytes| file.write_all(bytes),
        std::fs::File::sync_all,
        |from, to| std::fs::rename(from, to),
        |parent| OpenOptions::new().read(true).open(parent)?.sync_all(),
    )
}

fn write_output_file_atomically_with<C, W, S, R, D>(
    target: &Path,
    contents: &[u8],
    options: OutputWriteOptions,
    create: C,
    mut write: W,
    sync: S,
    rename: R,
    sync_parent: D,
) -> std::io::Result<()>
where
    C: FnOnce(&Path) -> std::io::Result<std::fs::File>,
    W: FnMut(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
    S: FnOnce(&std::fs::File) -> std::io::Result<()>,
    R: FnOnce(&Path, &Path) -> std::io::Result<()>,
    D: FnOnce(&Path) -> std::io::Result<()>,
{
    let temp = atomic_output_temp_path(target);
    let cleanup = || {
        let _ = std::fs::remove_file(&temp);
    };
    let mut file = create(&temp)?;
    if let Err(error) = write(&mut file, contents) {
        drop(file);
        cleanup();
        return Err(error);
    }
    if options.trailing_newline == TrailingNewline::Ensure && !contents.ends_with(b"\n") {
        if let Err(error) = write(&mut file, b"\n") {
            drop(file);
            cleanup();
            return Err(error);
        }
    }
    if let Err(error) = sync(&file) {
        drop(file);
        cleanup();
        return Err(error);
    }
    drop(file);

    match rename(&temp, target) {
        Ok(()) => {
            let parent = target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            sync_parent(parent)
        }
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
    let temp_name = format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4());
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

    #[test]
    fn atomic_writer_fsyncs_dot_for_a_bare_relative_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Path::new("output.json");
        write_output_file_atomically_with(
            path,
            b"published",
            OutputWriteOptions::file(),
            |relative| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dir.path().join(relative))
            },
            |file, bytes| file.write_all(bytes),
            |file| file.sync_all(),
            |from, to| std::fs::rename(dir.path().join(from), dir.path().join(to)),
            |parent| {
                assert_eq!(parent, Path::new("."));
                std::fs::File::open(dir.path())?.sync_all()
            },
        )
        .expect("a published bare relative output must not fail its parent fsync");

        assert_eq!(
            std::fs::read_to_string(dir.path().join(path)).unwrap(),
            "published"
        );
    }

    fn assert_injected_staging_failure_removes_temp(fail_write: bool, fail_sync: bool) {
        let dir = tempfile::tempdir().expect("temp dir");
        let output_path = dir.path().join("output.json");
        let error = write_output_file_atomically_with(
            &output_path,
            b"{}",
            OutputWriteOptions::artifact(),
            |path| OpenOptions::new().write(true).create_new(true).open(path),
            move |file, bytes| {
                if fail_write {
                    Err(std::io::Error::other("injected write failure"))
                } else {
                    file.write_all(bytes)
                }
            },
            move |file| {
                if fail_sync {
                    Err(std::io::Error::other("injected sync failure"))
                } else {
                    file.sync_all()
                }
            },
            |from, to| std::fs::rename(from, to),
            |parent| std::fs::File::open(parent)?.sync_all(),
        )
        .expect_err("injected failure");

        assert!(error.to_string().contains("injected"));
        assert!(!output_path.exists());
        assert!(std::fs::read_dir(dir.path())
            .expect("read dir")
            .next()
            .is_none());
    }

    #[test]
    fn atomic_writer_removes_temp_after_write_failure() {
        assert_injected_staging_failure_removes_temp(true, false);
    }

    #[test]
    fn atomic_writer_removes_temp_after_sync_failure() {
        assert_injected_staging_failure_removes_temp(false, true);
    }

    #[test]
    fn atomic_writer_surfaces_parent_directory_sync_failure_after_rename() {
        let dir = tempfile::tempdir().expect("temp dir");
        let output_path = dir.path().join("output.json");
        let error = write_output_file_atomically_with(
            &output_path,
            b"{}",
            OutputWriteOptions::artifact(),
            |path| std::fs::File::create(path),
            |file, bytes| file.write_all(bytes),
            |file| file.sync_all(),
            |from, to| std::fs::rename(from, to),
            |_| Err(std::io::Error::other("injected directory sync failure")),
        )
        .expect_err("directory sync failure must not report durable success");

        assert!(error.to_string().contains("directory sync failure"));
        assert_eq!(std::fs::read_to_string(output_path).unwrap(), "{}");
    }
}
