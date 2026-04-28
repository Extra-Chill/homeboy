use crate::config::{self, from_str};
use crate::engine::identifier;
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::git;
use crate::paths;
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
}

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

/// Check if an extension directory is safe to overwrite on update.
///
/// Treats the directory as "clean" when:
/// - It is not a git working tree (tarball / plain-directory installs have no
///   `.git`, so there is no working tree to be dirty in the first place), or
/// - It is a git working tree with no uncommitted changes.
///
/// Returns `false` only when the directory is a git working tree **and** has
/// uncommitted changes. This avoids the historical false positive where
/// `git status` returning a non-zero exit on a non-repo directory was treated
/// as "dirty" (see Extra-Chill/homeboy#1181).
fn is_workdir_clean(path: &Path) -> bool {
    // If the directory is not a git working tree, there is nothing that can
    // be "uncommitted". Short-circuit to clean before running `git status`.
    let inside_tree = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output();

    match inside_tree {
        Ok(output) if output.status.success() => {
            // Fall through: this is a git working tree; check for changes.
        }
        _ => return true,
    }

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match status {
        Ok(output) => output.status.success() && output.stdout.is_empty(),
        // If `status` unexpectedly fails after `rev-parse` succeeded, err on
        // the side of blocking an overwrite so the user can investigate.
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

/// Install every extension declared by a component from the same source.
///
/// Already-installed extensions are skipped so CI setup can be re-run safely.
pub fn install_for_component(
    component: &crate::component::Component,
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
        return install(&extension_path.to_string_lossy(), Some(extension_id));
    }

    install(source, Some(extension_id))
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

    // Linked extensions: resolve the symlink target and pull the source repo.
    // The target may be a subdirectory of a larger repo (e.g. homeboy-extensions/wordpress),
    // so we find the git root and pull from there.
    if is_extension_linked(extension_id) {
        return update_linked_extension(extension_id, &extension_dir, force);
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

/// Update a linked extension by pulling the source repository.
///
/// Linked extensions are symlinks pointing to a source directory, which may be
/// a subdirectory of a monorepo (e.g. `homeboy-extensions/wordpress`). This
/// function resolves the git root, checks out the default branch, and pulls.
fn update_linked_extension(
    extension_id: &str,
    extension_dir: &Path,
    force: bool,
) -> Result<UpdateResult> {
    // Resolve symlink to actual source directory
    let source_dir = std::fs::read_link(extension_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read symlink for {}", extension_id)),
        )
    })?;

    // Find the git root (source may be a subdirectory of a larger repo)
    let git_root_str = git::get_git_root(&source_dir.to_string_lossy())?;
    let git_root = PathBuf::from(&git_root_str);

    if !force && !is_workdir_clean(&git_root) {
        return Err(Error::validation_invalid_argument(
            "extension_id",
            format!(
                "Linked extension '{}' source repo has uncommitted changes. Use --force to proceed.",
                extension_id,
            ),
            Some(extension_id.to_string()),
            None,
        ));
    }

    // Checkout the default branch before pulling.
    // Try main first, fall back to master.
    let default_branch = detect_default_branch(&git_root).unwrap_or_else(|| "main".to_string());
    let checkout_output = Command::new("git")
        .args(["checkout", &default_branch])
        .current_dir(&git_root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    if let Ok(output) = &checkout_output {
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log_status!(
                "extension",
                "Warning: could not checkout {} for {}: {}",
                default_branch,
                extension_id,
                stderr.trim()
            );
            // Continue anyway — pull on current branch is better than nothing
        }
    }

    git::pull_repo(&git_root)?;

    // Auto-run setup if extension defines a setup_command
    if let Ok(extension) = load_extension(extension_id) {
        if extension
            .runtime()
            .is_some_and(|r| r.setup_command.is_some())
        {
            let _ = run_setup(extension_id);
        }
    }

    let url = format!("linked:{}", source_dir.display());
    Ok(UpdateResult {
        extension_id: extension_id.to_string(),
        url,
        path: source_dir,
    })
}

/// Detect the default branch of a git repository.
/// Checks `refs/remotes/origin/HEAD` first, then tries `main` and `master`.
fn detect_default_branch(repo_dir: &Path) -> Option<String> {
    // Try symbolic-ref first (most reliable)
    let output = git_silent(repo_dir, &["symbolic-ref", "refs/remotes/origin/HEAD"])?;
    if output.status.success() {
        let refname = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // refs/remotes/origin/main → main
        return refname.rsplit('/').next().map(|s| s.to_string());
    }

    // Fallback: check if main or master exist as local branches
    for branch in &["main", "master"] {
        let check = git_silent(repo_dir, &["rev-parse", "--verify", branch])?;
        if check.status.success() {
            return Some(branch.to_string());
        }
    }

    None
}

/// Run a git command silently (no stdin/stderr) and return the output.
fn git_silent(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
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

#[cfg(test)]
mod tests {
    use super::{install, install_for_component, is_workdir_clean, read_source_revision, update};
    use crate::component;
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn write_extension_fixture(root: &Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0"
}}"#,
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

    fn prepare_git_extension_repo(repo: &Path, extension_id: &str) -> Option<TempDir> {
        write_extension_fixture(repo, extension_id);
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
                read_source_revision("wordpress"),
                result.source_revision,
                "monorepo installs keep the stored source revision after .git is discarded"
            );
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
            is_workdir_clean(temp.path()),
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
            is_workdir_clean(temp.path()),
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
            !is_workdir_clean(temp.path()),
            "git repo with untracked file should be reported as dirty"
        );
    }
}
