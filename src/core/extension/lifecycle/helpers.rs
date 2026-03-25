//! helpers — extracted from lifecycle.rs.

use crate::config::{self, from_str};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::git;
use crate::paths;
use std::path::{Path, PathBuf};
use std::process::Command;
use super::super::execution::run_setup;
use super::super::{is_extension_linked, load_extension};
use super::super::manifest::ExtensionManifest;
use super::slugify_id;
use super::InstallResult;
use super::get_short_head_revision;
use super::UpdateAvailable;
use super::resolve_cloned_extension;
use super::derive_id_from_url;


/// Install a extension by cloning from a git repository URL.
///
/// Handles both single-extension repos (manifest at repo root) and monorepos
/// (manifest in a subdirectory matching the extension ID). For monorepos,
/// extracts just the target subdirectory.
pub(crate) fn install_from_url(url: &str, id_override: Option<&str>) -> Result<InstallResult> {
    let extension_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => derive_id_from_url(url)?,
    };

    // Check cross-entity name collision before checking extension-specific existence
    config::check_id_collision(&extension_id, "extension")?;

    let extension_dir = paths::extension(&extension_id)?;
    if extension_dir.exists() {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!("Extension {} already exists", extension_id),
            Some(extension_id),
            None,
        ));
    }

    local_files::ensure_app_dirs()?;

    // Clone to a temp directory first so we can detect monorepos before
    // committing to the final extension location.
    let extensions_dir = paths::extensions()?;
    let temp_dir = extensions_dir.join(format!(".clone-tmp-{}", extension_id));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| {
            Error::internal_io(e.to_string(), Some("clean stale temp dir".to_string()))
        })?;
    }

    git::clone_repo(url, &temp_dir)?;

    // Capture source revision before resolve_cloned_extension may discard .git
    // (monorepo installs extract only the subdirectory, losing git history).
    let source_revision = get_short_head_revision(&temp_dir);

    // Determine what was cloned and install accordingly.
    let result = resolve_cloned_extension(&temp_dir, &extension_id, &extension_dir, url);

    // Always clean up the temp clone dir (may already be renamed on success).
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    let extension_id = result?;

    // Write source revision so it survives even when .git is discarded.
    if let Some(ref rev) = source_revision {
        let _ = std::fs::write(extension_dir.join(".source-revision"), rev);
    }

    // Auto-run setup if extension defines a setup_command
    // Setup is best-effort: install succeeds even if setup fails
    if let Ok(extension) = load_extension(&extension_id) {
        if extension
            .runtime()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = run_setup(&extension_id);
        }
    }

    Ok(InstallResult {
        extension_id,
        url: url.to_string(),
        path: extension_dir,
        source_revision,
    })
}

/// Uninstall a extension. Automatically detects symlinks vs cloned directories.
/// - Symlinked extensions: removes symlink only (source preserved)
/// - Cloned extensions: removes directory entirely
pub fn uninstall(extension_id: &str) -> Result<PathBuf> {
    let extension_dir = paths::extension(extension_id)?;
    if !extension_dir.exists() {
        return Err(Error::extension_not_found(extension_id.to_string(), vec![]));
    }

    if extension_dir.is_symlink() {
        // Symlinked extension: just remove the symlink, source directory is preserved
        std::fs::remove_file(&extension_dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("remove symlink".to_string())))?;
    } else {
        // Cloned extension: remove the directory
        std::fs::remove_dir_all(&extension_dir).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("remove extension directory".to_string()),
            )
        })?;
    }

    Ok(extension_dir)
}

/// Check if a git-cloned extension has updates available.
/// Runs `git fetch` then checks if HEAD is behind the remote tracking branch.
/// Returns None for linked extensions or if check fails.
pub fn check_update_available(extension_id: &str) -> Option<UpdateAvailable> {
    let extension_dir = paths::extension(extension_id).ok()?;
    if !extension_dir.exists() || is_extension_linked(extension_id) {
        return None;
    }

    // Check it's a git repo
    if !extension_dir.join(".git").exists() {
        return None;
    }

    // Fetch latest (best-effort, short timeout)
    Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(&extension_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;

    // Check how many commits we're behind
    let output = Command::new("git")
        .args(["rev-list", "HEAD..@{u}", "--count"])
        .current_dir(&extension_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;

    let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let behind_count: usize = count_str.parse().ok()?;

    if behind_count == 0 {
        return None;
    }

    // Get installed version
    let extension = load_extension(extension_id).ok()?;
    let installed_version = extension.version.clone();

    Some(UpdateAvailable {
        extension_id: extension_id.to_string(),
        installed_version,
        behind_count,
    })
}
