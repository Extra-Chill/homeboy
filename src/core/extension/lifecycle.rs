use crate::config::{self, from_str};
use crate::error::{Error, Result};
use crate::git;
use crate::local_files::{self, FileSystem};
use crate::paths;
use crate::utils::slugify;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::execution::run_setup;
use super::manifest::ExtensionManifest;
use super::{is_extension_linked, load_extension};

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
}

pub fn slugify_id(value: &str) -> Result<String> {
    slugify::slugify_id(value, "extension_id")
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

/// Check if a git working directory is clean (no uncommitted changes).
fn is_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(output) => output.status.success() && output.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Returns the path to a extension's manifest file: {extension_dir}/{id}.json
fn manifest_path_for_extension(extension_dir: &Path, id: &str) -> PathBuf {
    extension_dir.join(format!("{}.json", id))
}

/// Install a extension from a git URL or link a local directory.
/// Automatically detects whether source is a URL (git clone) or local path (symlink).
pub fn install(source: &str, id_override: Option<&str>) -> Result<InstallResult> {
    if is_git_url(source) {
        install_from_url(source, id_override)
    } else {
        install_from_path(source, id_override)
    }
}

/// Install a extension by cloning from a git repository URL.
///
/// Handles both single-extension repos (manifest at repo root) and monorepos
/// (manifest in a subdirectory matching the extension ID). For monorepos,
/// extracts just the target subdirectory.
fn install_from_url(url: &str, id_override: Option<&str>) -> Result<InstallResult> {
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

/// After cloning a repo to a temp dir, figure out whether it's a single-extension
/// repo or a monorepo and move the right content to the final extension directory.
///
/// Returns the installed extension ID on success.
fn resolve_cloned_extension(
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
fn rename_dir(from: &Path, to: &Path) -> Result<()> {
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
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create target dir".into())))?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read source dir".into())))?
    {
        let entry =
            entry.map_err(|e| Error::internal_io(e.to_string(), Some("read dir entry".into())))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| Error::internal_io(e.to_string(), Some("copy file".into())))?;
        }
    }
    Ok(())
}

/// Install a extension by symlinking a local directory.
fn install_from_path(source_path: &str, id_override: Option<&str>) -> Result<InstallResult> {
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

    // Create symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, &extension_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source, &extension_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create symlink".to_string())))?;

    // For linked (local) extensions, read revision from the source dir if it's a git repo
    let source_revision = get_short_head_revision(&source);

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

    // Linked extensions are managed externally
    if is_extension_linked(extension_id) {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!(
                "Extension '{}' is linked. Update the source directory directly.",
                extension_id
            ),
            Some(extension_id.to_string()),
            None,
        ));
    }

    if !force && !is_workdir_clean(&extension_dir) {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            "Extension has uncommitted changes; update may overwrite them. Use --force to proceed.",
            Some(extension_id.to_string()),
            None,
        ));
    }

    let extension = load_extension(extension_id)?;

    let source_url = extension.source_url.ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension_id",
            format!(
                "Extension '{}' has no sourceUrl. Reinstall with 'homeboy extension install <url>'.",
                extension_id
            ),
            Some(extension_id.to_string()),
            None,
        )
    })?;

    git::pull_repo(&extension_dir)?;

    // Update .source-revision after pull so it stays current
    if let Some(rev) = get_short_head_revision(&extension_dir) {
        let _ = std::fs::write(extension_dir.join(".source-revision"), &rev);
    }

    // Auto-run setup if extension defines a setup_command
    // Setup is best-effort: update succeeds even if setup fails
    if let Ok(extension) = load_extension(extension_id) {
        if extension
            .runtime()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = run_setup(extension_id);
        }
    }

    Ok(UpdateResult {
        extension_id: extension_id.to_string(),
        url: source_url,
        path: extension_dir,
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

/// Get the short HEAD revision from a git directory.
/// Returns None if the directory is not a git repo or the command fails.
fn get_short_head_revision(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rev.is_empty() {
        None
    } else {
        Some(rev)
    }
}

/// Read the source revision for an installed extension.
/// Checks (in order): .git directory (git rev-parse), then .source-revision file.
pub fn read_source_revision(extension_id: &str) -> Option<String> {
    let extension_dir = paths::extension(extension_id).ok()?;
    if !extension_dir.exists() {
        return None;
    }

    // Try .git first (single-extension repos and linked extensions)
    if let Some(rev) = get_short_head_revision(&extension_dir) {
        return Some(rev);
    }

    // Fall back to .source-revision file (monorepo installs)
    let rev_file = extension_dir.join(".source-revision");
    std::fs::read_to_string(&rev_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
