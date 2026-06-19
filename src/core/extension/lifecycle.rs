use crate::core::config::{self, from_str};
use crate::core::engine::identifier;
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::paths;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use super::execution::run_setup;
use super::manifest::ExtensionManifest;
use super::{is_extension_linked, load_extension, ExtensionSourceUpdate};

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallForComponentResult {
    pub component_id: String,
    pub source: String,
    pub installed: Vec<InstallResult>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub linked: bool,
    pub source_path: Option<PathBuf>,
    pub git_root: Option<PathBuf>,
    pub source_update: ExtensionSourceUpdate,
    pub repaired_source_metadata: Option<source_metadata::SourceMetadataRepair>,
}

pub mod source_metadata;

pub fn slugify_id(value: &str) -> Result<String> {
    identifier::slugify_id(value, "extension_id")
}

/// Derive a extension ID from a git URL.
pub fn derive_id_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let segment = trimmed
        .split('/')
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");

    slugify_id(segment)
}

/// Check if a string looks like a git URL (vs a local path).
pub fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Returns the path to a extension's manifest file: {extension_dir}/{id}.json
fn manifest_path_for_extension(extension_dir: &Path, id: &str) -> PathBuf {
    extension_dir.join(format!("{}.json", id))
}

/// Install a extension from a git URL or link a local directory.
/// Automatically detects whether source is a URL (git clone) or local path (symlink).
pub fn install(source: &str, id_override: Option<&str>) -> Result<InstallResult> {
    install_with_revision(source, id_override, None)
}

/// Install a extension from a git URL or link a local directory.
/// Git URL installs optionally check out a branch, tag, or commit after cloning.
pub fn install_with_revision(
    source: &str,
    id_override: Option<&str>,
    revision: Option<&str>,
) -> Result<InstallResult> {
    if is_git_url(source) {
        install_from_url(source, id_override, revision)
    } else {
        install_from_path(source, id_override, None)
    }
}

/// Install every extension declared by a component from the same source.
///
/// Already-installed extensions are skipped so CI setup can be re-run safely.
pub fn install_for_component(
    component: &crate::core::component::Component,
    source: &str,
) -> Result<InstallForComponentResult> {
    let extensions = component.extensions.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            Some(component.id.clone()),
            None,
        )
    })?;

    if extensions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            Some(component.id.clone()),
            None,
        ));
    }

    let mut extension_ids: Vec<String> = extensions.keys().cloned().collect();
    extension_ids.sort();

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for extension_id in extension_ids {
        if load_extension(&extension_id).is_ok() {
            skipped.push(extension_id);
            continue;
        }

        installed.push(install_configured_extension(source, &extension_id)?);
    }

    Ok(InstallForComponentResult {
        component_id: component.id.clone(),
        source: source.to_string(),
        installed,
        skipped,
    })
}

fn install_configured_extension(source: &str, extension_id: &str) -> Result<InstallResult> {
    if is_git_url(source) {
        return install(source, Some(extension_id));
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

    install(source, Some(extension_id))
}

/// Install a extension by cloning from a git repository URL.
///
/// Handles both single-extension repos (manifest at repo root) and monorepos
/// (manifest in a subdirectory matching the extension ID). For monorepos,
/// extracts just the target subdirectory.
fn install_from_url(
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

    Ok(InstallResult {
        extension_id,
        url: url.to_string(),
        path: extension_dir,
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

        install_shared_assets_from_root(temp_dir, extension_dir)?;

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

fn install_shared_assets_from_root(source_root: &Path, extension_dir: &Path) -> Result<()> {
    let Some(extensions_dir) = extension_dir.parent() else {
        return Ok(());
    };

    for shared_dir in ["scripts", "agent-runtimes", "runtime-agent-ci"] {
        let source = source_root.join(shared_dir);
        if !source.is_dir() {
            continue;
        }

        let target = match shared_dir {
            "agent-runtimes" => paths::agent_runtimes()?,
            "runtime-agent-ci" => paths::homeboy()?.join(shared_dir),
            _ => extensions_dir.join(shared_dir),
        };
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("replace shared extension {shared_dir}")),
                )
            })?;
        }
        copy_dir_recursive(&source, &target)?;
    }

    Ok(())
}

