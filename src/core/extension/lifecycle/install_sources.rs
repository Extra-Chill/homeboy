//! Extension install-from-source resolution.
//!
//! Cohesive group extracted from the lifecycle root: cloning a extension from a
//! git URL, resolving single-extension vs monorepo clones, installing shared
//! assets, and linking a local source directory. Kept in a sibling module so
//! the lifecycle root stays under the structural line/item thresholds (#5241).

use std::path::Path;

use crate::core::agent_runtime_manifest::validate_installed_extension_agent_runtime_provider_discovery;
use crate::core::config::{self, from_str};
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::paths;

use super::super::execution::run_setup;
use super::super::load_extension;
use super::super::manifest::ExtensionManifest;
use super::{derive_id_from_url, manifest_path_for_extension, slugify_id, InstallResult};

pub(super) fn install_configured_extension(
    source: &str,
    extension_id: &str,
) -> Result<InstallResult> {
    if super::is_git_url(source) {
        return super::install(source, Some(extension_id));
    }

    let source_path = Path::new(source);
    let candidate = source_path
        .join(extension_id)
        .join(format!("{}.json", extension_id));

    if candidate.exists() {
        let extension_path = source_path.join(extension_id);
        return install_from_path(
            &extension_path.to_string_lossy(),
            Some(extension_id),
            Some(source_path),
        );
    }

    super::install(source, Some(extension_id))
}

/// Install a extension by cloning from a git repository URL.
///
/// Handles both single-extension repos (manifest at repo root) and monorepos
/// (manifest in a subdirectory matching the extension ID). For monorepos,
/// extracts just the target subdirectory.
pub(super) fn install_from_url(
    url: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<InstallResult> {
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

    git::clone_repo_at_ref(url, &temp_dir, revision)?;

    // Capture source revision before resolve_cloned_extension may discard .git
    // (monorepo installs extract only the subdirectory, losing git history).
    let source_revision = git::short_head_revision(&temp_dir);

    // Determine what was cloned and install accordingly.
    let result = resolve_cloned_extension(&temp_dir, &extension_id, &extension_dir, url);

    // Always clean up the temp clone dir (may already be renamed on success).
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    let extension_id = result?;

    // Write source metadata so it survives even when .git is discarded.
    if let Some(ref rev) = source_revision {
        let _ = std::fs::write(extension_dir.join(".source-revision"), rev);
    }
    let _ = std::fs::write(extension_dir.join(".source-url"), url);

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

    let manifest_path = paths::extension_manifest(&extension_id)?;
    if let Err(err) = validate_installed_extension_agent_runtime_provider_discovery(&extension_id) {
        let _ = std::fs::remove_dir_all(&extension_dir);
        return Err(err);
    }

    Ok(InstallResult {
        extension_id,
        url: url.to_string(),
        path: extension_dir,
        manifest_path,
        source_revision,
    })
}

/// After cloning a repo to a temp dir, figure out whether it's a single-extension
/// repo or a monorepo and move the right content to the final extension directory.
///
/// Returns the installed extension ID on success.
pub(crate) fn resolve_cloned_extension(
    temp_dir: &Path,
    extension_id: &str,
    extension_dir: &Path,
    _url: &str,
) -> Result<String> {
    let manifest_at_root = temp_dir.join(format!("{}.json", extension_id));

    // Case 1: Single-extension repo — manifest at clone root.
    if manifest_at_root.exists() {
        std::fs::rename(temp_dir, extension_dir).map_err(|e| {
            Error::internal_io(e.to_string(), Some("move cloned extension".to_string()))
        })?;
        return Ok(extension_id.to_string());
    }

    // Case 2: Monorepo — target extension exists as a subdirectory.
    let subdir = temp_dir.join(extension_id);
    let manifest_in_subdir = subdir.join(format!("{}.json", extension_id));

    if subdir.is_dir() && manifest_in_subdir.exists() {
        // Validate the manifest is parseable before moving.
        let content = local_files::local().read(&manifest_in_subdir)?;
        let _manifest: ExtensionManifest = from_str(&content)?;

        // Cloned installs copy: the clone temp dir is discarded after install,
        // so the shared trees must be materialized as standalone copies.
        install_shared_assets_from_root(temp_dir, extension_dir, SharedAssetMode::Copy)?;

        // Move just the subdirectory to the final extension location.
        rename_dir(&subdir, extension_dir)?;
        return Ok(extension_id.to_string());
    }

    // Case 3: No matching extension found. Scan for available extensions to help the user.
    let available = scan_available_extensions(temp_dir);

    if available.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "No extension manifest '{}.json' found in cloned repository",
                extension_id
            ),
            None,
            None,
        ));
    }

    let list = available.join(", ");
    Err(Error::validation_invalid_argument(
        "id",
        format!(
            "Extension '{}' not found in repository. Available extensions: {}",
            extension_id, list
        ),
        Some(extension_id.to_string()),
        None,
    )
    .with_hint(format!(
        "Install a specific extension with: homeboy extension install <url> --id <extension>\nAvailable: {}",
        list
    )))
}

