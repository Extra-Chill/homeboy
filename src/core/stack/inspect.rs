//! Read-only stack inspection: list commits in the current branch that are
//! ahead of an upstream tracking branch, with each commit decorated by its
//! GitHub PR status (when discoverable via `gh`).
//!
//! "Stack" here is the narrow read of the term used in the project's
//! combined-fixes workflow: a single branch that carries N upstream PRs as
//! cherry-picks on top of a base ref.
//!
//! `homeboy stack inspect` is the spec-less introspection layer: "what am I
//! currently carrying on this checkout?". When a stack spec exists for the
//! same combined-fixes workflow, prefer `homeboy stack status <id>` — it
//! also walks the commits but cross-references against the declared PR list.
//!
//! Performance: PR lookup uses one `gh pr list --search` invocation per
//! commit. Stacks are typically <20 commits so this is fine; scaling past
//! that would warrant a single `gh api` GraphQL query.

use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::error::{Error, Result};
use crate::git::resolve_target;

/// Per-commit detail row for the inspect output.
#[derive(Debug, Clone, Serialize)]
pub struct InspectCommitDetails {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

/// Output of [`inspect`].
#[derive(Debug, Clone, Serialize)]
pub struct InspectOutput {
    pub component_id: String,
    pub path: String,
    /// Branch name we inspected (current `HEAD`).
    pub branch: String,
    /// Upstream / base ref we compared against.
    pub base: String,
    /// Whether the base ref was auto-detected from `@{upstream}` (true) or
    /// passed explicitly via `--base` (false).
    pub base_auto_detected: bool,
    /// Commits in `base..HEAD`, ordered oldest-first (rebase order).
    pub commits: Vec<InspectCommit>,
    /// Count of commits whose detected PR is `MERGED`.
    pub merged_count: usize,
    /// `success` is `true` when inspection completed end-to-end. PR lookups
    /// failing for individual commits don't fail the whole command — they
    /// just leave the `pr` field unset on those commits.
    pub success: bool,
}

/// One commit in the inspected stack, with optional PR decoration.
#[derive(Debug, Clone, Serialize)]
pub struct InspectCommit {
    #[serde(flatten)]
    pub commit: InspectCommitDetails,
    /// Associated GitHub PR, if exactly one was found via `gh pr list --search`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<InspectPr>,
    /// Diagnostic when PR lookup didn't yield exactly one match. Distinct
    /// from "no PR" (the search itself succeeded with zero hits) so tooling
    /// can tell "we couldn't ask" from "the answer is no".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_lookup_note: Option<String>,
}

/// PR decoration on an inspected commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectPr {
    pub number: u64,
    /// `OPEN` / `CLOSED` / `MERGED`, as `gh` reports them.
    pub state: String,
    pub title: String,
    pub url: String,
}

/// Options for [`inspect`].
#[derive(Debug, Clone, Default)]
pub struct InspectOptions {
    /// Override the upstream ref. `None` uses the current branch's
    /// `@{upstream}`. Passing this skips the auto-detection.
    pub base: Option<String>,
    /// Skip the GitHub PR lookup pass entirely.
    pub no_pr: bool,
    /// Explicit GitHub repo (`owner/name`) to scope PR lookups to.
    pub repo: Option<String>,
}

/// Inspect the current branch as a stack of commits over an upstream ref.
pub fn inspect(component_id: Option<&str>, options: InspectOptions) -> Result<InspectOutput> {
    inspect_at(component_id, options, None)
}

/// Like [`inspect`] but with an explicit path override.
pub fn inspect_at(
    component_id: Option<&str>,
    options: InspectOptions,
    path_override: Option<&str>,
) -> Result<InspectOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    let branch = current_branch(&path)?;
    let (base, base_auto_detected) = resolve_base(&path, options.base.as_deref())?;

    let commits = list_commits_over_base(&path, &base)?;

    let mut decorated: Vec<InspectCommit> = Vec::with_capacity(commits.len());
    let mut merged_count = 0usize;

    for commit in commits {
        let mut entry = InspectCommit {
            commit,
            pr: None,
            pr_lookup_note: None,
        };

        if !options.no_pr {
            match find_pr_for_commit(&path, &entry.commit.sha, options.repo.as_deref()) {
                Ok(PrLookup::Single(pr)) => {
                    if pr.state == "MERGED" {
                        merged_count += 1;
                    }
                    entry.pr = Some(pr);
                }
                Ok(PrLookup::None) => {} // No PR — silent. Common case.
                Ok(PrLookup::Multiple(n)) => {
                    entry.pr_lookup_note =
                        Some(format!("{} PRs match this commit; decoration skipped", n));
                }
                Err(e) => {
                    entry.pr_lookup_note = Some(format!("PR lookup failed: {}", e));
                }
            }
        }

        decorated.push(entry);
    }

    Ok(InspectOutput {
        component_id: id,
        path,
        branch,
        base,
        base_auto_detected,
        commits: decorated,
        merged_count,
        success: true,
    })
}

