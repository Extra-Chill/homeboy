//! Extension update lifecycle.
//!
//! Cohesive group extracted from the lifecycle root: pulling latest changes for
//! cloned and linked extensions, reconciling source metadata, and reporting
//! available updates. Kept in a sibling module so the lifecycle root stays under
//! the structural line/item thresholds (#5241).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::paths;

use super::super::{is_extension_linked, load_extension, ExtensionSourceUpdate};
use super::install_sources::{install_linked_shared_assets, rename_dir, resolve_cloned_extension};
use super::{source_metadata, UpdateResult};

/// Update an installed extension by pulling latest changes.
pub fn update(extension_id: &str, force: bool) -> Result<UpdateResult> {
    let extension_dir = paths::extension(extension_id)?;
    if !extension_dir.exists() {
        return Err(Error::extension_not_found(extension_id.to_string(), vec![]));
    }

    // Linked extensions: resolve the symlink target and pull the source repo.
    // The target may be a subdirectory of a larger repo (e.g. an extensions
    // monorepo/<extension-id>), so we find the git root and pull from there.
    if is_extension_linked(extension_id) {
        return update_linked_extension(extension_id, &extension_dir, force);
    }

    if !force && !is_extension_update_workdir_clean(&extension_dir, &extension_dir) {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            "Extension has uncommitted changes; update may overwrite them. Use --force to proceed.",
            Some(extension_id.to_string()),
            None,
        ));
    }

    let source = source_metadata::resolve_source_url(extension_id)?;
    let source_url = source.url;
    let mut source_repair = source.repair;

    if extension_dir.join(".git").exists() {
        git::pull_repo(&extension_dir)?;

        // Update source metadata after pull so it stays current.
        write_source_metadata(
            &extension_dir,
            &source_url,
            git::short_head_revision(&extension_dir),
        );

        run_setup_if_configured(extension_id);

        return Ok(UpdateResult {
            extension_id: extension_id.to_string(),
            url: source_url,
            path: extension_dir,
            linked: false,
            source_path: None,
            git_root: None,
            source_update: ExtensionSourceUpdate::default(),
            repaired_source_metadata: source_repair.take(),
        });
    }

    update_extracted_extension(extension_id, &extension_dir, &source_url)?;

    run_setup_if_configured(extension_id);

    Ok(UpdateResult {
        extension_id: extension_id.to_string(),
        url: source_url,
        path: extension_dir,
        linked: false,
        source_path: None,
        git_root: None,
        source_update: ExtensionSourceUpdate::default(),
        repaired_source_metadata: source_repair,
    })
}

fn update_extracted_extension(
    extension_id: &str,
    extension_dir: &Path,
    source_url: &str,
) -> Result<()> {
    let extensions_dir = paths::extensions()?;
    let clone_dir = extensions_dir.join(format!(".update-clone-tmp-{}", extension_id));
    let staged_dir = extensions_dir.join(format!(".update-stage-tmp-{}", extension_id));
    let backup_dir = extensions_dir.join(format!(".update-backup-tmp-{}", extension_id));

    for stale in [&clone_dir, &staged_dir, &backup_dir] {
        if stale.exists() {
            std::fs::remove_dir_all(stale).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some("clean stale extension update dir".to_string()),
                )
            })?;
        }
    }

    git::clone_repo(source_url, &clone_dir)?;
    let source_revision = git::short_head_revision(&clone_dir);

    let result = resolve_cloned_extension(&clone_dir, extension_id, &staged_dir, source_url);
    if clone_dir.exists() {
        let _ = std::fs::remove_dir_all(&clone_dir);
    }
    result?;

    write_source_metadata(&staged_dir, source_url, source_revision);

    rename_dir(extension_dir, &backup_dir)?;
    if let Err(err) = rename_dir(&staged_dir, extension_dir) {
        let _ = rename_dir(&backup_dir, extension_dir);
        return Err(err);
    }

    if backup_dir.exists() {
        let _ = std::fs::remove_dir_all(&backup_dir);
    }

    Ok(())
}

pub(crate) fn write_source_metadata(
    extension_dir: &Path,
    source_url: &str,
    source_revision: Option<String>,
) {
    if let Some(rev) = source_revision {
        let _ = std::fs::write(extension_dir.join(".source-revision"), rev);
    }
    let _ = std::fs::write(extension_dir.join(".source-url"), source_url);
}

