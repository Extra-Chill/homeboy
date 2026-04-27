//! `homeboy stack status` — read-only report on a stack's PR list and
//! local target state.
//!
//! Performs ONE `git fetch <base.remote>` so ahead-counts are fresh; no
//! other mutations.
//!
//! For each declared PR:
//!   - `gh pr view <repo> <number> --json state,mergedAt,reviewDecision,title,url,headRefOid`
//!   - Cross-check whether the PR's head SHA is reachable from `target.branch`
//!     locally (`git merge-base --is-ancestor`) — surfaces the
//!     `applied / missing` axis the spec doesn't have.
//!
//! Status NEVER mutates the working tree. PR-API failures degrade gracefully
//! to a per-PR error note; the rest of the report still renders.

use serde::Serialize;

use crate::error::Result;

use super::git::run_git;
use super::pr_meta::fetch_pr_meta;
use super::spec::{resolve_existing_component_path, StackPrEntry, StackSpec};

#[derive(Debug, Clone, Serialize)]
pub struct StatusOutput {
    pub stack_id: String,
    pub component_path: String,
    pub base: String,
    pub target: String,
    /// `git rev-list --count <base.remote>/<base.branch>..<target.branch>`.
    /// `None` when the local target branch doesn't exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ahead: Option<usize>,
    /// `git rev-list --count <target.branch>..<base.remote>/<base.branch>`.
    /// `None` when the local target branch doesn't exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_behind: Option<usize>,
    pub prs: Vec<StatusPr>,
    pub merged_count: usize,
    pub success: bool,
}