/// Resolve the base ref. Returns `(base, auto_detected)`.
fn resolve_base(path: &str, override_base: Option<&str>) -> Result<(String, bool)> {
    if let Some(base) = override_base {
        return Ok((base.to_string(), false));
    }

    // Try @{upstream} of HEAD. If unset, return a clear error pointing the
    // user at --base.
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::git_command_failed(format!("git rev-parse @{{upstream}}: {}", e)))?;

    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "base",
            "Current branch has no tracked upstream and --base was not provided",
            None,
            Some(vec![
                "Pass an explicit base: homeboy stack inspect --base <ref>".to_string(),
                "Or set tracking: git branch --set-upstream-to=<remote>/<branch>".to_string(),
            ]),
        ));
    }

    let upstream = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if upstream.is_empty() {
        return Err(Error::validation_invalid_argument(
            "base",
            "Resolved upstream was empty",
            None,
            Some(vec![
                "Pass an explicit base: homeboy stack inspect --base <ref>".to_string(),
            ]),
        ));
    }
    Ok((upstream, true))
}

/// Get the current branch name (or `HEAD` for detached HEAD).
fn current_branch(path: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .map_err(|e| Error::git_command_failed(format!("git rev-parse HEAD: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git rev-parse --abbrev-ref HEAD failed: {}",
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// List commits in `base..HEAD`, oldest-first.
fn list_commits_over_base(path: &str, base: &str) -> Result<Vec<InspectCommitDetails>> {
    let range = format!("{}..HEAD", base);
    // Reverse so output is oldest-first (rebase order). Tab-separated
    // columns: full SHA, subject, author name, ISO date.
    let output = Command::new("git")
        .args([
            "log",
            "--reverse",
            "--format=%H%x09%s%x09%an%x09%aI",
            &range,
        ])
        .current_dir(path)
        .output()
        .map_err(|e| Error::git_command_failed(format!("git log {}: {}", range, e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Distinguish "base doesn't exist" from generic failure.
        let trimmed = stderr.trim();
        if trimmed.contains("unknown revision") || trimmed.contains("bad revision") {
            return Err(Error::validation_invalid_argument(
                "base",
                format!("Base ref '{}' not found in this repo", base),
                None,
                Some(vec![
                    format!("Verify the ref exists: git rev-parse {}", base),
                    "If using @{upstream}, set tracking: git branch --set-upstream-to=<remote>/<branch>".to_string(),
                ]),
            ));
        }
        return Err(Error::git_command_failed(format!(
            "git log {} failed: {}",
            range, trimmed
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let sha = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        let author = parts.next().unwrap_or("").to_string();
        let date = parts.next().unwrap_or("").to_string();
        if sha.is_empty() {
            continue;
        }
        let short_sha = if sha.len() >= 7 {
            sha[..7].to_string()
        } else {
            sha.clone()
        };
        commits.push(InspectCommitDetails {
            sha,
            short_sha,
            subject,
            author,
            date,
        });
    }

    Ok(commits)
}

/// Result of looking up a PR for a single commit SHA via `gh`.
enum PrLookup {
    Single(InspectPr),
    None,
    Multiple(usize),
}

/// Find the PR (if any) associated with a commit SHA via
/// `gh pr list --search <sha>`.
///
/// GitHub's PR search indexes commit SHAs in the search field, so a
/// free-text search for the SHA reliably returns the PR(s) containing
/// that commit.
fn find_pr_for_commit(path: &str, sha: &str, repo: Option<&str>) -> Result<PrLookup> {
    let mut args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "--search".into(),
        sha.to_string(),
        "--state".into(),
        "all".into(),
        "--json".into(),
        "number,state,title,url".into(),
        "--limit".into(),
        "5".into(),
    ];
    if let Some(repo) = repo {
        args.push("--repo".into());
        args.push(repo.to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = Command::new("gh")
        .args(&arg_refs)
        .current_dir(path)
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr list --search: {} (is `gh` installed and authenticated?)",
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr list --search {}: {}",
            sha,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<InspectPr> = serde_json::from_str(&stdout).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse `gh pr list --search {}`", sha)),
            Some(stdout.chars().take(200).collect()),
        )
    })?;

    match parsed.len() {
        0 => Ok(PrLookup::None),
        1 => Ok(PrLookup::Single(parsed.into_iter().next().unwrap())),
        n => Ok(PrLookup::Multiple(n)),
    }
}

#[cfg(test)]
#[path = "../../../tests/core/stack/inspect_test.rs"]
mod inspect_test;