pub(crate) fn is_extension_update_workdir_clean(git_root: &Path, extension_dir: &Path) -> bool {
    if !git::is_git_repo(&git_root.to_string_lossy()) {
        return true;
    }

    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(git_root)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let extension_rel = extension_dir
        .strip_prefix(git_root)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let status = String::from_utf8_lossy(&output.stdout);

    status.lines().all(|line| {
        dirty_path_from_status_line(line).is_some_and(|path| {
            is_generated_extension_metadata_path(&path, extension_rel.as_deref())
        })
    })
}

fn dirty_path_from_status_line(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').replace('\\', "/"))
}

fn is_generated_extension_metadata_path(path: &str, extension_rel: Option<&str>) -> bool {
    [".source-url", ".source-revision"].iter().any(|name| {
        path == *name
            || extension_rel
                .filter(|rel| !rel.is_empty())
                .is_some_and(|rel| path == format!("{rel}/{name}"))
    })
}

pub(crate) fn run_setup_if_configured(extension_id: &str) {
    if let Ok(extension) = load_extension(extension_id) {
        if extension
            .runtime()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = super::super::execution::run_setup(extension_id);
        }
    }
}

fn update_linked_extension(
    extension_id: &str,
    extension_dir: &Path,
    force: bool,
) -> Result<UpdateResult> {
    let source_dir = std::fs::read_link(extension_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read symlink for {}", extension_id)),
        )
    })?;
    let source_dir = if source_dir.is_absolute() {
        source_dir
    } else {
        extension_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(source_dir)
    };
    let source_dir = source_dir.canonicalize().unwrap_or(source_dir);
    let git_root_str = git::get_git_root(&source_dir.to_string_lossy())?;
    let git_root = PathBuf::from(&git_root_str)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(git_root_str));
    let old_branch = git::current_branch(&git_root);
    let old_source_revision = git::short_head_revision(&git_root);

    static UPDATED_ROOTS: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let updated_roots = UPDATED_ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = updated_roots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&git_root)
        .cloned();
    match cached {
        Some(None) => {}
        Some(Some(message)) => return Err(Error::validation_invalid_argument(
            "extension_id",
            format!("Linked extension '{}' skipped because shared source repo update previously failed: {}", extension_id, message),
            Some(extension_id.to_string()),
            None,
        )),
        None => {
            let result = (|| {
                if !force && !is_extension_update_workdir_clean(&git_root, &source_dir) {
                    return Err(Error::validation_invalid_argument(
                        "extension_id",
                        format!(
                            "Linked extension source repo has uncommitted changes for {}. Use --force to proceed.",
                            extension_id,
                        ),
                        Some(extension_id.to_string()),
                        None,
                    ));
                }

                git::update_to_remote_default_branch(&git_root)?;

                Ok(())
            })();
            updated_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    git_root.clone(),
                    result.as_ref().err().map(|e| e.message.clone()),
                );
            result?;
        }
    };
    install_linked_shared_assets(&source_dir, extension_dir, None)?;
    run_setup_if_configured(extension_id);
    let url = format!("linked:{}", source_dir.display());
    let new_branch = git::current_branch(&git_root);
    let new_source_revision = git::short_head_revision(&git_root);
    Ok(UpdateResult {
        extension_id: extension_id.to_string(),
        url,
        path: source_dir.clone(),
        linked: true,
        source_path: Some(source_dir.clone()),
        git_root: Some(git_root),
        source_update: ExtensionSourceUpdate {
            old_source_revision,
            new_source_revision,
            old_branch,
            new_branch,
            update_note: Some(
                "Linked extension source updated in place; clean linked repos switch to the remote default branch before pulling.".to_string(),
            ),
        },
        repaired_source_metadata: None,
    })
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

/// Read the source revision for an installed extension.
/// Checks (in order): .git directory (git rev-parse), then .source-revision file.
pub fn read_source_revision(extension_id: &str) -> Option<String> {
    let extension_dir = paths::extension(extension_id).ok()?;
    if !extension_dir.exists() {
        return None;
    }

    // Try .git first (single-extension repos and linked extensions)
    if let Some(rev) = git::short_head_revision(&extension_dir) {
        return Some(rev);
    }

    // Fall back to .source-revision file (monorepo installs)
    let rev_file = extension_dir.join(".source-revision");
    std::fs::read_to_string(&rev_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
