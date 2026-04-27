//! `homeboy stack apply` — rebuild the target branch from base + cherry-picked PRs.
//!
//! Algorithm (Phase 1 MVP):
//!
//! 1. Resolve `component_path`, fetch `base.remote/base.branch`.
//! 2. Best-effort fetch `target.remote/target.branch` so existing local
//!    history is up-to-date for diffing later. Failure here is non-fatal:
//!    a fresh stack may not have pushed `target` yet.
//! 3. Force-recreate `target.branch` locally from `base.remote/base.branch`.
//! 4. For each PR entry:
//!    - Resolve the PR's head SHA + head repo coordinates via `gh pr view`.
//!    - Add a temporary remote for the PR's head repo (if it's not the
//!      base repo and not already configured) and fetch the head SHA.
//!    - `git cherry-pick <sha>`.
//!    - On `--allow-empty`-style "nothing to commit" outcome (the PR is
//!      already in base), skip cleanly.
//!    - On any other conflict, abort the in-progress cherry-pick and
//!      return [`Error::stack_apply_conflict`] with a clear pause message.
//!
//! `apply` does NOT push to `target.remote`. That's `stack push` (Phase 2).
//!
//! Conflict resume primitives (`--continue` / `--reset`) are deferred to
//! Phase 2 — `apply` just prints what to run and exits non-zero.

use serde::Serialize;
use std::collections::HashSet;

use crate::error::{Error, Result};

use super::git::run_git;
use super::pr_meta::{fetch_pr_meta, PrHead};
use super::spec::{resolve_existing_component_path, StackPrEntry, StackSpec};

/// Per-PR outcome from a single `apply` run.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedPr {
    pub repo: String,
    pub number: u64,
    pub sha: String,
    /// `picked` (cherry-pick succeeded with new commit), `skipped_empty`
    /// (changes already in base), or `conflict` (errored — apply stopped here).
    pub outcome: PickOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Outcome of a single cherry-pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PickOutcome {
    /// Cherry-pick produced a new commit on `target`.
    Picked,
    /// Cherry-pick was empty — the PR's head SHA is already in base.
    SkippedEmpty,
    /// Cherry-pick conflicted. `apply` stops at this PR; the caller resolves
    /// manually with standard git tools and reruns `apply` (Phase 2 will add
    /// `--continue`).
    Conflict,
}

/// Output envelope for `homeboy stack apply`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutput {
    pub stack_id: String,
    pub component_path: String,
    pub branch: String,
    pub base: String,
    pub target: String,
    pub applied: Vec<AppliedPr>,
    pub picked_count: usize,
    pub skipped_count: usize,
    pub conflict_count: usize,
    pub success: bool,
}

/// Output envelope for `homeboy stack rebase`.
///
/// `rebase` intentionally reports the same data as `apply`: both verbs rebuild
/// the target branch from base plus the PRs currently listed in the spec. The
/// distinction is vocabulary — `rebase` is the upkeep-safe variant that never
/// edits the spec, while `sync` adds spec maintenance on top.
pub type RebaseOutput = ApplyOutput;

/// Apply a stack spec: build `target` from `base + prs`.
pub fn apply(spec: &StackSpec) -> Result<ApplyOutput> {
    rebuild(spec, "apply")
}

/// Rebase a stack spec: rebuild `target` from `base + prs` without editing the
/// stack spec, even when some listed PRs have merged upstream.
pub fn rebase(spec: &StackSpec) -> Result<RebaseOutput> {
    rebuild(spec, "rebase")
}

fn rebuild(spec: &StackSpec, rerun_verb: &str) -> Result<ApplyOutput> {
    let path = resolve_existing_component_path(spec)?;

    // 2. Fetch base — must succeed.
    fetch_remote_branch(&path, &spec.base.remote, &spec.base.branch)?;

    // 3. Best-effort fetch target.
    let _ = fetch_remote_branch(&path, &spec.target.remote, &spec.target.branch);

    // 4. Force-recreate target locally from base.
    let base_ref = format!("{}/{}", spec.base.remote, spec.base.branch);
    checkout_force(&path, &spec.target.branch, &base_ref)?;

    // Track which remotes we've ensured exist this run, so we don't
    // shell out repeatedly for the same head repo.
    let mut ensured_remotes: HashSet<String> = HashSet::new();

    let mut applied: Vec<AppliedPr> = Vec::with_capacity(spec.prs.len());
    let mut picked = 0usize;
    let mut skipped = 0usize;

    for pr in &spec.prs {
        let head = fetch_pr_meta(pr)?.require_head(pr)?;

        // Ensure we can fetch the head SHA. If it lives in a different
        // repo than the base remote, add a temp remote keyed by the head
        // repo's slug (avoids collisions with user-configured remotes).
        let head_remote = ensure_head_remote(&path, pr, &head, &mut ensured_remotes)?;
        fetch_sha(&path, &head_remote, &head.sha)?;

        // Cherry-pick.
        match cherry_pick(&path, &head.sha)? {
            CherryPickResult::Picked => {
                picked += 1;
                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: head.sha.clone(),
                    outcome: PickOutcome::Picked,
                    note: pr.note.clone(),
                });
            }
            CherryPickResult::Empty => {
                skipped += 1;
                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: head.sha.clone(),
                    outcome: PickOutcome::SkippedEmpty,
                    note: Some("PR changes already present in base — skipped".to_string()),
                });
            }
            CherryPickResult::Conflict(message) => {
                // Abort the in-progress cherry-pick so the working tree is
                // left clean. Phase 2 `--continue` will skip this abort.
                let _ = run_git(&path, &["cherry-pick", "--abort"]);

                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: head.sha.clone(),
                    outcome: PickOutcome::Conflict,
                    note: Some(message.clone()),
                });

                return Err(Error::stack_apply_conflict(
                    &spec.id,
                    pr.number,
                    &pr.repo,
                    format!(
                        "{}\n  Resolve manually with standard git tools, then re-run \
                         `homeboy stack {} {}`. (Phase 3 will add `--continue`.)",
                        message, rerun_verb, spec.id
                    ),
                ));
            }
        }
    }

    Ok(ApplyOutput {
        stack_id: spec.id.clone(),
        component_path: path,
        branch: spec.target.branch.clone(),
        base: spec.base.display(),
        target: spec.target.display(),
        applied,
        picked_count: picked,
        skipped_count: skipped,
        conflict_count: 0,
        success: true,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// One of three outcomes from a single `git cherry-pick` invocation.
#[derive(Debug)]
pub(crate) enum CherryPickResult {
    Picked,
    Empty,
    Conflict(String),
}

pub(super) fn fetch_remote_branch(path: &str, remote: &str, branch: &str) -> Result<()> {
    let output = run_git(path, &["fetch", remote, branch])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git fetch {} {}: {}",
            remote,
            branch,
            stderr.trim()
        )));
    }
    Ok(())
}