pub(crate) fn install_linked_shared_assets(
    source: &Path,
    extension_dir: &Path,
    source_root: Option<&Path>,
) -> Result<()> {
    if let Some(source_root) = source_root {
        return install_shared_assets_from_root(source_root, extension_dir);
    }

    if let Some(parent) = source.parent() {
        install_shared_assets_from_root(parent, extension_dir)?;
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
fn install_from_path(
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

    Ok(InstallResult {
        extension_id,
        url: source.to_string_lossy().to_string(),
        path: extension_dir,
        source_revision,
    })
}

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

fn is_extension_update_workdir_clean(git_root: &Path, extension_dir: &Path) -> bool {
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
            let _ = run_setup(extension_id);
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

#[cfg(test)]
mod tests {
    use super::{
        install, install_for_component, install_with_revision, is_extension_update_workdir_clean,
        load_extension, read_source_revision, source_metadata, update,
    };
    use crate::core::component;
    use crate::core::extension::update_all;
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn write_extension_fixture(root: &Path, id: &str) {
        write_extension_fixture_with_version(root, id, "1.0.0");
    }

    fn write_extension_fixture_with_version(root: &Path, id: &str, version: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "{}"
}}"#,
                id, version
            ),
        )
        .expect("extension manifest");
    }

    fn write_extension_fixture_with_setup(root: &Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{"name":"{} extension","version":"1.0.0","executable":{{"runtime":{{"setup_command":"printf setup >> setup-count.txt"}}}}}}"#,
                id
            ),
        )
        .expect("extension manifest");
    }

    fn write_component_fixture(root: &Path, extensions: &[&str]) {
        let extension_json = extensions
            .iter()
            .map(|id| format!(r#"    "{}": {{}}"#, id))
            .collect::<Vec<_>>()
            .join(",\n");

        fs::write(
            root.join("homeboy.json"),
            format!(
                r#"{{
  "id": "multi-extension-component",
  "extensions": {{
{}
  }}
}}"#,
                extension_json
            ),
        )
        .expect("component config");
    }

    fn write_shared_runtime_fixture(root: &Path) {
        let runtime_script = root.join(
            "agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs",
        );
        fs::create_dir_all(runtime_script.parent().expect("runtime script parent"))
            .expect("runtime script dir");
        fs::write(&runtime_script, "console.log('sample runtime');\n").expect("runtime script");

        let runtime_agent_ci_helper =
            root.join("runtime-agent-ci/lib/agent-task-provider-contract.js");
        fs::create_dir_all(
            runtime_agent_ci_helper
                .parent()
                .expect("runtime agent ci helper parent"),
        )
        .expect("runtime agent ci helper dir");
        fs::write(
            &runtime_agent_ci_helper,
            "module.exports = { schema: 'fixture' };\n",
        )
        .expect("runtime agent ci helper");
    }

    fn run_git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn commit_all(dir: &Path, message: &str) -> bool {
        run_git(dir, &["add", "."])
            && run_git(
                dir,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    message,
                ],
            )
    }

    fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn prepare_git_extension_repo(repo: &Path, extension_id: &str) -> Option<TempDir> {
        write_extension_fixture(repo, extension_id);
        prepare_git_repo(repo)
    }

    fn prepare_git_extension_monorepo(repo: &Path, extension_ids: &[&str]) -> Option<TempDir> {
        for extension_id in extension_ids {
            write_extension_fixture_with_setup(repo, extension_id);
        }
        prepare_git_repo(repo)
    }

    fn prepare_git_repo(repo: &Path) -> Option<TempDir> {
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return None;
        }

        let remote_parent = TempDir::new().expect("remote parent");
        let remote_path = remote_parent.path().join("extension.git");
        let remote_path_str = remote_path.to_string_lossy().to_string();
        if !run_git(
            repo,
            &["clone", "--bare", repo.to_str().unwrap(), &remote_path_str],
        ) {
            return None;
        }
        if !run_git(repo, &["remote", "add", "origin", &remote_path_str]) {
            return None;
        }
        if !run_git(repo, &["fetch", "origin", "--quiet"]) {
            return None;
        }
        let branch = if run_git(repo, &["rev-parse", "--verify", "main"]) {
            "main"
        } else {
            "master"
        };
        if !run_git(
            repo,
            &[
                "branch",
                "--set-upstream-to",
                &format!("origin/{branch}"),
                branch,
            ],
        ) {
            return None;
        }

        Some(remote_parent)
    }

    #[cfg(unix)]
    fn write_git_wrapper(bin_dir: &Path, pull_count_file: &Path) {
        fs::create_dir_all(bin_dir).expect("wrapper bin dir");
        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git");
        let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "pull" ]; then
  printf x >> '{}'
