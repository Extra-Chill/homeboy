use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::Serialize;

use crate::core::error::{Error, Result};

use super::execute_git;

const VERBOSE_UNTRACKED_THRESHOLD: usize = 200;

const NOISY_UNTRACKED_DIRS: [&str; 8] = [
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    "vendor",
    "target",
    ".cache",
];

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
///
/// Refreshes the git index (`git update-index --refresh`) before reading
/// status so stale stat info from upstream tooling — common in CI after
/// `git pull --ff-only`, build steps, or extension setup steps that
/// touch tracked files without changing their content — does not surface
/// as false-positive "modified" entries. The refresh never modifies file
/// contents; it only updates the index's mtime/size cache so `git status`
/// reports the true content state.
pub fn get_uncommitted_changes(path: &str) -> Result<UncommittedChanges> {
    // Best-effort: ignore failures (returns nonzero when files have real
    // changes, which is exactly what `git status` is about to report anyway).
    let _ = execute_git(path, &["update-index", "--refresh"]);

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
    let hint = build_untracked_hint(path, untracked.len());

    Ok(UncommittedChanges {
        has_changes,
        staged,
        unstaged,
        untracked,
        hint,
    })
}

fn build_untracked_hint(path: &str, untracked_count: usize) -> Option<String> {
    if untracked_count < VERBOSE_UNTRACKED_THRESHOLD {
        return None;
    }

    let ignored_output = execute_git(path, &["status", "--ignored", "--porcelain=v1"]).ok()?;
    if !ignored_output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&ignored_output.stdout);
    let ignored_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("!!"))
        .collect();
    if ignored_lines.is_empty() {
        return None;
    }

    let mut noisy_ignored = Vec::new();
    for line in ignored_lines {
        let path = line[3..].trim();
        for dir in NOISY_UNTRACKED_DIRS {
            if path == dir || path.starts_with(&format!("{}/", dir)) {
                noisy_ignored.push(dir.to_string());
                break;
            }
        }
    }

    noisy_ignored.sort();
    noisy_ignored.dedup();

    if noisy_ignored.is_empty() {
        return None;
    }

    Some(format!(
        "Large untracked list detected ({}). Common noisy directories ignored by git: {}. If output feels too big, add them to .gitignore.",
        untracked_count,
        noisy_ignored.join(", ")
    ))
}

/// Get file paths changed between a ref and HEAD.
/// Uses `--diff-filter=ACMR` to include only Added, Copied, Modified, Renamed files
/// (excludes Deleted files since there's nothing to lint).
/// Returns repo-relative paths.
///
/// Uses triple-dot (`ref...HEAD`) to get only changes on the current branch
/// relative to the merge base. In shallow clones (common in CI), the merge base
/// may not be reachable — the function progressively deepens the repository
/// until the ancestry is available.
///
/// Fails explicitly if the merge base cannot be resolved. No silent fallbacks.
pub fn get_files_changed_since(path: &str, git_ref: &str) -> Result<Vec<String>> {
    // Ensure the ref's ancestry is reachable (handles shallow CI clones).
    ensure_ancestry_for_ref(path, git_ref)?;

    // Triple-dot (merge-base diff) — shows only changes on the current
    // branch, not changes on the ref's branch.
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
        let mut files: BTreeSet<String> = parse_diff_output(&output.stdout)?.into_iter().collect();
        files.extend(get_working_tree_files_for_changed_since(path)?);
        return Ok(files.into_iter().collect());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::git_command_failed(format!(
        "git diff {}...HEAD failed: {}",
        git_ref,
        stderr.trim()
    )))
}

