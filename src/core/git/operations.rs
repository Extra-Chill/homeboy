use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::core::config::read_json_spec_to_string;
use crate::core::error::{Error, Result};
use crate::core::output::BulkResult;

use super::operation_output::{run_bulk_ids, GitOutput};
use super::primitives::is_git_repo;
use super::{execute_git, resolve_target};

#[derive(Debug, Clone, Serialize)]
pub struct RepoSnapshot {
    pub branch: String,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
}

// Input types for JSON parsing
#[derive(Debug, Deserialize)]

pub(crate) struct BulkIdsInput {
    pub(crate) component_ids: Vec<String>,
}

pub fn execute_git_for_release(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    execute_git(path, args)
}

pub fn get_repo_snapshot(path: &str) -> Result<RepoSnapshot> {
    if !is_git_repo(path) {
        return Err(Error::git_command_failed("Not a git repository"));
    }

    let branch = crate::core::engine::command::run_in(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "git branch",
    )?;

    // Use direct Command to properly handle empty output (clean repo).
    // run_in_optional returns None for empty stdout, which would incorrectly
    // indicate a dirty repo when used with .unwrap_or(false).
    let clean = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false);

    let (ahead, behind) = crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
    )
    .and_then(|_| {
        crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )
    })
    .map(|counts| parse_ahead_behind(&counts))
    .unwrap_or((None, None));

    Ok(RepoSnapshot {
        branch,
        clean,
        ahead,
        behind,
    })
}

fn parse_ahead_behind(counts: &str) -> (Option<u32>, Option<u32>) {
    // git rev-list --left-right --count @{upstream}...HEAD outputs:
    //   <upstream_only>\t<local_only>
    // upstream_only = commits on remote not in local (behind)
    // local_only = commits in local not on remote (ahead)
    let trimmed = counts.trim();
    let mut parts = trimmed.split_whitespace();
    let behind = parts.next().and_then(|v| v.parse::<u32>().ok());
    let ahead = parts.next().and_then(|v| v.parse::<u32>().ok());
    (ahead, behind)
}

/// Get git status for a component.
pub fn status(component_id: Option<&str>) -> Result<GitOutput> {
    status_at(component_id, None)
}

/// Like [`status`] but with an explicit path override for git operations.
pub fn status_at(component_id: Option<&str>, path_override: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "status", output))
}

/// Get git status for multiple components from JSON spec.
pub fn status_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk status input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    Ok(run_bulk_ids(&input.component_ids, "status", |id| {
        status(Some(id))
    }))
}

/// Pull remote changes for a component.
pub fn pull(component_id: Option<&str>) -> Result<GitOutput> {
    pull_at(component_id, None)
}

/// Like [`pull`] but with an explicit path override for git operations.
pub fn pull_at(component_id: Option<&str>, path_override: Option<&str>) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let output =
        execute_git(&path, &["pull"]).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "pull", output))
}

/// Options for [`rebase`].
#[derive(Debug, Clone, Default)]
pub struct RebaseOptions {
    /// Upstream / target ref to rebase onto. `None` defaults to the
    /// current branch's tracked upstream (`@{upstream}`), matching
    /// `git pull --rebase` semantics.
    pub onto: Option<String>,
    /// `git rebase --continue` after manual conflict resolution. Mutually
    /// exclusive with `abort` at the CLI layer.
    pub continue_: bool,
    /// `git rebase --abort` to bail out of an in-progress rebase.
    pub abort: bool,
}

/// Rebase the current branch onto another ref.
///
/// Default behaviour (no `onto`) is `git rebase @{upstream}`, which drops
/// commits whose patch-id matches a commit already in upstream — the
/// standard rebase merged-commit dedup. Squash-merged PRs (different
/// patch-id) are NOT dropped by default; that case will land in a
/// follow-up via `gh`-aware PR drop.
///
/// On conflict, the operation returns a `GitOutput { success: false }`
/// with stderr from git. The caller resolves with raw `git`, then runs
/// `homeboy git rebase --continue` or `--abort`. No state-machine
/// orchestration in MVP.
pub fn rebase(component_id: Option<&str>, options: RebaseOptions) -> Result<GitOutput> {
    rebase_at(component_id, options, None)
}

/// Like [`rebase`] but with an explicit path override.
pub fn rebase_at(
    component_id: Option<&str>,
    options: RebaseOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    let args: Vec<String> = if options.abort {
        vec!["rebase".into(), "--abort".into()]
    } else if options.continue_ {
        vec!["rebase".into(), "--continue".into()]
    } else {
        let mut a = vec!["rebase".into()];
        if let Some(onto) = options.onto.as_deref() {
            a.push(onto.to_string());
        }
        // No `onto` arg → bare `git rebase` rebases onto @{upstream}.
        a
    };
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "rebase", output))
}

/// Options for [`cherry_pick`].
#[derive(Debug, Clone, Default)]
pub struct CherryPickOptions {
    /// Commit refs to cherry-pick. Accepts SHAs, branches, ranges
    /// (`<sha1>..<sha2>`). Empty when `continue_` or `abort` is set.
    pub refs: Vec<String>,
    /// Cherry-pick all commits from a GitHub PR (one or more). Resolved
    /// via `gh pr view <n> --json commits`. Each PR's commits are picked
    /// in oldest-to-newest order. Combinable with `refs`; PR commits are
    /// expanded first and then concatenated with explicit refs.
    pub prs: Vec<u64>,
    /// `git cherry-pick --continue` after manual conflict resolution.
    pub continue_: bool,
    /// `git cherry-pick --abort` to bail out of an in-progress pick.
    pub abort: bool,
}

