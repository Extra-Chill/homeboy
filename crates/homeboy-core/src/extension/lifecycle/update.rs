//! Extension update lifecycle.
//!
//! Cohesive group extracted from the lifecycle root: pulling latest changes for
//! cloned and linked extensions, reconciling source metadata, and reporting
//! available updates. Kept in a sibling module so the lifecycle root stays under
//! the structural line/item thresholds (#5241).

use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy_core::error::{Error, Result};
use homeboy_core::git;
use homeboy_core::paths;

use super::install_sources::{install_linked_shared_assets, rename_dir, resolve_cloned_extension};
use super::{source_metadata, UpdateResult};
use crate::extension::catalog::{is_extension_linked, load_extension};
use homeboy_extension_contract::update_output::ExtensionSourceUpdate;

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

    let initially_clean = is_extension_update_workdir_clean(&extension_dir, &extension_dir).is_ok();
    if !force {
        if !initially_clean {
            let dirty_paths =
                extension_update_dirty_paths(&extension_dir, &extension_dir).unwrap_or_default();
            return Err(Error::validation_invalid_argument(
                "extension_id",
                format!(
                    "Extension has uncommitted changes ({}); update may overwrite them. Use --force to proceed, which applies the update over the listed changes.",
                    dirty_path_detail(&dirty_paths),
                ),
                Some(extension_id.to_string()),
                None,
            ));
        }
    }

    let source = source_metadata::resolve_source_url(extension_id)?;
    let source_url = source.url;
    let mut source_repair = source.repair;

    if extension_dir.join(".git").exists() {
        let old_branch = git::current_branch(&extension_dir);
        let old_revision = git::short_head_revision(&extension_dir);
        let metadata = read_in_place_metadata(&extension_dir);
        if let Err(error) = git::pull_repo(&extension_dir) {
            return Err(compensate_or_preserve_dirty(
                error,
                initially_clean,
                &extension_dir,
                old_branch.as_deref(),
                old_revision.as_deref(),
                &metadata,
            ));
        }
        let new_revision = git::short_head_revision(&extension_dir);
        write_source_metadata(&extension_dir, &source_url, new_revision.clone());
        let refreshed = (|| {
            let manifest = load_extension(extension_id)?;
            homeboy_extension_contract::validate_core_compatibility(
                "extension",
                extension_id,
                manifest
                    .requires
                    .as_ref()
                    .and_then(|requires| requires.homeboy.as_deref()),
                new_revision.clone(),
            )?;
            run_setup_if_configured(extension_id)
        })();
        if let Err(error) = refreshed {
            return Err(compensate_or_preserve_dirty(
                error,
                initially_clean,
                &extension_dir,
                old_branch.as_deref(),
                old_revision.as_deref(),
                &metadata,
            ));
        }

        return Ok(UpdateResult {
            extension_id: extension_id.to_string(),
            url: source_url,
            path: extension_dir.clone(),
            linked: false,
            source_path: None,
            git_root: None,
            source_update: ExtensionSourceUpdate {
                old_source_revision: old_revision,
                new_source_revision: new_revision,
                old_branch,
                new_branch: git::current_branch(&extension_dir),
                ..Default::default()
            },
            repaired_source_metadata: source_repair.take(),
        });
    }

    update_extracted_extension(extension_id, &extension_dir, &source_url)?;

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