/// How a shared monorepo asset tree is materialized into `~/.config/homeboy/`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SharedAssetMode {
    /// Copy the tree into place. Used for cloned installs, where the clone temp
    /// directory is discarded after install and there is no live source to point
    /// back to.
    Copy,
    /// Symlink the install target to its live source tree. Used for linked/dev
    /// installs (local-path source): edits to the shared monorepo assets
    /// (`agent-runtimes`, `runtime-agent-ci`, `scripts/lib`,
    /// `agent-task-contracts`) in the source worktree are visible to the live
    /// install immediately, with no reinstall or release. See #6396 / #1954.
    Symlink,
}

/// Shared assets installed from a monorepo extension source root.
///
/// Each entry maps a source-relative directory to its install target. The
/// `scripts/lib` narrowing (#4907) is deliberate: after the extension helper
/// migration (homeboy-extensions#1466), installed extension layouts only source
/// the shared library subtree via `../../../scripts/lib/...` for direct
/// invocation. The rest of the monorepo `scripts/` tree is repo dev/CI tooling
/// that never participates in the installed layout, so lifecycle no longer
/// installs it under `~/.config/homeboy/extensions/scripts`.
///
/// `mode` selects copy vs symlink materialization. Cloned installs copy;
/// linked/dev installs symlink so local edits to the shared trees go live
/// without a reinstall.
fn install_shared_assets_from_root(
    source_root: &Path,
    extension_dir: &Path,
    mode: SharedAssetMode,
) -> Result<()> {
    let Some(extensions_dir) = extension_dir.parent() else {
        return Ok(());
    };

    for shared_dir in [
        "scripts/lib",
        "agent-runtimes",
        "runtime-agent-ci",
        "agent-task-contracts",
    ] {
        let source = source_root.join(shared_dir);
        if !source.is_dir() {
            continue;
        }

        let target = match shared_dir {
            "agent-runtimes" => paths::agent_runtimes()?,
            "runtime-agent-ci" | "agent-task-contracts" => paths::homeboy()?.join(shared_dir),
            // Shared extension libraries install under the extensions root so
            // installed wrappers can source `../../../scripts/lib/...`.
            _ => extensions_dir.join(shared_dir),
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("prepare shared extension {shared_dir}")),
                )
            })?;
        }
        remove_existing_target(&target, shared_dir)?;

        match mode {
            SharedAssetMode::Copy => copy_dir_recursive(&source, &target)?,
            SharedAssetMode::Symlink => symlink_shared_asset(&source, &target, shared_dir)?,
        }
    }

    Ok(())
}

/// Remove a shared-asset install target if present, handling both real
/// directories (previous copy installs) and symlinks (previous linked installs)
/// so re-installs are idempotent regardless of the prior materialization mode.
fn remove_existing_target(target: &Path, shared_dir: &str) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(target) else {
        return Ok(());
    };
    let remove = if meta.file_type().is_symlink() || meta.is_file() {
        std::fs::remove_file(target)
    } else {
        std::fs::remove_dir_all(target)
    };
    remove.map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("replace shared extension {shared_dir}")),
        )
    })
}

/// Symlink a shared-asset install target to its live source tree.
///
/// The source is canonicalized to an absolute path so the link stays valid
/// regardless of the caller's working directory and resolves to the live source
/// worktree (making in-place edits visible to the install).
fn symlink_shared_asset(source: &Path, target: &Path, shared_dir: &str) -> Result<()> {
    let resolved = std::fs::canonicalize(source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("resolve shared extension source {shared_dir}")),
        )
    })?;

    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&resolved, target);

    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(&resolved, target);

    result.map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("symlink shared extension {shared_dir}")),
        )
    })
}

