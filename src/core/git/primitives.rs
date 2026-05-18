use std::path::Path;
use std::process::Command;

use crate::core::engine::command;
use crate::core::error::{Error, Result};

/// Clone a git repository to a target directory.
pub fn clone_repo(url: &str, target_dir: &Path) -> Result<()> {
    command::run(
        "git",
        &["clone", url, &target_dir.to_string_lossy()],
        "git clone",
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(())
}

/// Clone a git repository to a target directory and check out a requested ref.
pub fn clone_repo_at_ref(url: &str, target_dir: &Path, revision: Option<&str>) -> Result<()> {
    clone_repo(url, target_dir)?;

    if let Some(revision) = revision {
        command::run_in(
            &target_dir.to_string_lossy(),
            "git",
            &["checkout", "--quiet", revision],
            "git checkout",
        )
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    }

    Ok(())
}

/// Pull latest changes in a git repository.
pub fn pull_repo(repo_dir: &Path) -> Result<()> {
    command::run_in(&repo_dir.to_string_lossy(), "git", &["pull"], "git pull")
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(())
}

/// Check if a git working directory has no uncommitted changes.
///
/// Uses direct Command execution to properly handle empty output (clean repo).
/// `run_in_optional` returns None for empty stdout, which would incorrectly
/// indicate a dirty repo when used with `.unwrap_or(false)`.
pub fn is_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(o) if o.status.success() => o.stdout.is_empty(),
        _ => false, // Command failed = assume not clean (conservative)
    }
}

/// Check if a path is either not a git worktree or is a clean git worktree.
pub fn is_workdir_clean_or_not_git(path: &Path) -> bool {
    let inside_tree = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output();

    match inside_tree {
        Ok(output) if output.status.success() => is_workdir_clean(path),
        _ => true,
    }
}

/// Run a git command in a repository and return stdout.
pub fn run_git(git_root: &Path, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some(context.to_string())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(Error::git_command_failed(if detail.is_empty() {
            context.to_string()
        } else {
            detail
        }));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn current_branch(git_root: &Path) -> Option<String> {
    run_git(
        git_root,
        &["branch", "--show-current"],
        "git current branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

fn default_remote_branch(git_root: &Path) -> Option<String> {
    run_git(
        git_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        "git default remote branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

/// Update a clean linked repo to the latest remote default-branch revision.
pub fn update_to_remote_default_branch(git_root: &Path) -> Result<()> {
    let old_branch = current_branch(git_root);
    run_git(git_root, &["fetch", "origin"], "git fetch origin")?;
    let mut detached_default_branch: Option<String> = None;

    if let Some(remote_branch) = default_remote_branch(git_root) {
        let local_branch = remote_branch
            .strip_prefix("origin/")
            .unwrap_or(&remote_branch)
            .to_string();

        if old_branch.as_deref() != Some(local_branch.as_str())
            && run_git(
                git_root,
                &["switch", &local_branch],
                "git switch default branch",
            )
            .is_err()
        {
            run_git(
                git_root,
                &["switch", "--detach", &remote_branch],
                "git switch detached default branch",
            )?;
            detached_default_branch = Some(local_branch);
        }
    }

    if let Some(branch) = detached_default_branch {
        run_git(
            git_root,
            &["pull", "--ff-only", "origin", &branch],
            "git pull detached default branch --ff-only",
        )?;
    } else {
        run_git(git_root, &["pull", "--ff-only"], "git pull --ff-only")?;
    }

    Ok(())
}

/// Get the short HEAD revision from a git directory.
pub fn short_head_revision(dir: &Path) -> Option<String> {
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

/// List all git-tracked markdown files in a directory.
/// Uses `git ls-files` to respect .gitignore and only include tracked/staged files.
/// Returns relative paths from the repository root.
pub(crate) fn list_tracked_markdown_files(path: &Path) -> Result<Vec<String>> {
    let stdout = command::run_in(
        &path.to_string_lossy(),
        "git",
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "*.md",
        ],
        "git ls-files",
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

pub(crate) fn is_git_repo(path: &str) -> bool {
    command::succeeded_in(path, "git", &["rev-parse", "--git-dir"])
}

/// Get the git repository root directory from any path within the repo.
pub fn get_git_root(path: &str) -> Result<String> {
    command::run_in(path, "git", &["rev-parse", "--show-toplevel"], "git root")
        .map(|s| s.trim().to_string())
        .map_err(|e| Error::git_command_failed(e.to_string()))
}

/// Compute the relative path prefix of a component within a monorepo.
///
/// If `local_path` is a subdirectory of the git root, returns the relative path
/// (e.g. "wordpress" for `/repo/wordpress`). Returns None if local_path IS the
/// git root (not a monorepo component).
pub fn get_component_path_prefix(local_path: &str) -> Option<String> {
    let git_root = get_git_root(local_path).ok()?;
    let root = std::path::Path::new(&git_root).canonicalize().ok()?;
    let component = std::path::Path::new(local_path).canonicalize().ok()?;

    if root == component {
        return None; // Not a monorepo — component IS the repo root
    }

    component
        .strip_prefix(&root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}