/// Per-PR row in the status report.
#[derive(Debug, Clone, Serialize)]
pub struct StatusPr {
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Upstream PR title (from `gh pr view`). Absent when the API lookup
    /// failed for this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Upstream PR URL (`https://github.com/...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `OPEN` / `CLOSED` / `MERGED` (gh's casing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_state: Option<String>,
    /// `gh`'s `reviewDecision` field — `APPROVED`, `CHANGES_REQUESTED`,
    /// `REVIEW_REQUIRED`, etc. Absent when the lookup failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    /// Merge timestamp (RFC3339) when `state = MERGED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    /// `applied` (head SHA reachable from local target), `missing` (not
    /// reachable), or `unknown` (couldn't ask: PR API failed or local
    /// target branch missing).
    pub local_state: LocalState,
    /// Set when a PR is upstream-merged AND still cherry-picked locally —
    /// the "drop me from the spec" hint. Mirrors the issue body's
    /// example output.
    #[serde(default, skip_serializing_if = "is_false")]
    pub candidate_for_drop: bool,
    /// Diagnostic note when the API lookup failed for this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Applied,
    Missing,
    Unknown,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Run `homeboy stack status <id>`.
pub fn status(spec: &StackSpec) -> Result<StatusOutput> {
    let path = resolve_existing_component_path(spec)?;

    // Single fetch of the base, so ahead-counts are honest. Failure here is
    // non-fatal — we want status to work offline.
    let _ = run_git(&path, &["fetch", &spec.base.remote, &spec.base.branch]);

    let base_ref = format!("{}/{}", spec.base.remote, spec.base.branch);
    let target_branch = &spec.target.branch;

    let target_exists = git_ref_exists(&path, target_branch);

    let (target_ahead, target_behind) = if target_exists {
        (
            count_revs(&path, &base_ref, target_branch),
            count_revs(&path, target_branch, &base_ref),
        )
    } else {
        (None, None)
    };

    let mut prs: Vec<StatusPr> = Vec::with_capacity(spec.prs.len());
    let mut merged_count = 0usize;

    for pr in &spec.prs {
        let row = build_status_row(&path, target_branch, &base_ref, target_exists, pr);
        if row.upstream_state.as_deref() == Some("MERGED") {
            merged_count += 1;
        }
        prs.push(row);
    }

    Ok(StatusOutput {
        stack_id: spec.id.clone(),
        component_path: path,
        base: spec.base.display(),
        target: spec.target.display(),
        target_ahead,
        target_behind,
        prs,
        merged_count,
        success: true,
    })
}

fn build_status_row(
    path: &str,
    target_branch: &str,
    base_ref: &str,
    target_exists: bool,
    pr: &StackPrEntry,
) -> StatusPr {
    match fetch_pr_meta(pr) {
        Ok(meta) => {
            let local_state = if !target_exists {
                LocalState::Unknown
            } else {
                match commit_reachable(path, &meta.head_sha, target_branch) {
                    Some(true) => LocalState::Applied,
                    Some(false) => {
                        // Squash-merge fallback: head SHA isn't reachable
                        // from target, but the patch may be in base if
                        // upstream squash-merged the PR. Treat patch-in-base
                        // as Applied so `candidate_for_drop` fires. This
                        // deliberately uses the BASE ref (not target) — the
                        // question is "did upstream absorb this content?",
                        // not "is it on target?".
                        if patch_in_base(path, &meta.head_sha, base_ref).unwrap_or(false) {
                            LocalState::Applied
                        } else {
                            LocalState::Missing
                        }
                    }
                    None => LocalState::Unknown,
                }
            };
            let candidate_for_drop = meta.state == "MERGED" && local_state == LocalState::Applied;

            StatusPr {
                repo: pr.repo.clone(),
                number: pr.number,
                note: pr.note.clone(),
                title: Some(meta.title_for_status()),
                url: Some(meta.url_for_status()),
                upstream_state: Some(meta.state),
                review_decision: meta.review_decision,
                merged_at: meta.merged_at,
                local_state,
                candidate_for_drop,
                error: None,
            }
        }
        Err(e) => StatusPr {
            repo: pr.repo.clone(),
            number: pr.number,
            note: pr.note.clone(),
            title: None,
            url: None,
            upstream_state: None,
            review_decision: None,
            merged_at: None,
            local_state: LocalState::Unknown,
            candidate_for_drop: false,
            error: Some(e.to_string()),
        },
    }
}

pub(crate) fn count_revs(path: &str, from: &str, to: &str) -> Option<usize> {
    let range = format!("{}..{}", from, to);
    let output = run_git(path, &["rev-list", "--count", &range]).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub(crate) fn git_ref_exists(path: &str, refname: &str) -> bool {
    let output = match run_git(path, &["rev-parse", "--verify", "--quiet", refname]) {
        Ok(o) => o,
        Err(_) => return false,
    };
    output.status.success()
}

/// `git merge-base --is-ancestor <sha> <branch>` is the canonical "is this
/// commit reachable from the branch tip" probe. Returns `Some(true)` /
/// `Some(false)` / `None` for "couldn't tell" (commit not present locally,
/// e.g. the SHA hasn't been fetched).
pub(crate) fn commit_reachable(path: &str, sha: &str, branch: &str) -> Option<bool> {
    if sha.is_empty() {
        return None;
    }
    // First check the SHA is even known locally.
    let lookup = run_git(path, &["cat-file", "-e", sha]).ok()?;
    if !lookup.status.success() {
        return None;
    }
    let output = run_git(path, &["merge-base", "--is-ancestor", sha, branch]).ok()?;
    // Exit 0 = ancestor, 1 = not ancestor, 128 = error.
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Whether the PR's content is in `base_ref` via `git cherry` patch-id
/// equivalence — handles squash-merge where the PR's head SHA is replaced
/// with a new commit on base whose tree matches the PR's content but whose
/// SHA is unrelated. Without this fallback, `local_state` reports `missing`
/// for squash-merged PRs that were already cherry-picked onto target, and
/// `candidate_for_drop` never fires for the most common GitHub merge style.
///
/// Returns `Some(true)` when the PR's patch-id appears in base (the `-`
/// prefix in `git cherry` output), `Some(false)` when it doesn't, and
/// `None` when the probe couldn't run (commit not local, git error, etc.).
pub(crate) fn patch_in_base(path: &str, head_sha: &str, base_ref: &str) -> Option<bool> {
    if head_sha.is_empty() {
        return None;
    }
    // Confirm the SHA is actually present locally before asking git to
    // diff against it — otherwise `git cherry` errors with a confusing
    // "bad revision" message.
    let lookup = run_git(path, &["cat-file", "-e", head_sha]).ok()?;
    if !lookup.status.success() {
        return None;
    }
    // `git cherry <upstream> <head>` lists every commit reachable from
    // <head> that is NOT in <upstream>, prefixing with `+` if its patch-id
    // isn't present in <upstream>, and `-` if it is. We pass the PR's
    // head SHA as <head>, so the output is one line; if that line starts
    // with `- ` the patch is already in base.
    let output = run_git(path, &["cherry", base_ref, head_sha]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.lines().any(|l| l.starts_with("- ")))
}

#[cfg(test)]
#[path = "../../../tests/core/stack/status_test.rs"]
mod status_test;