/// Get staged, unstaged, and untracked files that should participate in a
/// changed-since scope before they are committed.
///
/// Uses the same add/copy/modify/rename filter as the committed diff path, so
/// deleted files are not returned to lint/test scopes that need existing files.
fn get_working_tree_files_for_changed_since(path: &str) -> Result<Vec<String>> {
    // Best-effort index refresh keeps stat-only touches from surfacing as dirty.
    let _ = execute_git(path, &["update-index", "--refresh"]);

    let mut files: BTreeSet<String> = BTreeSet::new();

    for args in [
        vec!["diff", "--name-only", "--diff-filter=ACMR"],
        vec!["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let output =
            execute_git(path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git_command_failed(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        files.extend(parse_diff_output(&output.stdout)?);
    }

    Ok(files.into_iter().collect())
}

/// Check whether the repo is a shallow clone.
fn is_shallow_repo(path: &str) -> bool {
    execute_git(path, &["rev-parse", "--is-shallow-repository"])
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Check whether `git merge-base <ref> HEAD` succeeds (the ref's ancestry
/// is reachable from HEAD).
fn has_merge_base(path: &str, git_ref: &str) -> bool {
    execute_git(path, &["merge-base", git_ref, "HEAD"])
        .ok()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// In shallow clones, the merge base between a ref and HEAD may not be
/// reachable. This function progressively deepens the repository until the
/// merge base is available.
///
/// Deepening strategy: 50 → 200 → full unshallow. This matches what CI
/// environments typically need — most PRs have <50 commits, so the first
/// deepen usually suffices.
///
/// Returns an error if the merge base cannot be resolved after all attempts.
fn ensure_ancestry_for_ref(path: &str, git_ref: &str) -> Result<()> {
    // Fast path: merge base already reachable (full clone or sufficient depth).
    if has_merge_base(path, git_ref) {
        return Ok(());
    }

    // Only deepen if this is actually a shallow clone. In a full clone,
    // a missing merge base means the ref itself is invalid — deepening won't help.
    if !is_shallow_repo(path) {
        return Err(Error::git_command_failed(format!(
            "Cannot resolve merge base for {git_ref}: ref is not reachable and repository is not shallow (is the ref valid?)"
        )));
    }

    eprintln!("Shallow clone detected — deepening to resolve merge base for {git_ref}");

    // Fetch the ref itself if it's not already present.
    let remote = super::resolve_default_remote(Path::new(path));
    let _ = execute_git(path, &["fetch", &remote, git_ref, "--depth=50"]);

    // Progressive deepening: try increasingly generous depths.
    for depth in &["50", "200"] {
        let _ = execute_git(path, &["fetch", "--deepen", depth]);
        if has_merge_base(path, git_ref) {
            eprintln!("Merge base found after deepening by {depth} commits");
            return Ok(());
        }
    }

    // Last resort: full unshallow.
    eprintln!("Merge base not found with depth 200, unshallowing repository");
    let _ = execute_git(path, &["fetch", "--unshallow"]);

    if has_merge_base(path, git_ref) {
        eprintln!("Merge base found after full unshallow");
        Ok(())
    } else {
        Err(Error::git_command_failed(format!(
            "Cannot resolve merge base for {git_ref} even after full unshallow — the ref may not exist in the remote"
        )))
    }
}

/// Ensure a ref's merge base with HEAD is reachable, then return it.
///
/// Handles shallow CI clones by progressively deepening the repository
/// (the same strategy as [`get_files_changed_since`]). Returns the resolved
/// merge-base SHA on success, or an error if the ancestry cannot be made
/// reachable. Callers that want a deterministic "fall back to full scope"
/// behavior should treat the error as a signal to skip changed-since scoping
/// rather than aborting.
pub fn resolve_merge_base(path: &str, git_ref: &str) -> Result<String> {
    ensure_ancestry_for_ref(path, git_ref)?;

    let output = execute_git(path, &["merge-base", git_ref, "HEAD"])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git merge-base {git_ref} HEAD failed: {}",
            stderr.trim()
        )));
    }

    let merge_base = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if merge_base.is_empty() {
        return Err(Error::git_command_failed(format!(
            "git merge-base {git_ref} HEAD returned no commit"
        )));
    }

    Ok(merge_base)
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

/// Discard worktree changes for repo-relative files.
///
/// Tracked files are restored from `HEAD`; untracked files created by the
/// operation are removed. Callers should only pass files they just dirtied.
pub fn discard_worktree_changes(path: &str, files: &[String]) -> Result<()> {
    for file in files {
        validate_repo_relative_path(file)?;

        if is_tracked_file(path, file)? {
            let output = execute_git(path, &["checkout", "--", file])
                .map_err(|e| Error::git_command_failed(e.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::git_command_failed(format!(
                    "git checkout -- {} failed: {}",
                    file,
                    stderr.trim()
                )));
            }
        } else {
            let target = Path::new(path).join(file);
            if target.is_dir() {
                std::fs::remove_dir_all(&target).map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(format!(
                            "Failed to remove untracked directory {}",
                            target.display()
                        )),
                    )
                })?;
            } else if target.exists() {
                std::fs::remove_file(&target).map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(format!(
                            "Failed to remove untracked file {}",
                            target.display()
                        )),
                    )
                })?;
            }
        }
    }

    Ok(())
}

fn is_tracked_file(path: &str, file: &str) -> Result<bool> {
    let output = execute_git(path, &["ls-files", "--error-unmatch", "--", file])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(output.status.success())
}

