use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::utils::command;

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

/// List all git-tracked markdown files in a directory.
/// Uses `git ls-files` to respect .gitignore and only include tracked/staged files.
/// Returns relative paths from the repository root.
pub fn list_tracked_markdown_files(path: &Path) -> Result<Vec<String>> {
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
