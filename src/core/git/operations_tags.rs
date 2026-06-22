//! Tag and ref query git operations: creating/deleting tags, resolving tag and
//! branch commits, ancestry checks, and remote fetch helpers.
//!
//! Split out of `operations.rs` to keep that module focused on
//! status/pull/rebase/cherry-pick flows.

use std::path::Path;

use crate::core::error::{Error, Result};

use super::operation_output::GitOutput;
use super::operations::execute_git_for_release;
use super::primitives_query::short_head_revision;
use super::{execute_git, resolve_target};

pub fn tag(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
) -> Result<GitOutput> {
    tag_at(component_id, tag_name, message, None)
}

/// Like [`tag`] but with an explicit path override for git operations.
pub fn tag_at(
    component_id: Option<&str>,
    tag_name: Option<&str>,
    message: Option<&str>,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let name = tag_name.ok_or_else(|| {
        Error::validation_invalid_argument("tagName", "Missing tag name", None, None)
    })?;
    let args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", name, "-m", msg],
        None => vec!["tag", name],
    };
    super::run_resolved_git(component_id, path_override, "tag", &args)
}

/// Check if a tag exists on the remote.
pub fn tag_exists_on_remote(path: &str, tag_name: &str) -> Result<bool> {
    Ok(remote_tag_commit(path, tag_name)?.is_some())
}

/// Get the commit SHA a remote tag points to, if it exists.
pub fn remote_tag_commit(path: &str, tag_name: &str) -> Result<Option<String>> {
    let peeled_ref = format!("refs/tags/{}^{{}}", tag_name);
    if let Some(commit) = remote_ref_commit(path, &peeled_ref) {
        return Ok(Some(commit));
    }

    Ok(remote_ref_commit(path, &format!("refs/tags/{}", tag_name)))
}

fn remote_ref_commit(path: &str, ref_name: &str) -> Option<String> {
    crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["ls-remote", "--tags", "origin", ref_name],
    )
    .and_then(|output| {
        output
            .lines()
            .find_map(|line| line.split_whitespace().next().map(str::to_string))
    })
}

/// Check if a tag exists locally.
pub fn tag_exists_locally(path: &str, tag_name: &str) -> Result<bool> {
    Ok(
        crate::core::engine::command::run_in_optional(path, "git", &["tag", "-l", tag_name])
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    )
}

/// Get the commit SHA a tag points to.
pub fn get_tag_commit(path: &str, tag_name: &str) -> Result<String> {
    crate::core::engine::command::run_in(
        path,
        "git",
        &["rev-list", "-n", "1", tag_name],
        &format!("get commit for tag '{}'", tag_name),
    )
}

/// Get the current HEAD commit SHA.
pub fn get_head_commit(path: &str) -> Result<String> {
    crate::core::engine::command::run_in(path, "git", &["rev-parse", "HEAD"], "get HEAD commit")
}

/// Get the commit SHA the `origin` remote currently has for a branch, without
/// fetching. Returns `Ok(None)` when the branch does not exist on the remote.
///
/// Used to detect that the remote branch advanced after a release commit/tag
/// were created — the partial-release state in issue #3611, where the tag push
/// succeeded but the branch push was rejected as non-fast-forward.
pub fn remote_branch_commit(path: &str, branch: &str) -> Result<Option<String>> {
    let ref_name = format!("refs/heads/{}", branch);
    Ok(crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["ls-remote", "--heads", "origin", &ref_name],
    )
    .and_then(|output| {
        output
            .lines()
            .find_map(|line| line.split_whitespace().next().map(str::to_string))
    }))
}

/// Fetch `origin` so remote-tracking refs reflect the current remote state.
///
/// A thin wrapper used by release recovery before it inspects how far the local
/// branch has diverged from the advanced remote (issue #3611).
pub fn fetch_origin(path: &str) -> Result<()> {
    crate::core::engine::command::run_in(path, "git", &["fetch", "origin"], "git fetch origin")?;
    Ok(())
}

