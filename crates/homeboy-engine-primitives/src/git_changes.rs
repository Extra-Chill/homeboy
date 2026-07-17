//! Changed-since git file scoping.
//!
//! Given a repository path and a base git ref, return the set of files that
//! changed on the current branch relative to that ref — including uncommitted
//! staged, unstaged, and untracked files — so lint/test/audit scopes can target
//! only what a change actually touched. Handles shallow CI clones by
//! progressively deepening until the merge base is reachable.
//!
//! This is a std-only git primitive (raw `git` subprocess + `homeboy_error`)
//! shared by the audit engine, the refactor planner, and the extension
//! lint/test scopers. It lives in `homeboy-engine-primitives` so those consumers
//! — including a future `homeboy-code-audit` crate — depend on the slim
//! primitives base rather than all of `homeboy-core` for changed-file scoping.

use std::collections::BTreeSet;
use std::process::{Command, Output};

use homeboy_error::{Error, Result};

/// Run a git subcommand in `path`, returning the raw process output.
fn execute_git(path: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").args(args).current_dir(path).output()
}

/// Get the files that changed on the current branch relative to `git_ref`.
///
/// Combines the committed merge-base diff (`<ref>...HEAD`) with staged,
/// unstaged, and untracked working-tree files, filtered to added/copied/
/// modified/renamed paths (deletions are excluded so scopes only reference
/// existing files). Handles shallow clones by deepening to reach the merge base.
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
        let mut files: BTreeSet<String> = parse_diff_output(&output.stdout).into_iter().collect();
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
        files.extend(parse_diff_output(&output.stdout));
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

/// Resolve the default remote of a repository: prefer `origin`, else a sole
/// remote, else fall back to `origin`.
fn resolve_default_remote(path: &str) -> String {
    let remotes: Vec<String> = execute_git(path, &["remote"])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if remotes.iter().any(|remote| remote == "origin") {
        return "origin".to_string();
    }
    if let [only] = remotes.as_slice() {
        return only.clone();
    }
    "origin".to_string()
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
///
/// Exposed so callers that resolve the merge base themselves (e.g. core's
/// `resolve_merge_base`) can reuse the same shallow-clone deepening strategy.
pub fn ensure_ancestry_for_ref(path: &str, git_ref: &str) -> Result<()> {
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
    let remote = resolve_default_remote(path);
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

/// Parse newline-delimited `git diff --name-only` output into a file list.
fn parse_diff_output(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