/// Cherry-pick one or more commits onto the current branch.
///
/// On conflict, returns `GitOutput { success: false }` with git's stderr.
/// Resolve manually, then run with `--continue` or `--abort`.
pub fn cherry_pick(component_id: Option<&str>, options: CherryPickOptions) -> Result<GitOutput> {
    cherry_pick_at(component_id, options, None)
}

/// Like [`cherry_pick`] but with an explicit path override.
pub fn cherry_pick_at(
    component_id: Option<&str>,
    options: CherryPickOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    if options.abort {
        let output = execute_git(&path, &["cherry-pick", "--abort"])
            .map_err(|e| Error::git_command_failed(e.to_string()))?;
        return Ok(GitOutput::from_output(id, path, "cherry-pick", output));
    }
    if options.continue_ {
        let output = execute_git(&path, &["cherry-pick", "--continue"])
            .map_err(|e| Error::git_command_failed(e.to_string()))?;
        return Ok(GitOutput::from_output(id, path, "cherry-pick", output));
    }

    // Expand any PR numbers into commit SHAs via `gh`. PR commits come
    // before explicit refs in argv order so the user's positional args
    // can fine-tune ordering by interleaving — but in practice most
    // callers pass either `--pr` or `<refs>`, not both.
    let mut refs: Vec<String> = Vec::new();
    for pr in &options.prs {
        let pr_commits = resolve_pr_commits(&path, *pr)?;
        refs.extend(pr_commits);
    }
    refs.extend(options.refs.iter().cloned());

    if refs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "refs",
            "cherry-pick requires at least one commit ref or --pr <number>",
            None,
            Some(vec![
                "Provide a commit ref: homeboy git cherry-pick <sha>".to_string(),
                "Or pick a PR: homeboy git cherry-pick --pr <number>".to_string(),
            ]),
        ));
    }

    let mut args: Vec<String> = vec!["cherry-pick".into()];
    args.extend(refs);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "cherry-pick", output))
}

/// Resolve a GitHub PR number to its list of commit SHAs (oldest first)
/// using `gh pr view`. Used by [`cherry_pick`] to expand `--pr <n>`.
fn resolve_pr_commits(path: &str, pr: u64) -> Result<Vec<String>> {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", &pr.to_string(), "--json", "commits"])
        .current_dir(path)
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr view {}: {} (is `gh` installed and authenticated?)",
                pr, e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr view {} failed: {}",
            pr,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse `gh pr view {} --json commits`", pr)),
            Some(stdout.chars().take(200).collect()),
        )
    })?;

    let commits = parsed
        .get("commits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "gh pr view {} returned JSON without a `commits` array",
                pr
            ))
        })?;

    let mut shas = Vec::with_capacity(commits.len());
    for commit in commits {
        let oid = commit.get("oid").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::git_command_failed(format!("gh pr view {} returned a commit without `oid`", pr))
        })?;
        shas.push(oid.to_string());
    }
    Ok(shas)
}

/// Pull multiple components from JSON spec.
pub fn pull_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: BulkIdsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk pull input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    Ok(run_bulk_ids(&input.component_ids, "pull", |id| {
        pull(Some(id))
    }))
}

/// Create a git tag for a component.
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
    let (id, path) = resolve_target(component_id, path_override)?;
    let args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", name, "-m", msg],
        None => vec!["tag", name],
    };
    let output = execute_git(&path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "tag", output))
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
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!revision.is_empty()).then_some(revision)
}

/// Fetch from remote and return count of commits behind upstream.
/// Returns Ok(Some(n)) if behind by n commits, Ok(None) if not behind or no upstream.
pub fn fetch_and_get_behind_count(path: &str) -> Result<Option<u32>> {
    // Run git fetch (update tracking refs)
    crate::core::engine::command::run_in(path, "git", &["fetch"], "git fetch")?;

    // Check if upstream exists
    let upstream = crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
    );
    if upstream.is_none() {
        return Ok(None); // No upstream configured
    }

    // Get ahead/behind counts
    let counts = crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    );

    match counts {
        Some(output) => {
            let (_, behind) = parse_ahead_behind(&output);
            Ok(behind.filter(|&n| n > 0))
        }
        None => Ok(None),
    }
}

/// Fetch from remote and fast-forward if behind.
///
/// Returns Ok(Some(n)) with the number of commits fast-forwarded, or Ok(None) if
/// already up-to-date. Errors if the fast-forward fails (diverged histories).
pub fn fetch_and_fast_forward(path: &str) -> Result<Option<u32>> {
    let behind = fetch_and_get_behind_count(path)?;

    match behind {
        None => Ok(None),
        Some(n) => {
            // Attempt fast-forward pull
            let output = execute_git(path, &["pull", "--ff-only"])
                .map_err(|e| Error::git_command_failed(e.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::validation_invalid_argument(
                    "remote_sync",
                    format!(
                        "Branch has diverged from remote — fast-forward failed: {}",
                        stderr.trim()
                    ),
                    None,
                    Some(vec![
                        "Resolve the divergence manually before releasing".to_string(),
                        "Run: git pull --rebase".to_string(),
                    ]),
                ));
            }

            Ok(Some(n))
        }
    }
}

#[cfg(test)]
mod is_ancestor_tests {
    use super::*;

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
