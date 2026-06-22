use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::core::config::read_json_spec_to_string;
use crate::core::error::{Error, Result};
use crate::core::output::BulkResult;

use super::operation_output::{run_bulk_ids, GitOutput};
use super::primitives::{is_git_repo, run_git};
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

    let clean = run_git(
        Path::new(path),
        &["status", "--porcelain=v1"],
        "git status --porcelain",
    )
    .map(|output| output.is_empty())
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
    super::run_resolved_git(
        component_id,
        path_override,
        "status",
        &["status", "--porcelain=v1"],
    )
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
    super::run_resolved_git(component_id, path_override, "pull", &["pull"])
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