/// Refresh every linked extension sharing one Git root as one transaction.
pub fn update_linked_group(extension_ids: &[String], force: bool) -> Result<Vec<UpdateResult>> {
    // One resolution for the whole group. This function's contract is that the
    // group refreshes as ONE transaction; resolving per member meant a repoint
    // mid-loop could span two installations and still report success (#7505).
    let config_root = paths::homeboy()?;
    let mut entries = Vec::new();
    for id in extension_ids {
        let extension_dir = paths::extension_in_root(&config_root, id);
        let source_dir = linked_source_dir(&extension_dir)?;
        let git_root = linked_extension_git_root(id, &source_dir)?;
        entries.push((id.as_str(), extension_dir, source_dir, git_root));
    }
    let Some((_, _, _, git_root)) = entries.first() else {
        return Ok(Vec::new());
    };
    let git_root = git_root.clone();
    if entries.iter().any(|(_, _, _, root)| root != &git_root) {
        return Err(Error::internal_unexpected(
            "linked extension group spans multiple Git roots",
        ));
    }
    let initially_clean = entries
        .iter()
        .all(|(_, _, source, _)| is_extension_update_workdir_clean(&git_root, source).is_ok());
    if !force {
        for (id, _, source, _) in &entries {
            if let Err(paths) = is_extension_update_workdir_clean(&git_root, source) {
                return Err(dirty_linked_error(id, &paths));
            }
        }
    }
    let old_branch = git::current_branch(&git_root);
    let old_revision = git::short_head_revision(&git_root);
    if let Err(error) = git::update_to_remote_default_branch(&git_root) {
        return Err(compensate_or_preserve_dirty_linked(
            error,
            initially_clean,
            &git_root,
            old_branch.as_deref(),
            old_revision.as_deref(),
        ));
    }
    let new_revision = git::short_head_revision(&git_root);
    for (id, extension_dir, source_dir, _) in &entries {
        if let Err(error) = refresh_linked_extension_install(id, source_dir, extension_dir) {
            if initially_clean {
                restore_linked_checkout(&git_root, old_branch.as_deref(), old_revision.as_deref());
                for (_, extension_dir, source_dir, _) in &entries {
                    let _ = install_linked_shared_assets(source_dir, extension_dir, None);
                }
            }
            return Err(preserve_dirty_error(error, initially_clean, &git_root));
        }
    }
    Ok(entries
        .into_iter()
        .map(|(id, _, source_dir, _)| UpdateResult {
            extension_id: id.to_string(),
            url: format!("linked:{}", source_dir.display()),
            path: source_dir.clone(),
            linked: true,
            source_path: Some(source_dir),
            git_root: Some(git_root.clone()),
            source_update: ExtensionSourceUpdate {
                old_source_revision: old_revision.clone(),
                new_source_revision: new_revision.clone(),
                old_branch: old_branch.clone(),
                new_branch: git::current_branch(&git_root),
                update_note: Some("Linked source group updated transactionally.".to_string()),
            },
            repaired_source_metadata: None,
        })
        .collect())
}

fn linked_source_dir(extension_dir: &Path) -> Result<PathBuf> {
    let source_dir = std::fs::read_link(extension_dir).map_err(|e| {
        Error::internal_io(e.to_string(), Some("read linked extension".to_string()))
    })?;
    let source_dir = if source_dir.is_absolute() {
        source_dir
    } else {
        extension_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(source_dir)
    };
    Ok(source_dir.canonicalize().unwrap_or(source_dir))
}

