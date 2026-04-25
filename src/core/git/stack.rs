//! Read-only stack inspection: list commits in the current branch that are
//! ahead of an upstream tracking branch, with each commit decorated by its
//! GitHub PR status (when discoverable via `gh`).
//!
//! "Stack" here is the narrow read of the term used in the project's
//! combined-fixes workflow: a single branch that carries N upstream PRs as
//! cherry-picks on top of a base ref. It is NOT the broader Graphite-style
//! "stack of my own PRs" abstraction — see the discussion in
//! `MEMORY.md > Homeboy stack primitive` for why that whole abstraction is
//! intentionally not built.
//!
//! `homeboy git stack` is the introspection layer on top of the rewriting
//! verbs (`rebase` / `cherry-pick` / `push --force-with-lease`): it answers
//! the question "what am I currently carrying?" so the user knows whether
//! to drop merged PRs before the next rebase ritual.
//!
//! Performance: PR lookup uses one `gh search prs` invocation per commit.
//! Stacks are typically <20 commits so this is fine; if it ever needs to
//! scale we can switch to a single `gh api` GraphQL query.

use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::error::{Error, Result};

use super::resolve_target;

/// Per-commit detail row for the stack output. Distinct from
/// [`super::commits::CommitInfo`] because that struct is narrower
/// (hash + subject + category for changelog generation) — stack output
/// needs author + date for human-readable rendering and a short SHA so
/// downstream tooling doesn't have to recompute it.
#[derive(Debug, Clone, Serialize)]
pub struct StackCommitDetails {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

/// Output of [`stack`].
#[derive(Debug, Clone, Serialize)]
pub struct StackOutput {
    pub component_id: String,
    pub path: String,
    /// Branch name we inspected (current `HEAD`).
    pub branch: String,
    /// Upstream / base ref we compared against.
    pub base: String,
    /// Whether the base ref was auto-detected from `@{upstream}` (true) or
    /// passed explicitly via `--base` (false). Useful for human output to
    /// say "vs upstream/trunk" without having to repeat the user's flag.
    pub base_auto_detected: bool,
    /// Commits in `base..HEAD`, ordered oldest-first (rebase order).
    pub commits: Vec<StackCommit>,
    /// Count of commits whose detected PR is `MERGED`. Decoration field —
    /// callers that want a stronger "drop these" signal can read it
    /// directly without walking `commits`.
    pub merged_count: usize,
    /// `success` is `true` when stack inspection completed end-to-end.
    /// PR lookups failing for individual commits don't fail the whole
    /// command — they just leave the `pr` field unset on those commits.
    pub success: bool,
}

/// One commit in the stack, with optional PR decoration.
#[derive(Debug, Clone, Serialize)]
pub struct StackCommit {
    #[serde(flatten)]
    pub commit: StackCommitDetails,
    /// Associated GitHub PR, if exactly one was found via `gh search prs`.
    /// Multiple matches are recorded in `pr_lookup_note` so users know
    /// the decoration is ambiguous rather than missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<StackPr>,
    /// Diagnostic when PR lookup didn't yield exactly one match. Distinct
    /// from "no PR" (the search itself succeeded with zero hits) so
    /// tooling can tell "we couldn't ask" from "the answer is no".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_lookup_note: Option<String>,
}

/// PR decoration on a stack commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackPr {
    pub number: u64,
    /// `OPEN` / `CLOSED` / `MERGED`, as `gh` reports them.
    pub state: String,
    pub title: String,
    pub url: String,
}

/// Options for [`stack`].
#[derive(Debug, Clone, Default)]
pub struct StackOptions {
    /// Override the upstream ref. `None` uses the current branch's
    /// `@{upstream}`. Passing this skips the auto-detection.
    pub base: Option<String>,
    /// Skip the GitHub PR lookup pass entirely. Useful for offline use,
    /// for repos without `gh` configured, or when the caller just wants
    /// the commit list.
    pub no_pr: bool,
    /// Explicit GitHub repo (`owner/name`) to scope PR lookups to.
    /// `None` lets `gh` infer from the local checkout's remote, which
    /// is what users normally want.
    pub repo: Option<String>,
}

/// Inspect the current branch as a stack of commits over an upstream ref.
pub fn stack(component_id: Option<&str>, options: StackOptions) -> Result<StackOutput> {
    stack_at(component_id, options, None)
}