/// Materialize shared monorepo assets for a linked (local-path) install.
///
/// Linked installs symlink the shared trees to their live source so that edits
/// to `agent-runtimes`, `runtime-agent-ci`, `scripts/lib`, and
/// `agent-task-contracts` in the source worktree take effect immediately,
/// enabling rapid local iteration without a reinstall or release.
pub(crate) fn install_linked_shared_assets(
    source: &Path,
    extension_dir: &Path,
    source_root: Option<&Path>,
) -> Result<()> {
    if let Some(source_root) = source_root {
        return install_shared_assets_from_root(source_root, extension_dir, SharedAssetMode::Symlink);
    }

    if let Some(parent) = source.parent() {
        install_shared_assets_from_root(parent, extension_dir, SharedAssetMode::Symlink)?;
    }
    Ok(())
}

/// Scan a cloned repo for subdirectories that contain a matching manifest file.
/// Returns a sorted list of extension IDs found.
fn scan_available_extensions(repo_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(repo_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                    // Skip hidden dirs (.git, .github, etc.)
                    if dir_name.starts_with('.') {
                        continue;
                    }
                    let manifest = path.join(format!("{}.json", dir_name));
                    if manifest.exists() {
                        found.push(dir_name.to_string());
                    }
                }
            }
        }
    }
    found.sort();
    found
}

/// Move a directory, falling back to recursive copy + delete if rename fails
/// (e.g., across filesystem boundaries).
pub(crate) fn rename_dir(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }

    // Fallback: recursive copy then remove source.
    copy_dir_recursive(from, to)?;
    std::fs::remove_dir_all(from)
        .map_err(|e| Error::internal_io(e.to_string(), Some("remove source after copy".into())))?;
    Ok(())
}

/// Recursively copy a directory tree.
///
/// Thin wrapper over [`crate::core::io::copy_tree`] with the legacy
/// extension-lifecycle entry policy (copy any non-directory entry).
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    crate::core::io::copy_tree(
        src,
        dst,
        "extension.lifecycle.copy_dir_recursive",
        crate::core::io::EntryPolicy::CopyAnyNonDir,
    )
}

/// Install a extension by symlinking a local directory.
pub(super) fn install_from_path(
    source_path: &str,
    id_override: Option<&str>,
    source_root: Option<&Path>,
) -> Result<InstallResult> {
    let source = Path::new(source_path);

    // Resolve to absolute path
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::internal_io(e.to_string(), Some("get current dir".to_string())))?
            .join(source)
    };

    if !source.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Path does not exist: {}", source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    // Derive extension ID from directory name or override
    let dir_name = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            "Could not determine directory name",
            Some(source_path.to_string()),
            None,
        )
    })?;

    let extension_id = match id_override {
        Some(id) => slugify_id(id)?,
        None => slugify_id(dir_name)?,
    };

    // Check cross-entity name collision before checking extension-specific existence
    config::check_id_collision(&extension_id, "extension")?;

    let manifest_path = manifest_path_for_extension(&source, &extension_id);
    if !manifest_path.exists() {
        if id_override.is_some() {
            let monorepo_extension = source.join(&extension_id);
            let monorepo_manifest = manifest_path_for_extension(&monorepo_extension, &extension_id);
            if monorepo_manifest.exists() {
                return install_from_path(
                    &monorepo_extension.to_string_lossy(),
                    Some(&extension_id),
                    Some(&source),
                );
            }
        }

        return Err(Error::validation_invalid_argument(
            "source",
            format!("No {}.json found at {}", extension_id, source.display()),
            Some(source_path.to_string()),
            None,
        ));
    }

    // Validate manifest is parseable
    let manifest_content = local_files::local().read(&manifest_path)?;
    let _manifest: ExtensionManifest = from_str(&manifest_content)?;

    let extension_dir = paths::extension(&extension_id)?;
    if extension_dir.exists() {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!(
                "Extension '{}' already exists at {}",
                extension_id,
                extension_dir.display()
            ),
            Some(extension_id),
            None,
        ));
    }

    local_files::ensure_app_dirs()?;

    install_linked_shared_assets(&source, &extension_dir, source_root)?;

    // Create symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, &extension_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source, &extension_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    // For linked (local) extensions, read revision from the source dir if it's a git repo
    let source_revision = git::short_head_revision(&source);
    let manifest_path = paths::extension_manifest(&extension_id)?;
    if let Err(err) = validate_installed_extension_agent_runtime_provider_discovery(&extension_id) {
        let _ = std::fs::remove_file(&extension_dir);
        return Err(err);
    }

    Ok(InstallResult {
        extension_id,
        url: source.to_string_lossy().to_string(),
        path: extension_dir,
        manifest_path,
        source_revision,
    })
}
