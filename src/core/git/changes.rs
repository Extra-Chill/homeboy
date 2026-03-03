use serde::Serialize;

use crate::error::{Error, Result};

use super::execute_git;

#[derive(Debug, Clone, Serialize)]

pub struct UncommittedChanges {
    pub has_changes: bool,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Parse git status output into structured uncommitted changes.
pub fn get_uncommitted_changes(path: &str) -> Result<UncommittedChanges> {
    let output = execute_git(
        path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git status failed: {}",
            stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for line in stdout.lines() {
        if line.len() < 3 {
            continue;
        }
        let index_status = line.chars().next().unwrap_or(' ');
        let worktree_status = line.chars().nth(1).unwrap_or(' ');
        let file_path = line[3..].to_string();

        match (index_status, worktree_status) {
            ('?', '?') => untracked.push(file_path),
            (idx, wt) => {
                if idx != ' ' && idx != '?' {
                    staged.push(file_path.clone());
                }
                if wt != ' ' && wt != '?' {
                    unstaged.push(file_path);
                }
            }
        }
    }

    let has_changes = !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty();
    let hint = super::operations::build_untracked_hint(path, untracked.len());

    Ok(UncommittedChanges {
        has_changes,
        staged,
        unstaged,
        untracked,
        hint,
    })
}

/// Get file paths changed between a ref and HEAD.
/// Uses `--diff-filter=ACMR` to include only Added, Copied, Modified, Renamed files
/// (excludes Deleted files since there's nothing to lint).
/// Returns repo-relative paths.
pub fn get_files_changed_since(path: &str, git_ref: &str) -> Result<Vec<String>> {
    // Use triple-dot (merge-base) so we get only the changes on the current
    // branch relative to the ref, not changes on the ref's branch.
    let output = execute_git(
        path,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            &format!("{}...HEAD", git_ref),
        ],
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git diff --name-only {}...HEAD failed: {}",
            git_ref, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(files)
}

/// Get diff of uncommitted changes.
pub fn get_diff(path: &str) -> Result<String> {
    // Get both staged and unstaged diff
    let staged = execute_git(path, &["diff", "--cached"])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    let unstaged =
        execute_git(path, &["diff"]).map_err(|e| Error::git_command_failed(e.to_string()))?;

    let staged_diff = String::from_utf8_lossy(&staged.stdout);
    let unstaged_diff = String::from_utf8_lossy(&unstaged.stdout);

    let mut result = String::new();
    if !staged_diff.is_empty() {
        result.push_str("=== Staged Changes ===\n");
        result.push_str(&staged_diff);
    }
    if !unstaged_diff.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("=== Unstaged Changes ===\n");
        result.push_str(&unstaged_diff);
    }

    Ok(result)
}

/// Get diff between baseline ref and HEAD (commit range diff).
pub fn get_range_diff(path: &str, baseline_ref: &str) -> Result<String> {
    let output = execute_git(
        path,
        &["diff", &format!("{}..HEAD", baseline_ref), "--", "."],
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