fn dirty_linked_error(extension_id: &str, paths: &[String]) -> Error {
    Error::validation_invalid_argument("extension_id", format!("Linked extension source repo has uncommitted changes for {}: {}. Use --force to proceed, which applies the update over the listed changes.", extension_id, dirty_path_detail(paths)), Some(extension_id.to_string()), None)
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

    let requested_ref = read_source_requested_ref(extension_dir);
    git::clone_repo_at_ref(source_url, &clone_dir, requested_ref.as_deref())?;
    let source_revision = git::short_head_revision(&clone_dir);

    let result = resolve_cloned_extension(&clone_dir, extension_id, &staged_dir, source_url);
    if clone_dir.exists() {
        let _ = std::fs::remove_dir_all(&clone_dir);
    }
    result?;

    write_source_metadata(&staged_dir, source_url, source_revision.clone());
    write_requested_source_ref(&staged_dir, requested_ref.as_deref());

    rename_dir(extension_dir, &backup_dir)?;
    if let Err(err) = rename_dir(&staged_dir, extension_dir) {
        let _ = rename_dir(&backup_dir, extension_dir);
        return Err(err);
    }

    let refreshed = (|| {
        let manifest = load_extension(extension_id)?;
        homeboy_extension_contract::validate_core_compatibility(
            "extension",
            extension_id,
            manifest
                .requires
                .as_ref()
                .and_then(|requires| requires.homeboy.as_deref()),
            source_revision.clone(),
        )?;
        run_setup_if_configured(extension_id)
    })();
    if let Err(err) = refreshed {
        let _ = std::fs::remove_dir_all(extension_dir);
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
    let metadata_dir = homeboy_core::extension_update_check::source_metadata_dir(extension_dir);
    let revision_path = metadata_dir.join(source_metadata_file(extension_dir, "revision"));
    if let Some(rev) = source_revision {
        let _ = std::fs::write(revision_path, rev);
    } else {
        let _ = std::fs::remove_file(revision_path);
    }
    let _ = std::fs::write(
        metadata_dir.join(source_metadata_file(extension_dir, "url")),
        source_url,
    );
}

/// Persist the user-requested source ref separately from the resolved revision.
/// Extracted monorepo installs discard `.git`, so this is the only durable
/// input that lets a later update preserve a branch, tag, or commit pin.
pub(crate) fn write_requested_source_ref(extension_dir: &Path, requested_ref: Option<&str>) {
    let path = homeboy_core::extension_update_check::source_metadata_dir(extension_dir)
        .join(source_metadata_file(extension_dir, "requested-ref"));
    match requested_ref.filter(|value| !value.trim().is_empty()) {
        Some(value) => {
            let _ = std::fs::write(path, value);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn read_source_requested_ref(extension_dir: &Path) -> Option<String> {
    homeboy_core::extension_update_check::read_source_metadata_value(extension_dir, "requested-ref")
}

/// The extension update gate. `Ok(())` when the worktree holds nothing an
/// update would overwrite; `Err(dirty_paths)` names the repo-relative paths
/// that block it. An unreadable git status also refuses (with no names) so an
/// update never proceeds over unknown state. Only generated metadata Homeboy
/// itself writes (`.source-url`, `.source-revision`) is tolerated.
pub(crate) fn is_extension_update_workdir_clean(
    git_root: &Path,
    extension_dir: &Path,
) -> std::result::Result<(), Vec<String>> {
    match extension_update_dirty_paths(git_root, extension_dir) {
        Some(paths) if paths.is_empty() => Ok(()),
        Some(paths) => Err(paths),
        None => Err(Vec::new()),
    }
}

/// Repo-relative paths with changes an extension update would overwrite,
/// ignoring the generated metadata paths Homeboy itself writes
/// (`.source-url`, `.source-revision`). `Some` when the worktree state is
/// known (empty means clean enough to refresh); `None` when it cannot be
/// determined. Public so the upgrade path can reuse the exact gate tolerance
/// when deciding whether a symlinked clone can be refreshed with a plain
/// `pull --ff-only` (#12181).
pub fn extension_update_dirty_paths(git_root: &Path, extension_dir: &Path) -> Option<Vec<String>> {
    if !git::is_git_repo(&git_root.to_string_lossy()) {
        return Some(Vec::new());
    }

    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(git_root)
        .output()
    else {
        return None;
    };
    if !output.status.success() {
        return None;
    }

    let extension_rel = extension_dir
        .strip_prefix(git_root)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let status = String::from_utf8_lossy(&output.stdout);

    let dirty = status
        .lines()
        .filter_map(dirty_path_from_status_line)
        .filter(|path| !is_generated_extension_metadata_path(path, extension_rel.as_deref()))
        .collect();
    Some(dirty)
}

fn dirty_path_from_status_line(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').replace('\\', "/"))
}

/// Human detail for the dirty paths blocking an update. Falls back to an
/// explicit "cannot be verified" when the gate refused without names (e.g. git
/// status unavailable), so the message never names an empty list.
fn dirty_path_detail(dirty_paths: &[String]) -> String {
    if dirty_paths.is_empty() {
        "state could not be verified".to_string()
    } else {
        dirty_paths.join(", ")
    }
}

fn is_generated_extension_metadata_path(path: &str, extension_rel: Option<&str>) -> bool {
    [".source-url", ".source-revision"].iter().any(|name| {
        path == *name
            || extension_rel
                .filter(|rel| !rel.is_empty())
                .is_some_and(|rel| path == format!("{rel}/{name}"))
    })
}

pub(crate) fn run_setup_if_configured(extension_id: &str) -> Result<()> {
    let extension = load_extension(extension_id)?;
    if extension
        .runtime()
        .is_some_and(|r| r.setup_command.is_some())
    {
        super::super::execution::run_setup(extension_id)?;
    }
    Ok(())
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
    let git_root = linked_extension_git_root(extension_id, &source_dir)?;
    let old_branch = git::current_branch(&git_root);
    let old_source_revision = git::short_head_revision(&git_root);

    // Capture this before any branch switch or pull. Forced dirty updates may
    // proceed, but their user state is never eligible for destructive rollback.
    let initially_clean = is_extension_update_workdir_clean(&git_root, &source_dir).is_ok();
    if !force {
        if !initially_clean {
            let dirty_paths =
                extension_update_dirty_paths(&git_root, &source_dir).unwrap_or_default();
            return Err(Error::validation_invalid_argument(
                "extension_id",
                format!(
                    "Linked extension source repo has uncommitted changes for {}: {}. Use --force to proceed, which applies the update over the listed changes.",
                    extension_id,
                    dirty_path_detail(&dirty_paths),
                ),
                Some(extension_id.to_string()),
                None,
            ));
        }
    }

    // Observe the remote on every convergence. A process-lifetime cache would
    // hide a newer revision from a subsequent explicit convergence command.
    if let Err(error) = git::update_to_remote_default_branch(&git_root) {
        return Err(compensate_or_preserve_dirty_linked(
            error,
            initially_clean,
            &git_root,
            old_branch.as_deref(),
            old_source_revision.as_deref(),
        ));
    }
    if let Err(error) = refresh_linked_extension_install(extension_id, &source_dir, extension_dir) {
        // The clean-worktree gate above proves this transaction owns every
        // changed path. Restore the prior checkout before returning so a failed
        // setup or incompatible refreshed manifest never strands a user on a
        // partially refreshed linked source.
        if initially_clean {
            restore_linked_checkout(
                &git_root,
                old_branch.as_deref(),
                old_source_revision.as_deref(),
            );
            let _ = install_linked_shared_assets(&source_dir, extension_dir, None);
        }
        return Err(preserve_dirty_error(error, initially_clean, &git_root));
    }
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

fn refresh_linked_extension_install(
    extension_id: &str,
    source_dir: &Path,
    extension_dir: &Path,
) -> Result<()> {
    install_linked_shared_assets(source_dir, extension_dir, None)?;
    let manifest = load_extension(extension_id)?;
    homeboy_extension_contract::validate_core_compatibility(
        "extension",
        extension_id,
        manifest
            .requires
            .as_ref()
            .and_then(|requires| requires.homeboy.as_deref()),
        git::short_head_revision(source_dir),
    )?;
    run_setup_if_configured(extension_id)
}

/// Best-effort compensation after a linked refresh failed after its clean gate.
/// The gate prevents this from resetting a pre-existing dirty user worktree.
fn restore_linked_checkout(git_root: &Path, branch: Option<&str>, revision: Option<&str>) {
    if let Some(branch) = branch {
        let _ = Command::new("git")
            .args(["checkout", branch])
            .current_dir(git_root)
            .output();
    }
    if let Some(revision) = revision {
        let _ = Command::new("git")
            .args(["reset", "--hard", revision])
            .current_dir(git_root)
            .output();
    }
}

fn read_in_place_metadata(extension_dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    [".source-url", ".source-revision", ".source-requested-ref"]
        .into_iter()
        .map(|name| {
            let path = extension_dir.join(name);
            let contents = std::fs::read(&path).ok();
            (path, contents)
        })
        .collect()
}

fn restore_in_place_extension(
    extension_dir: &Path,
    branch: Option<&str>,
    revision: Option<&str>,
    metadata: &[(PathBuf, Option<Vec<u8>>)],
) {
    restore_linked_checkout(extension_dir, branch, revision);
    for (path, contents) in metadata {
        match contents {
            Some(contents) => {
                let _ = std::fs::write(path, contents);
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn compensate_or_preserve_dirty(
    error: Error,
    initially_clean: bool,
    extension_dir: &Path,
    branch: Option<&str>,
    revision: Option<&str>,
    metadata: &[(PathBuf, Option<Vec<u8>>)],
) -> Error {
    if initially_clean {
        restore_in_place_extension(extension_dir, branch, revision, metadata);
    }
    preserve_dirty_error(error, initially_clean, extension_dir)
}

fn compensate_or_preserve_dirty_linked(
    error: Error,
    initially_clean: bool,
    git_root: &Path,
    branch: Option<&str>,
    revision: Option<&str>,
) -> Error {
    if initially_clean {
        restore_linked_checkout(git_root, branch, revision);
    }
    preserve_dirty_error(error, initially_clean, git_root)
}

fn preserve_dirty_error(error: Error, initially_clean: bool, worktree: &Path) -> Error {
    if initially_clean {
        return error;
    }
    error
        .with_hint(format!(
            "Forced update began with user changes in {}; Homeboy did not reset, checkout, or restore this worktree after the failure.",
            worktree.display()
        ))
        .with_hint(format!(
            "Inspect the uncompensated worktree and reconcile it manually: git -C {} status --short && git -C {} diff",
            worktree.display(), worktree.display()
        ))
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rollback_restores_the_prior_clean_revision() {
        let repo = tempdir().expect("repo");
        if !git_succeeds(repo.path(), &["init", "--quiet"]) {
            return;
        }
        std::fs::write(repo.path().join("extension.json"), "before").expect("before");
        if !git_succeeds(repo.path(), &["add", "."])
            || !git_succeeds(
                repo.path(),
                &[
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "user.name=test",
                    "commit",
                    "-m",
                    "before",
                    "--quiet",
                ],
            )
        {
            return;
        }
        let before = git::short_head_revision(repo.path()).expect("before revision");
        std::fs::write(repo.path().join("extension.json"), "after").expect("after");
        assert!(git_succeeds(repo.path(), &["add", "."]));
        assert!(git_succeeds(
            repo.path(),
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "-m",
                "after",
                "--quiet",
            ],
        ));

        restore_linked_checkout(repo.path(), None, Some(&before));

        assert_eq!(
            git::short_head_revision(repo.path()).as_deref(),
            Some(before.as_str())
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("extension.json"))
                .expect("restored extension content"),
            "before"
        );
    }

    #[test]
    fn pull_failure_rollback_restores_the_prior_branch() {
        let repo = committed_repo();
        let branch = git::current_branch(repo.path()).expect("initial branch");
        assert!(git_succeeds(
            repo.path(),
            &["checkout", "-b", "other", "--quiet"]
        ));
        std::fs::write(repo.path().join("extension.json"), "other").expect("other content");
        assert!(git_succeeds(repo.path(), &["add", "."]));
        assert!(commit(repo.path(), "other"));
        let prior = git::short_head_revision(repo.path()).expect("prior revision");

        assert!(git_succeeds(repo.path(), &["checkout", &branch, "--quiet"]));
        // This models `switch` succeeding before a subsequent pull fails.
        restore_linked_checkout(repo.path(), Some("other"), Some(&prior));

        assert_eq!(git::current_branch(repo.path()).as_deref(), Some("other"));
        assert_eq!(
            git::short_head_revision(repo.path()).as_deref(),
            Some(prior.as_str())
        );
    }

    #[test]
    fn in_place_incompatible_rollback_restores_checkout_and_metadata() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        let metadata = read_in_place_metadata(repo.path());
        std::fs::write(repo.path().join(".source-revision"), "new").expect("new metadata");
        std::fs::write(repo.path().join("extension.json"), "incompatible").expect("new manifest");
        assert!(git_succeeds(repo.path(), &["add", "extension.json"]));
        assert!(commit(repo.path(), "incompatible"));

        restore_in_place_extension(repo.path(), None, Some(&prior), &metadata);

        assert_eq!(
            git::short_head_revision(repo.path()).as_deref(),
            Some(prior.as_str())
        );
        assert!(!repo.path().join(".source-revision").exists());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("extension.json")).expect("content"),
            "before"
        );
    }

    #[test]
    fn forced_dirty_failure_preserves_user_changes_without_rollback() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        let metadata = read_in_place_metadata(repo.path());
        std::fs::write(repo.path().join("user-notes.txt"), "keep me").expect("dirty user file");
        std::fs::write(repo.path().join("extension.json"), "failed refreshed state")
            .expect("failed refresh mutation");

        let error = compensate_or_preserve_dirty(
            Error::internal_unexpected("setup failed"),
            false,
            repo.path(),
            None,
            Some(&prior),
            &metadata,
        );

        assert!(repo.path().join("user-notes.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("extension.json")).expect("mutated content"),
            "failed refreshed state"
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("did not reset")));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("git -C")));
    }

    #[test]
    fn forced_dirty_pull_failure_preserves_user_changes_without_rollback() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        let metadata = read_in_place_metadata(repo.path());
        std::fs::write(repo.path().join("user-notes.txt"), "keep me").expect("dirty user file");

        let error = compensate_or_preserve_dirty(
            Error::internal_unexpected("pull failed"),
            false,
            repo.path(),
            None,
            Some(&prior),
            &metadata,
        );

        assert!(repo.path().join("user-notes.txt").exists());
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("did not reset")));
    }

    #[test]
    fn forced_dirty_linked_failure_preserves_user_changes_without_rollback() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        std::fs::write(repo.path().join("user-notes.txt"), "keep me").expect("dirty user file");
        std::fs::write(repo.path().join("extension.json"), "failed refreshed state")
            .expect("failed refresh mutation");

        let error = compensate_or_preserve_dirty_linked(
            Error::internal_unexpected("compatibility failed"),
            false,
            repo.path(),
            None,
            Some(&prior),
        );

        assert!(repo.path().join("user-notes.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("extension.json")).expect("mutated content"),
            "failed refreshed state"
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("uncompensated")));
    }

    #[test]
    fn single_linked_forced_dirty_pull_failure_preserves_user_changes() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        std::fs::write(repo.path().join("user-notes.txt"), "keep me").expect("dirty user file");

        let error = compensate_or_preserve_dirty_linked(
            Error::internal_unexpected("remote pull failed"),
            false,
            repo.path(),
            None,
            Some(&prior),
        );

        assert!(repo.path().join("user-notes.txt").exists());
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("did not reset")));
    }

    #[test]
    fn single_linked_forced_dirty_post_refresh_failure_preserves_user_changes() {
        let repo = committed_repo();
        let prior = git::short_head_revision(repo.path()).expect("prior revision");
        std::fs::write(repo.path().join("user-notes.txt"), "keep me").expect("dirty user file");
        std::fs::write(
            repo.path().join("extension.json"),
            "incompatible refreshed state",
        )
        .expect("post-refresh mutation");

        let error = preserve_dirty_error(
            Error::internal_unexpected("compatibility failed"),
            false,
            repo.path(),
        );

        assert!(repo.path().join("user-notes.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("extension.json")).expect("mutated content"),
            "incompatible refreshed state"
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("reconcile")));
        assert!(git::short_head_revision(repo.path()).is_some_and(|revision| revision == prior));
    }

    #[cfg(unix)]
    #[test]
    fn linked_group_resolves_relative_extension_target() {
        let home = tempfile::tempdir().expect("home");
        let extensions = home.path().join("extensions");
        let source = extensions.join("sources/fixture");
        std::fs::create_dir_all(&source).expect("source");
        assert!(git_succeeds(&source, &["init", "--quiet"]));
        std::fs::write(
            source.join("fixture.json"),
            r#"{"name":"Fixture","version":"1.0.0"}"#,
        )
        .expect("manifest");
        assert!(git_succeeds(&source, &["add", "."]));
        assert!(commit(&source, "initial"));
        std::fs::create_dir_all(&extensions).expect("extensions");
        std::os::unix::fs::symlink("sources/fixture", extensions.join("fixture"))
            .expect("relative link");

        assert_eq!(
            linked_source_dir(&extensions.join("fixture")).expect("relative target"),
            source.canonicalize().expect("canonical source")
        );
    }

    fn committed_repo() -> tempfile::TempDir {
        let repo = tempdir().expect("repo");
        assert!(git_succeeds(repo.path(), &["init", "--quiet"]));
        std::fs::write(repo.path().join("extension.json"), "before").expect("before");
        assert!(git_succeeds(repo.path(), &["add", "."]));
        assert!(commit(repo.path(), "before"));
        repo
    }

    fn commit(path: &Path, message: &str) -> bool {
        git_succeeds(
            path,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "-m",
                message,
                "--quiet",
            ],
        )
    }

    fn git_succeeds(path: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .is_ok_and(|status| status.success())
    }
}

fn linked_extension_git_root(extension_id: &str, source_dir: &Path) -> Result<PathBuf> {
    let mut candidate = Some(source_dir);
    while let Some(path) = candidate {
        if let Ok(git_root_str) = git::get_git_root(&path.to_string_lossy()) {
            return Ok(PathBuf::from(&git_root_str)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(git_root_str)));
        }
        candidate = path.parent();
    }

    Err(Error::validation_invalid_argument(
        "extension_id",
        format!(
            "Linked extension '{}' points at {}, but that path is not inside a git checkout Homeboy can update.",
            extension_id,
            source_dir.display()
        ),
        Some(extension_id.to_string()),
        None,
    )
    .with_hint(format!(
        "Repair by reinstalling from the original source: homeboy extension install <source-path-or-url> --id {} --reinstall",
        extension_id
    )))
}

fn source_metadata_file(extension_dir: &std::path::Path, kind: &str) -> String {
    if extension_dir.is_symlink() {
        if let Some(name) = extension_dir.file_name().and_then(|name| name.to_str()) {
            return format!(".{name}.source-{kind}");
        }
    }

    format!(".source-{kind}")
}