/// Return true when `ancestor` is an ancestor of (i.e. reachable from)
/// `descendant`. Used to confirm a stale tag's commit is strictly behind HEAD
/// before moving it, so a tag is never relocated onto an unrelated/divergent
/// history.
pub fn is_ancestor(path: &str, ancestor: &str, descendant: &str) -> Result<bool> {
    let output =
        execute_git_for_release(path, &["merge-base", "--is-ancestor", ancestor, descendant])
            .map_err(|e| {
                Error::internal_io(
                    format!("Failed to check ancestry: {}", e),
                    Some(format!(
                        "git merge-base --is-ancestor {} {}",
                        ancestor, descendant
                    )),
                )
            })?;
    // `--is-ancestor` exits 0 when true, 1 when false. Any other code is an error.
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        other => Err(Error::git_command_failed(format!(
            "git merge-base --is-ancestor exited with {:?}: {}",
            other,
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

/// Delete a local tag. No-op-safe: returns the git output for inspection.
pub fn delete_local_tag(path: &str, tag_name: &str) -> Result<GitOutput> {
    let (id, resolved) = resolve_target(None, Some(path))?;
    let output = execute_git(&resolved, &["tag", "-d", tag_name])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, resolved, "tag.delete", output))
}

/// Delete a tag on the `origin` remote.
pub fn delete_remote_tag(path: &str, tag_name: &str) -> Result<GitOutput> {
    let (id, resolved) = resolve_target(None, Some(path))?;
    let refspec = format!(":refs/tags/{}", tag_name);
    let output = execute_git(&resolved, &["push", "origin", &refspec])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(
        id,
        resolved,
        "tag.delete_remote",
        output,
    ))
}

/// Get the current HEAD short commit SHA, returning `None` outside git checkouts.
pub fn short_head_revision_at(path: &Path) -> Option<String> {
    short_head_revision(path)
}

#[cfg(test)]
mod is_ancestor_tests {
    use super::*;
    use std::process::Command;

    fn run_in(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn rev(dir: &std::path::Path, refname: &str) -> String {
        let output = Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(dir)
            .output()
            .expect("spawn git");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A linear two-commit history: the first commit must be an ancestor of the
    /// second, but not vice versa. This is the exact safety check that gates a
    /// stale-tag retag (the tagged commit must be strictly behind HEAD).
    #[test]
    fn test_is_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_in(dir, &["git", "init", "-q"]);
        run_in(dir, &["git", "config", "user.email", "t@example.com"]);
        run_in(dir, &["git", "config", "user.name", "T"]);
        run_in(dir, &["git", "config", "commit.gpgsign", "false"]);

        std::fs::write(dir.join("f.txt"), "one").unwrap();
        run_in(dir, &["git", "add", "."]);
        run_in(dir, &["git", "commit", "-q", "-m", "first"]);
        let first = rev(dir, "HEAD");

        std::fs::write(dir.join("f.txt"), "two").unwrap();
        run_in(dir, &["git", "add", "."]);
        run_in(dir, &["git", "commit", "-q", "-m", "second"]);
        let second = rev(dir, "HEAD");

        let path = dir.to_str().unwrap();

        // first is behind second -> ancestor
        assert!(is_ancestor(path, &first, &second).unwrap());
        // second is ahead -> NOT an ancestor of first
        assert!(!is_ancestor(path, &second, &first).unwrap());
        // a commit is its own ancestor (git treats reflexive as true)
        assert!(is_ancestor(path, &second, &second).unwrap());
    }

    /// Set up a bare remote + a clone with one pushed commit on `main`.
    /// Returns (remote_dir, clone_dir, pushed_commit_sha).
    fn remote_and_clone() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let remote = tempfile::tempdir().expect("remote tempdir");
        let clone = tempfile::tempdir().expect("clone tempdir");
        run_in(
            remote.path(),
            &["git", "init", "--bare", "-b", "main", "-q"],
        );
        run_in(
            clone.path(),
            &["git", "clone", "-q", remote.path().to_str().unwrap(), "."],
        );
        run_in(clone.path(), &["git", "config", "user.email", "t@x.test"]);
        run_in(clone.path(), &["git", "config", "user.name", "T"]);
        run_in(clone.path(), &["git", "config", "commit.gpgsign", "false"]);
        std::fs::write(clone.path().join("f.txt"), "one").unwrap();
        run_in(clone.path(), &["git", "add", "."]);
        run_in(clone.path(), &["git", "commit", "-q", "-m", "first"]);
        run_in(clone.path(), &["git", "push", "-q", "origin", "main"]);
        let sha = rev(clone.path(), "HEAD");
        (remote, clone, sha)
    }

    #[test]
    fn test_remote_branch_commit() {
        let (_remote, clone, pushed) = remote_and_clone();
        let path = clone.path().to_str().unwrap();

        // Existing branch resolves to the pushed commit.
        assert_eq!(
            remote_branch_commit(path, "main").unwrap().as_deref(),
            Some(pushed.as_str())
        );
        // A branch that does not exist on the remote returns None.
        assert_eq!(remote_branch_commit(path, "does-not-exist").unwrap(), None);
    }

    #[test]
    fn test_fetch_origin() {
        let (_remote, clone, _pushed) = remote_and_clone();
        // A plain fetch against a healthy remote must succeed.
        fetch_origin(clone.path().to_str().unwrap()).expect("fetch origin should succeed");
    }
}
