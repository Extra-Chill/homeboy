//! Extension update-check + source-URL utilities (core glue over an extension's
//! git checkout). Relocated from the extension lifecycle module - depends only on
//! core paths/git/error + the core extension store, no extension behavior.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Result;
use crate::extension_store::{is_extension_linked, load_extension};
use crate::git;
use crate::paths;

/// Check if a string looks like a git URL (vs a local path).
pub fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
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
#[derive(Debug, Clone)]
pub struct UpdateAvailable {
    pub extension_id: String,
    pub installed_version: String,
    pub behind_count: usize,
}

pub fn read_source_revision(extension_id: &str) -> Option<String> {
    let extension_dir = paths::extension(extension_id).ok()?;
    if !extension_dir.exists() {
        return None;
    }

    // Try .git first (single-extension repos and linked extensions)
    if let Some(rev) = git::head_sha(&extension_dir) {
        return Some(rev);
    }

    // Fall back to source metadata files (monorepo installs and staged linked installs).
    read_source_metadata_value(&extension_dir, "revision")
}

pub fn read_source_metadata_value(extension_dir: &Path, kind: &str) -> Option<String> {
    let sidecar =
        source_metadata_dir(extension_dir).join(source_metadata_file(extension_dir, kind));
    let embedded = extension_dir.join(format!(".source-{kind}"));
    let paths = if extension_dir.is_symlink() {
        [sidecar, embedded]
    } else {
        [embedded, sidecar]
    };

    for path in paths {
        if let Some(value) = std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(value);
        }
    }

    None
}

pub fn read_source_url(extension_dir: &Path) -> Option<String> {
    read_source_metadata_value(extension_dir, "url")
}

pub fn source_metadata_dir(extension_dir: &Path) -> PathBuf {
    if extension_dir.is_symlink() {
        return extension_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
    }

    extension_dir.to_path_buf()
}

fn source_metadata_file(extension_dir: &std::path::Path, kind: &str) -> String {
    if extension_dir.is_symlink() {
        if let Some(name) = extension_dir.file_name().and_then(|name| name.to_str()) {
            return format!(".{name}.source-{kind}");
        }
    }

    format!(".source-{kind}")
}