/// Like [`stack`] but with an explicit path override.
pub fn stack_at(
    component_id: Option<&str>,
    options: StackOptions,
    path_override: Option<&str>,
) -> Result<StackOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;

    let branch = current_branch(&path)?;
    let (base, base_auto_detected) = resolve_base(&path, options.base.as_deref())?;

    let commits = list_commits_over_base(&path, &base)?;

    let mut decorated: Vec<StackCommit> = Vec::with_capacity(commits.len());
    let mut merged_count = 0usize;

    for commit in commits {
        let mut entry = StackCommit {
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

    Ok(StackOutput {
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
                "Pass an explicit base: homeboy git stack --base <ref>".to_string(),
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
                "Pass an explicit base: homeboy git stack --base <ref>".to_string(),
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
fn list_commits_over_base(path: &str, base: &str) -> Result<Vec<StackCommitDetails>> {
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
        commits.push(StackCommitDetails {
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
    Single(StackPr),
    None,
    Multiple(usize),
}

/// Find the PR (if any) associated with a commit SHA via
/// `gh pr list --search <sha>`. GitHub's PR search indexes commit SHAs
/// in the search field, so a free-text search for the SHA reliably
/// returns the PR(s) containing that commit.
///
/// Returns `Ok(PrLookup::None)` when no PRs match, `Ok(PrLookup::Multiple(n))`
/// when more than one matches (decoration is intentionally skipped to avoid
/// guessing), and `Err` when `gh` is unreachable or returns invalid JSON.
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
    let parsed: Vec<StackPr> = serde_json::from_str(&stdout).map_err(|e| {
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a fresh git repo with a single committed file. Mirrors the
    /// helper in operations.rs::tests so the stack tests stay
    /// self-contained.
    fn init_repo() -> (TempDir, String) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&path)
            .output()
            .unwrap();
        fs::write(dir.path().join("README.md"), "initial\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();
        (dir, path)
    }

    fn add_commit(dir: &TempDir, path: &str, file: &str, contents: &str, message: &str) {
        fs::write(dir.path().join(file), contents).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(path)
            .output()
            .unwrap();
    }

    #[test]
    fn empty_stack_when_branch_is_at_base() {
        let (_dir, path) = init_repo();

        let out = stack_at(
            None,
            StackOptions {
                base: Some("HEAD".to_string()),
                no_pr: true,
                ..Default::default()
            },
            Some(&path),
        )
        .expect("stack_at");

        assert_eq!(out.commits.len(), 0);
        assert_eq!(out.base, "HEAD");
        assert!(
            !out.base_auto_detected,
            "explicit --base should not auto-detect"
        );
        assert!(out.success);
    }

    #[test]
    fn lists_commits_oldest_first_over_explicit_base() {
        let (dir, path) = init_repo();
        // Mark base before adding new commits.
        Command::new("git")
            .args(["tag", "base"])
            .current_dir(&path)
            .output()
            .unwrap();

        add_commit(&dir, &path, "a.txt", "a\n", "first new");
        add_commit(&dir, &path, "b.txt", "b\n", "second new");
        add_commit(&dir, &path, "c.txt", "c\n", "third new");

        let out = stack_at(
            None,
            StackOptions {
                base: Some("base".to_string()),
                no_pr: true,
                ..Default::default()
            },
            Some(&path),
        )
        .expect("stack_at");

        assert_eq!(out.commits.len(), 3);
        // Oldest-first ordering — the first new commit should be at index 0.
        assert_eq!(out.commits[0].commit.subject, "first new");
        assert_eq!(out.commits[1].commit.subject, "second new");
        assert_eq!(out.commits[2].commit.subject, "third new");
        // Each commit has a populated 7-char short_sha.
        for c in &out.commits {
            assert_eq!(c.commit.short_sha.len(), 7);
            assert!(c.pr.is_none());
            assert!(c.pr_lookup_note.is_none());
        }
        assert_eq!(out.merged_count, 0);
    }

    #[test]
    fn errors_helpfully_when_no_upstream_and_no_base_arg() {
        // Fresh repo with no remote / no @{upstream} configured.
        let (_dir, path) = init_repo();

        let err = stack_at(None, StackOptions::default(), Some(&path))
            .expect_err("stack_at should Err without upstream or --base");

        let msg = err.to_string();
        assert!(
            msg.contains("upstream") || msg.contains("--base"),
            "expected helpful error, got: {}",
            msg
        );
    }

    #[test]
    fn errors_when_base_ref_does_not_exist() {
        let (_dir, path) = init_repo();

        let err = stack_at(
            None,
            StackOptions {
                base: Some("does-not-exist".to_string()),
                no_pr: true,
                ..Default::default()
            },
            Some(&path),
        )
        .expect_err("stack_at should Err on bad base ref");

        let msg = err.to_string();
        assert!(
            msg.contains("does-not-exist") || msg.contains("not found"),
            "expected ref-not-found error, got: {}",
            msg
        );
    }
}
