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
///
/// Prefers triple-dot (`ref...HEAD`) to get only changes on the current branch
/// relative to the merge base. Falls back to two-dot (`ref..HEAD`) when the
/// merge base is unavailable (e.g. shallow clones in CI).
pub fn get_files_changed_since(path: &str, git_ref: &str) -> Result<Vec<String>> {
    // Try triple-dot first (merge-base diff) — shows only changes on the
    // current branch, not changes on the ref's branch.
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

    if output.status.success() {
        return parse_diff_output(&output.stdout);
    }

    // Triple-dot failed (likely shallow clone — no merge base available).
    // Fall back to two-dot diff which only needs both commits to exist.
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!(
        "Three-dot diff failed ({}), falling back to two-dot diff",
        stderr.trim()
    );

    let fallback = execute_git(
        path,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            &format!("{}..HEAD", git_ref),
        ],
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    if !fallback.status.success() {
        let fallback_stderr = String::from_utf8_lossy(&fallback.stderr);
        return Err(Error::git_command_failed(format!(
            "git diff --name-only {}..HEAD failed: {}",
            git_ref, fallback_stderr
        )));
    }

    parse_diff_output(&fallback.stdout)
}

/// Get all dirty files in the working tree (modified, new, deleted).
/// Returns repo-relative paths. Useful for detecting what changed between
/// operations on the working tree.
pub fn get_dirty_files(path: &str) -> Result<Vec<String>> {
    let changes = get_uncommitted_changes(path)?;
    let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    files.extend(changes.staged);
    files.extend(changes.unstaged);
    files.extend(changes.untracked);
    Ok(files.into_iter().collect())
}

/// Parse `git diff --name-only` output into a list of file paths.
fn parse_diff_output(stdout: &[u8]) -> Result<Vec<String>> {
    let text = String::from_utf8_lossy(stdout);
    let files: Vec<String> = text
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_uncommitted_changes_default_path() {
        let path = "";
        let _result = get_uncommitted_changes(&path);
    }

    #[test]
    fn test_get_uncommitted_changes_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_uncommitted_changes(&path);
    }

    #[test]
    fn test_get_files_changed_since_default_path() {
        let path = "";
        let git_ref = "";
        let _result = get_files_changed_since(&path, &git_ref);
    }

    #[test]
    fn test_get_files_changed_since_default_path_2() {
        let path = "";
        let git_ref = "";
        let _result = get_files_changed_since(&path, &git_ref);
    }

    #[test]
    fn test_get_files_changed_since_has_expected_effects() {
        // Expected effects: logging
        let path = "";
        let git_ref = "";
        let _ = get_files_changed_since(&path, &git_ref);
    }

    #[test]
    fn test_get_dirty_files_default_path() {
        let path = "";
        let _result = get_dirty_files(&path);
    }

    #[test]
    fn test_get_dirty_files_ok_files_into_iter_collect() {
        let path = "";
        let result = get_dirty_files(&path);
        assert!(result.is_ok(), "expected Ok for: Ok(files.into_iter().collect())");
    }

    #[test]
    fn test_get_dirty_files_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_dirty_files(&path);
    }

    #[test]
    fn test_get_diff_default_path() {
        let path = "";
        let _result = get_diff(&path);
    }

    #[test]
    fn test_get_diff_default_path_2() {
        let path = "";
        let _result = get_diff(&path);
    }

    #[test]
    fn test_get_diff_ok_result() {
        let path = "";
        let result = get_diff(&path);
        assert!(result.is_ok(), "expected Ok for: Ok(result)");
    }

    #[test]
    fn test_get_diff_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_diff(&path);
    }

    #[test]
    fn test_get_range_diff_default_path() {
        let path = "";
        let baseline_ref = "";
        let _result = get_range_diff(&path, &baseline_ref);
    }

    #[test]
    fn test_get_range_diff_ok_string_from_utf8_lossy_output_stdout_to_string() {
        let path = "";
        let baseline_ref = "";
        let result = get_range_diff(&path, &baseline_ref);
        assert!(result.is_ok(), "expected Ok for: Ok(String::from_utf8_lossy(&output.stdout).to_string())");
    }

}