pub(crate) fn checkout_force(path: &str, branch: &str, start_point: &str) -> Result<()> {
    let output = run_git(path, &["checkout", "-B", branch, start_point])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git checkout -B {} {}: {}",
            branch,
            start_point,
            stderr.trim()
        )));
    }
    Ok(())
}

/// Make sure a git remote exists pointing at the PR's head repo, and return
/// its name. The remote name is derived from the head-repo slug
/// (`owner-name` lowercased) so two PRs from the same fork share a remote.
///
/// If a remote with the right URL already exists (any name), reuses it
/// instead of adding a new one.
pub(super) fn ensure_head_remote(
    path: &str,
    _pr: &StackPrEntry,
    head: &PrHead,
    ensured: &mut HashSet<String>,
) -> Result<String> {
    if let Some(name) = find_existing_remote(path, &head.clone_url)? {
        ensured.insert(name.clone());
        return Ok(name);
    }

    let synthesized = format!(
        "homeboy-stack-{}",
        head.head_repo.replace('/', "-").to_lowercase()
    );

    if ensured.contains(&synthesized) {
        return Ok(synthesized);
    }

    let output = run_git(path, &["remote", "add", &synthesized, &head.clone_url])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "remote already exists" is fine — we may have raced or be on a
        // re-run after a partial earlier apply.
        if !stderr.contains("already exists") {
            return Err(Error::git_command_failed(format!(
                "git remote add {} {}: {}",
                synthesized,
                head.clone_url,
                stderr.trim()
            )));
        }
    }

    ensured.insert(synthesized.clone());
    Ok(synthesized)
}

fn find_existing_remote(path: &str, url: &str) -> Result<Option<String>> {
    let output = run_git(path, &["remote", "-v"])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git remote -v: {}",
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: `<name>\t<url> (fetch|push)`
        let mut cols = line.split_whitespace();
        let name = cols.next().unwrap_or("");
        let candidate = cols.next().unwrap_or("");
        if !name.is_empty() && url_matches(candidate, url) {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Loose URL match: accepts `https://...`, `http://...`, `git@github.com:...`,
/// trailing-`.git` differences. Just compares the `<owner>/<repo>` segment.
pub(crate) fn url_matches(a: &str, b: &str) -> bool {
    fn key(url: &str) -> Option<String> {
        let stripped = url
            .trim_end_matches(".git")
            .trim_start_matches("https://github.com/")
            .trim_start_matches("http://github.com/")
            .trim_start_matches("git@github.com:");
        if stripped.is_empty() || stripped == url {
            return None;
        }
        Some(stripped.to_lowercase())
    }
    match (key(a), key(b)) {
        (Some(ka), Some(kb)) => ka == kb,
        _ => false,
    }
}

pub(super) fn fetch_sha(path: &str, remote: &str, sha: &str) -> Result<()> {
    let output = run_git(path, &["fetch", remote, sha])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "git fetch {} {}: {}",
            remote,
            sha,
            stderr.trim()
        )));
    }
    Ok(())
}

pub(crate) fn cherry_pick(path: &str, sha: &str) -> Result<CherryPickResult> {
    let output = run_git(path, &["cherry-pick", sha])?;
    if output.status.success() {
        return Ok(CherryPickResult::Picked);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let combined = format!("{}{}", stdout, stderr);

    // Empty cherry-pick: PR already in base. Various wordings across git
    // versions; check both the canonical phrase and the short-form hint.
    if combined.contains("nothing to commit") || combined.contains("--allow-empty") {
        // Abort to leave the working tree clean before continuing.
        let _ = run_git(path, &["cherry-pick", "--skip"]);
        return Ok(CherryPickResult::Empty);
    }

    Ok(CherryPickResult::Conflict(combined.trim().to_string()))
}

#[cfg(test)]
#[path = "../../../tests/core/stack/apply_test.rs"]
mod apply_test;