fi
exec '{}' "$@"
"#,
            pull_count_file.display(),
            real_git
        );
        let wrapper = bin_dir.join("git");
        fs::write(&wrapper, script).expect("git wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("wrapper perms");
    }

    #[test]
    fn test_install_for_component_installs_multiple_extensions() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);
            let component = component::discover_from_portable(&component_dir).expect("component");

            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            let installed_ids = result
                .installed
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(installed_ids, vec!["alpha", "beta"]);
            assert!(result.skipped.is_empty());
            assert!(home
                .join(".config/homeboy/extensions/alpha/alpha.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/beta/beta.json")
                .exists());
        });
    }

    #[test]
    fn test_install_for_component_skips_already_installed_extensions() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);
            let component = component::discover_from_portable(&component_dir).expect("component");

            install(&source.join("alpha").to_string_lossy(), Some("alpha"))
                .expect("pre-install alpha");

            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            let installed_ids = result
                .installed
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(installed_ids, vec!["beta"]);
            assert_eq!(result.skipped, vec!["alpha"]);
        });
    }

    #[test]
    fn test_install_for_component_uses_path_based_portable_component_config() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);

            let component = component::discover_from_portable(&component_dir)
                .expect("component should resolve from portable path");
            let result = install_for_component(&component, &source.to_string_lossy())
                .expect("install should succeed");

            assert_eq!(result.component_id, "multi-extension-component");
            assert_eq!(result.installed.len(), 2);
        });
    }

    #[test]
    fn install_without_replace_remains_non_destructive() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "swift");

            install(&source.join("swift").to_string_lossy(), Some("swift"))
                .expect("initial install");

            let err = install(&source.join("swift").to_string_lossy(), Some("swift"))
                .expect_err("second install should still fail");

            assert!(err.to_string().contains("already exists"));
        });
    }

    #[test]
    fn linked_update_does_not_write_source_revision_to_source_checkout() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };

            let extension_source = source.join("wordpress");
            install(&extension_source.to_string_lossy(), Some("wordpress"))
                .expect("install linked extension");

            let before = read_source_revision("wordpress").expect("linked git revision");
            assert!(!extension_source.join(".source-revision").exists());

            update("wordpress", false).expect("update linked extension");

            assert!(
                !extension_source.join(".source-revision").exists(),
                "linked update must not write metadata into the source checkout"
            );
            assert_eq!(
                read_source_revision("wordpress"),
                Some(before),
                "linked extensions should resolve revisions through git discovery"
            );
        });
    }

    #[test]
    fn extension_update_allows_generated_source_metadata_dirty_in_linked_subdir() {
        let temp = TempDir::new().expect("create tempdir");
        let repo = temp.path();
        let extension_dir = repo.join("wordpress");
        write_extension_fixture(repo, "wordpress");
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return;
        }

        fs::write(
            extension_dir.join(".source-url"),
            "https://example.com/ext.git",
        )
        .expect("source url metadata");
        fs::write(extension_dir.join(".source-revision"), "abc123")
            .expect("source revision metadata");

        assert!(
            is_extension_update_workdir_clean(repo, &extension_dir),
            "generated extension metadata should not block update"
        );
    }

    #[test]
    fn extension_update_blocks_user_dirty_file_next_to_generated_metadata() {
        let temp = TempDir::new().expect("create tempdir");
        let repo = temp.path();
        let extension_dir = repo.join("wordpress");
        write_extension_fixture(repo, "wordpress");
        if !run_git(repo, &["init", "--quiet"]) || !commit_all(repo, "init") {
            return;
        }

        fs::write(
            extension_dir.join(".source-url"),
            "https://example.com/ext.git",
        )
        .expect("source url metadata");
        fs::write(extension_dir.join("notes.txt"), "user work").expect("user-authored dirty file");

        assert!(
            !is_extension_update_workdir_clean(repo, &extension_dir),
            "user-authored dirt should still block update"
        );
    }

    #[test]
    fn cloned_monorepo_install_preserves_source_revision_marker() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");

            assert!(result.path.join(".source-revision").exists());
            assert_eq!(
                fs::read_to_string(result.path.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                remote_url.to_string_lossy()
            );
            assert_eq!(
                read_source_revision("wordpress"),
                result.source_revision,
                "monorepo installs keep the stored source revision after .git is discarded"
            );
        });
    }

    #[test]
    fn cloned_monorepo_install_materializes_shared_scripts() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "rust");
            let shared_helper = source.join("scripts/lib/test-result-adapters.sh");
            fs::create_dir_all(shared_helper.parent().expect("helper parent"))
                .expect("shared scripts dir");
            fs::write(
                &shared_helper,
                "homeboy_parse_test_results_with_adapters() { :; }\n",
            )
            .expect("shared helper");
            let remote = match prepare_git_repo(&source) {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("rust")).expect("install cloned extension");

            assert!(home
                .join(".config/homeboy/extensions/rust/rust.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/scripts/lib/test-result-adapters.sh")
                .exists());
        });
    }

    #[test]
    fn cloned_monorepo_install_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);
            let remote = match prepare_git_repo(&source) {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/runtime-agent-ci/lib/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn extracted_monorepo_update_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install cloned extension");
            assert!(!home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());

            write_shared_runtime_fixture(&source);
            assert!(commit_all(&source, "add runtime package"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            update("wordpress", false).expect("update cloned extension");

            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
            assert!(home
                .join(".config/homeboy/runtime-agent-ci/lib/agent-task-provider-contract.js")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_install_materializes_shared_scripts() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            let shared_helper = source.join("scripts/lib/test-result-adapters.sh");
            fs::create_dir_all(shared_helper.parent().expect("helper parent"))
                .expect("shared scripts dir");
            fs::write(
                &shared_helper,
                "homeboy_parse_test_results_with_adapters() { :; }\n",
            )
            .expect("shared helper");

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/extensions/scripts/lib/test-result-adapters.sh")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_install_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
        });
    }

    #[test]
    fn linked_monorepo_root_install_with_id_materializes_shared_agent_runtimes() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            write_extension_fixture(&source, "wordpress");
            write_shared_runtime_fixture(&source);

            install(&source.to_string_lossy(), Some("wordpress"))
                .expect("install linked extension from monorepo root");

            assert!(home
                .join(".config/homeboy/extensions/wordpress/wordpress.json")
                .exists());
            assert!(home
                .join(".config/homeboy/agent-runtimes/sample-runtime/scripts/agent/sample-runtime-agent-task-executor.cjs")
                .exists());
        });
    }

    #[test]
    fn test_install_with_revision() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let pinned_revision = match git_output(&source, &["rev-parse", "--short", "HEAD"]) {
                Some(revision) => revision,
                None => return,
            };

            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));
            let remote_url = remote.path().join("extension.git");

            let result = install_with_revision(
                &remote_url.to_string_lossy(),
                Some("wordpress"),
                Some(&pinned_revision),
            )
            .expect("install pinned revision");

            let installed = load_extension("wordpress").expect("installed extension");
            assert_eq!(installed.version, "1.0.0");
            assert_eq!(
                result.source_revision.as_deref(),
                Some(pinned_revision.as_str())
            );
            assert_eq!(
                read_source_revision("wordpress").as_deref(),
                Some(pinned_revision.as_str())
            );
        });
    }

    #[test]
    fn cloned_install_can_checkout_requested_branch() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            assert!(run_git(&source, &["checkout", "-b", "next-extension"]));
            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "branch extension update"));
            assert!(run_git(&source, &["push", "origin", "next-extension"]));
            let branch_revision = match git_output(&source, &["rev-parse", "--short", "HEAD"]) {
                Some(revision) => revision,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install_with_revision(
                &remote_url.to_string_lossy(),
                Some("wordpress"),
                Some("next-extension"),
            )
            .expect("install branch revision");

            let installed = load_extension("wordpress").expect("installed extension");
            assert_eq!(installed.version, "2.0.0");
            assert_eq!(
                result.source_revision.as_deref(),
                Some(branch_revision.as_str())
            );
        });
    }

    #[test]
    fn update_all_updates_linked_extensions_through_single_update_path() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            let result = update_all(false);

            assert_eq!(result.updated.len(), 1);
            assert_eq!(result.updated[0].extension_id, "wordpress");
            assert!(
                result.skipped.is_empty(),
                "linked extensions should not be pre-skipped by update_all"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_run_setup_if_configured() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_monorepo(&source, &["fixture-a", "fixture-b"])
            {
                Some(remote) => remote,
                None => return,
            };

            install(
                &source.join("fixture-a").to_string_lossy(),
                Some("fixture-a"),
            )
            .expect("install linked fixture-a extension");
            install(
                &source.join("fixture-b").to_string_lossy(),
                Some("fixture-b"),
            )
            .expect("install linked fixture-b extension");

            let bin_dir = home.join("bin");
            let pull_count_file = home.join("pull-count");
            write_git_wrapper(&bin_dir, &pull_count_file);
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), old_path));

            let result = update_all(false);

            std::env::set_var("PATH", old_path);

            let updated_ids = result
                .updated
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(updated_ids, vec!["fixture-a", "fixture-b"]);
            assert!(result.skipped.is_empty());
            assert_eq!(
                fs::read_to_string(&pull_count_file)
                    .unwrap_or_default()
                    .len(),
                1,
                "linked extensions sharing one git root should run one git pull"
            );
            assert_eq!(
                fs::read_to_string(source.join("fixture-a/setup-count.txt"))
                    .unwrap_or_default()
                    .matches("setup")
                    .count(),
                1,
                "fixture-a setup should still run after the shared root update"
            );
            assert_eq!(
                fs::read_to_string(source.join("fixture-b/setup-count.txt"))
                    .unwrap_or_default()
                    .matches("setup")
                    .count(),
                1,
                "fixture-b setup should still run after the shared root update"
            );
        });
    }

    #[test]
    fn linked_update_switches_clean_worktree_to_default_branch_or_detached_default() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let _remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let default_branch = if run_git(&source, &["rev-parse", "--verify", "main"]) {
                "main"
            } else {
                "master"
            };

            assert!(run_git(
                &source,
                &["checkout", "-b", "feature-linked-extension"]
            ));
            assert!(run_git(
                &source,
                &[
                    "push",
                    "--set-upstream",
                    "origin",
                    "feature-linked-extension"
                ]
            ));
            let stable_checkout = home.join("stable-checkout");
            assert!(run_git(
                &source,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    stable_checkout.to_str().expect("stable checkout path"),
                    default_branch,
                ]
            ));

            install(
                &source.join("wordpress").to_string_lossy(),
                Some("wordpress"),
            )
            .expect("install linked extension");

            let result =
                update("wordpress", false).expect("linked update should use default branch");
            assert!(result.linked);
            assert_eq!(
                result.source_update.old_branch.as_deref(),
                Some("feature-linked-extension")
            );

            let branch_output = Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&source)
                .output()
                .expect("current branch");
            let current_branch = String::from_utf8_lossy(&branch_output.stdout)
                .trim()
                .to_string();
            assert_eq!(
                current_branch,
                result.source_update.new_branch.unwrap_or_default(),
                "linked update metadata should report the resulting branch"
            );
            assert!(
                current_branch == default_branch || current_branch.is_empty(),
                "linked update should use the default branch, or detached origin/default when the branch is checked out in another worktree"
            );
        });
    }

    #[test]
    fn extracted_monorepo_update_reclones_from_stored_source_url() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install extracted extension");
            assert!(!result.path.join(".git").exists());

            write_extension_fixture_with_version(&source, "wordpress", "2.0.0");
            assert!(commit_all(&source, "update extension"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            update("wordpress", false).expect("update extracted extension");

            let updated = load_extension("wordpress").expect("updated extension");
            assert_eq!(updated.version, "2.0.0");
            assert_eq!(
                fs::read_to_string(result.path.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                remote_url.to_string_lossy()
            );
        });
    }

    #[test]
    fn extracted_monorepo_update_keeps_existing_install_when_validation_fails() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source-repo");
            fs::create_dir_all(&source).expect("source repo");
            let remote = match prepare_git_extension_repo(&source, "wordpress") {
                Some(remote) => remote,
                None => return,
            };
            let remote_url = remote.path().join("extension.git");

            let result = install(&remote_url.to_string_lossy(), Some("wordpress"))
                .expect("install extracted extension");

            fs::write(source.join("wordpress/wordpress.json"), "not json")
                .expect("write invalid manifest");
            assert!(commit_all(&source, "break extension manifest"));
            assert!(run_git(&source, &["push", "origin", "HEAD"]));

            assert!(update("wordpress", false).is_err());

            let current = load_extension("wordpress").expect("previous extension remains loadable");
            assert_eq!(current.version, "1.0.0");
            assert!(
                result.path.join("wordpress.json").exists(),
                "failed update must leave the prior install in place"
            );
        });
    }

    #[test]
    fn copied_extension_manifest_source_metadata_repairs_source_url() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let extension_dir = extensions_dir.join("rust");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("rust.json"),
                r#"{
  "name": "rust extension",
  "version": "1.0.0",
  "source_url": "https://github.com/Extra-Chill/homeboy-extensions"
}"#,
            )
            .expect("extension manifest");

            let source =
                source_metadata::resolve_source_url("rust").expect("manifest source repair");

            assert_eq!(
                source.url,
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
            let repair = source.repair.expect("repair result");
            assert_eq!(
                repair.source_url,
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
            assert!(repair.reason.contains("manifest sourceUrl"));
            assert_eq!(
                fs::read_to_string(extension_dir.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                "https://github.com/Extra-Chill/homeboy-extensions"
            );
        });
    }

    #[test]
    fn manifest_source_url_alias_repairs_missing_source_url_marker() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/custom");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("custom.json"),
                r#"{
  "name": "custom extension",
  "version": "1.0.0",
  "sourceUrl": "https://example.com/custom.git"
}"#,
            )
            .expect("extension manifest");

            let source =
                source_metadata::resolve_source_url("custom").expect("manifest source repair");

            assert_eq!(source.url, "https://example.com/custom.git");
            let repair = source.repair.expect("repair result");
            assert!(repair.reason.contains("manifest sourceUrl"));
            assert_eq!(
                fs::read_to_string(extension_dir.join(".source-url"))
                    .expect("source url marker")
                    .trim(),
                "https://example.com/custom.git"
            );
        });
    }

    #[test]
    fn copied_unknown_extension_missing_source_metadata_is_actionable_error() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            write_extension_fixture(&extensions_dir, "custom");
            let extension_dir = extensions_dir.join("custom");

            let err = source_metadata::resolve_source_url("custom")
                .expect_err("unknown source stays unresolved");
            let text = err.to_string();
            let hints = err
                .hints
                .iter()
                .map(|hint| hint.message.as_str())
                .collect::<Vec<_>>()
                .join("\n");

            assert!(text.contains("no sourceUrl or .source-url metadata"));
            assert!(hints.contains("homeboy extension install <url> --id custom"));
            assert!(hints.contains(&extension_dir.to_string_lossy().to_string()));
        });
    }

    #[test]
    fn update_all_reports_actionable_missing_source_metadata_skip() {
        with_isolated_home(|home| {
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            write_extension_fixture(&extensions_dir, "custom");

            let result = update_all(false);

            assert_eq!(result.skipped, vec!["custom"]);
            assert_eq!(result.skipped_details.len(), 1);
            assert_eq!(result.skipped_details[0].extension_id, "custom");
            assert!(result.skipped_details[0]
                .reason
                .contains("no sourceUrl or .source-url metadata"));
            assert!(result.skipped_details[0]
                .hints
                .iter()
                .any(|hint| hint.contains("homeboy extension install <url> --id custom")));
        });
    }

    #[test]
    fn is_workdir_clean_non_git_dir_returns_true() {
        // Regression test for Extra-Chill/homeboy#1181: tarball / plain-directory
        // installs (no `.git`) must be treated as clean, since there is no
        // working tree to be dirty in the first place.
        let temp = TempDir::new().expect("create tempdir");
        std::fs::write(temp.path().join("some-file.txt"), "content").expect("write file");

        assert!(
            crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "non-git directory should be treated as clean"
        );
    }

    #[test]
    fn is_workdir_clean_clean_git_repo_returns_true() {
        let temp = TempDir::new().expect("create tempdir");

        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status();
        if init.map(|s| !s.success()).unwrap_or(true) {
            // git not available in this environment; skip.
            return;
        }

        assert!(
            crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "freshly-initialized git repo with no changes should be clean"
        );
    }

    #[test]
    fn is_workdir_clean_dirty_git_repo_returns_false() {
        let temp = TempDir::new().expect("create tempdir");

        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status();
        if init.map(|s| !s.success()).unwrap_or(true) {
            // git not available in this environment; skip.
            return;
        }

        std::fs::write(temp.path().join("untracked.txt"), "hi").expect("write untracked file");

        assert!(
            !crate::core::git::is_workdir_clean_or_not_git(temp.path()),
            "git repo with untracked file should be reported as dirty"
        );
    }
}