fn validate_repo_relative_path(file: &str) -> Result<()> {
    let path = Path::new(file);
    let safe = !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));

    if safe {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "file",
            format!("Expected repo-relative path, got {}", file),
            None,
            None,
        ))
    }
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
        let _result = get_uncommitted_changes(path);
    }

    /// Regression: get_uncommitted_changes runs `git update-index --refresh`
    /// before reading status so stale stat info from upstream tooling
    /// (build steps, git pull, extension setup) doesn't surface as
    /// false-positive "modified" entries. After a touch that doesn't
    /// change content, status must report a clean tree.
    #[test]
    fn get_uncommitted_changes_treats_stat_only_touches_as_clean() {
        use std::fs;
        use std::process::Command;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_str().unwrap();

        // Init repo, commit a file, then touch it without changing content.
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .output();
        if init.is_err() || !init.unwrap().status.success() {
            // Test environment lacks git — skip rather than fail.
            return;
        }
        let _ = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(path)
            .output();

        let file = dir.path().join("hello.txt");
        fs::write(&file, "content\n").expect("write");
        let _ = Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(path)
            .output();

        // Rewrite the file with identical content via `touch`-then-rewrite
        // to bump mtime/size cache state without changing the bytes. On
        // platforms with `touch`, this is the simplest way to simulate the
        // stale-index condition that `git pull` + prior tooling can leave
        // behind in CI. Without the index refresh, `git status --porcelain`
        // would report it as modified.
        let touch = Command::new("touch")
            .args(["-c", "-m", "-t", "203012010101"])
            .arg(&file)
            .output();
        if touch.is_err() || !touch.unwrap().status.success() {
            // No `touch` available — skip rather than fail. The Linux/macOS
            // CI runners we care about all have it.
            return;
        }

        let result = get_uncommitted_changes(path).expect("status read");
        assert!(
            !result.has_changes,
            "stat-only touch must not surface as a real change after index refresh, got: {:?}",
            result
        );
    }

    #[test]
    fn get_files_changed_since_includes_dirty_and_untracked_files() {
        use std::fs;
        use std::process::Command;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_str().unwrap();

        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .output();
        if init.is_err() || !init.unwrap().status.success() {
            return;
        }
        let _ = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(path)
            .output();

        fs::write(dir.path().join("tracked.txt"), "initial\n").expect("write tracked");
        fs::write(dir.path().join("staged.txt"), "initial\n").expect("write staged");
        fs::write(dir.path().join("deleted.txt"), "initial\n").expect("write deleted");
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(path)
            .output();

        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("modify tracked");
        fs::write(dir.path().join("staged.txt"), "staged dirty\n").expect("modify staged");
        fs::write(dir.path().join("untracked.txt"), "new\n").expect("write untracked");
        fs::remove_file(dir.path().join("deleted.txt")).expect("delete tracked");
        let _ = Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(path)
            .output();

        let files = get_files_changed_since(path, "HEAD").expect("changed files");

        assert!(
            files.contains(&"tracked.txt".to_string()),
            "unstaged tracked file included: {files:?}"
        );
        assert!(
            files.contains(&"staged.txt".to_string()),
            "staged tracked file included: {files:?}"
        );
        assert!(
            files.contains(&"untracked.txt".to_string()),
            "untracked file included: {files:?}"
        );
        assert!(
            !files.contains(&"deleted.txt".to_string()),
            "deleted file excluded: {files:?}"
        );
    }

    #[test]
    fn test_get_uncommitted_changes_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_uncommitted_changes(path);
    }

    #[test]
    fn test_get_files_changed_since_default_path() {
        let path = "";
        let git_ref = "";
        let _result = get_files_changed_since(path, git_ref);
    }

    #[test]
    fn test_get_files_changed_since_default_path_2() {
        let path = "";
        let git_ref = "";
        let _result = get_files_changed_since(path, git_ref);
    }

    #[test]
    fn test_get_files_changed_since_has_expected_effects() {
        // Expected effects: logging
        let path = "";
        let git_ref = "";
        let _ = get_files_changed_since(path, git_ref);
    }

    #[test]
    fn test_get_dirty_files_default_path() {
        let path = "";
        let _result = get_dirty_files(path);
    }

    #[test]
    fn test_get_dirty_files_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_dirty_files(path);
    }

    #[test]
    fn test_get_diff_default_path() {
        let path = "";
        let _result = get_diff(path);
    }

    #[test]
    fn test_get_diff_default_path_2() {
        let path = "";
        let _result = get_diff(path);
    }

    #[test]
    fn test_get_diff_has_expected_effects() {
        // Expected effects: mutation
        let path = "";
        let _ = get_diff(path);
    }

    #[test]
    fn test_get_range_diff_default_path() {
        let path = "";
        let baseline_ref = "";
        let _result = get_range_diff(path, baseline_ref);
    }
}
