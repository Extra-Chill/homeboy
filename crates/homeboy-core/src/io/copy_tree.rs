//! Recursive directory copy shared between extension lifecycle and invocation
//! artifact preservation.
//!
//! Two callers have historically maintained near-identical recursive copy
//! loops with their own error-context labels and slightly different entry
//! filtering rules. This module unifies the loop shape; callers pass an
//! [`EntryPolicy`] to preserve their original semantics.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// How [`copy_tree`] handles non-directory entries it encounters while walking
/// the source tree, and how it formats IO error messages.
#[derive(Clone, Copy, Debug)]
pub(crate) enum EntryPolicy {
    /// Copy every entry that is not a directory using a cheap `Path::is_dir()`
    /// check. Mirrors the historical `extension::lifecycle::copy_dir_recursive`
    /// behavior; symlinks fall through to `fs::copy` (which follows them).
    /// Error messages contain only the underlying IO error.
    CopyAnyNonDir,
    /// Copy only entries whose `DirEntry` metadata reports `is_file()`,
    /// silently skipping symlinks and other non-regular files. Mirrors the
    /// historical `engine::invocation::copy_directory` behavior used for
    /// preserving invocation artifacts. Error messages embed the offending
    /// source/target paths for debuggability.
    CopyRegularFilesOnly,
}

/// Recursively copy `src` into `dst`, creating `dst` (and intermediate
/// directories) if necessary.
///
/// `error_context` is attached to every IO failure so callers retain their
/// own label (e.g. `"invocation.artifacts.preserve"`,
/// `"extension.lifecycle.copy_dir_recursive"`). The previous
/// `extension::lifecycle` implementation used per-step labels (`"create target
/// dir"`, `"read dir entry"`, etc.); those were debug-only breadcrumbs not
/// consumed by any caller, so the unified helper folds them into a single
/// caller label for simplicity.
pub(crate) fn copy_tree(
    src: &Path,
    dst: &Path,
    error_context: &str,
    policy: EntryPolicy,
) -> Result<()> {
    let ctx = || error_context.to_string();

    fs::create_dir_all(dst).map_err(|e| {
        let msg = match policy {
            EntryPolicy::CopyAnyNonDir => e.to_string(),
            EntryPolicy::CopyRegularFilesOnly => {
                format!("Failed to create directory {}: {e}", dst.display())
            }
        };
        Error::internal_io(msg, Some(ctx()))
    })?;

    let read_dir = fs::read_dir(src).map_err(|e| {
        let msg = match policy {
            EntryPolicy::CopyAnyNonDir => e.to_string(),
            EntryPolicy::CopyRegularFilesOnly => {
                format!("Failed to read directory {}: {e}", src.display())
            }
        };
        Error::internal_io(msg, Some(ctx()))
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|e| {
            let msg = match policy {
                EntryPolicy::CopyAnyNonDir => e.to_string(),
                EntryPolicy::CopyRegularFilesOnly => {
                    format!("Failed to read directory entry in {}: {e}", src.display())
                }
            };
            Error::internal_io(msg, Some(ctx()))
        })?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        match policy {
            EntryPolicy::CopyAnyNonDir => {
                if src_path.is_dir() {
                    copy_tree(&src_path, &dst_path, error_context, policy)?;
                } else {
                    fs::copy(&src_path, &dst_path)
                        .map_err(|e| Error::internal_io(e.to_string(), Some(ctx())))?;
                }
            }
            EntryPolicy::CopyRegularFilesOnly => {
                let metadata = entry.metadata().map_err(|e| {
                    Error::internal_io(
                        format!("Failed to stat {}: {e}", src_path.display()),
                        Some(ctx()),
                    )
                })?;
                if metadata.is_dir() {
                    copy_tree(&src_path, &dst_path, error_context, policy)?;
                } else if metadata.is_file() {
                    fs::copy(&src_path, &dst_path).map_err(|e| {
                        Error::internal_io(
                            format!(
                                "Failed to copy {} to {}: {e}",
                                src_path.display(),
                                dst_path.display()
                            ),
                            Some(ctx()),
                        )
                    })?;
                }
                // symlinks and other non-regular entries are intentionally skipped.
            }
        }
    }

    Ok(())
}
