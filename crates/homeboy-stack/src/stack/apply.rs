//! `homeboy stack apply` — rebuild the target branch from base + cherry-picked PRs.
//!
//! Algorithm:
//!
//! 0. Resolve `component_path` and refuse to run while a cherry-pick from an
//!    earlier (preserved) conflict is still paused in that checkout.
//! 1. Fetch `base.remote/base.branch`.
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
//!    - On any other conflict, return [`Error::stack_apply_conflict`] with a
//!      pause message. The conflicted index and working tree are left in
//!      place so the operator can actually resolve them; pass
//!      [`ConflictPolicy::Abort`] (`--abort-on-conflict`) to have the
//!      in-progress pick aborted instead.
//!
//! `apply` does NOT push to `target.remote`. That's `stack push`.
//!
//! `apply` has no resume primitive of its own: it pauses on conflict and
//! hands the operator raw `git cherry-pick --continue` / `--abort`.

use serde::Serialize;
use std::collections::HashSet;

use homeboy_core::error::{Error, Result};

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
    /// Cherry-pick conflicted. `apply` stops at this PR; unless
    /// [`ConflictPolicy::Abort`] was requested the conflicted state is left
    /// in the checkout for the operator to resolve with standard git tools.
    Conflict,
}

/// What to do with the in-progress cherry-pick when a pick conflicts.
///
/// The default is [`ConflictPolicy::Preserve`]: aborting would delete the
/// conflict markers, the index state, and `CHERRY_PICK_HEAD` that the error
/// message tells the operator to resolve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Leave the conflicted index and working tree exactly as git left them
    /// so `git cherry-pick --continue` (or `--abort`) is available.
    #[default]
    Preserve,
    /// Run `git cherry-pick --abort`, restoring a clean working tree and
    /// discarding the conflicted state.
    Abort,
}

impl ConflictPolicy {
    /// Map the `--abort-on-conflict` CLI flag onto a policy.
    pub fn from_abort_flag(abort_on_conflict: bool) -> Self {
        if abort_on_conflict {
            Self::Abort
        } else {
            Self::Preserve
        }
    }
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
pub fn apply(spec: &StackSpec, conflict_policy: ConflictPolicy) -> Result<ApplyOutput> {
    rebuild(spec, "apply", conflict_policy)
}

/// Rebase a stack spec: rebuild `target` from `base + prs` without editing the
/// stack spec, even when some listed PRs have merged upstream.
pub fn rebase(spec: &StackSpec, conflict_policy: ConflictPolicy) -> Result<RebaseOutput> {
    rebuild(spec, "rebase", conflict_policy)
}

fn rebuild(
    spec: &StackSpec,
    rerun_verb: &str,
    conflict_policy: ConflictPolicy,
) -> Result<ApplyOutput> {
    let path = resolve_existing_component_path(spec)?;

    // A preserved conflict from an earlier run is a reachable entry state —
    // refuse to clobber it with the force-checkout below.
    ensure_no_cherry_pick_in_progress(&path, &format!("homeboy stack {} {}", rerun_verb, spec.id))?;

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
                // By default the conflicted state stays exactly where git
                // left it — the error message below tells the operator to
                // resolve it, so destroying it here would be a lie.
                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: head.sha.clone(),
                    outcome: PickOutcome::Conflict,
                    note: Some(message.clone()),
                });

                return Err(conflict_error(
                    ConflictContext {
                        path: &path,
                        stack_id: &spec.id,
                        pr,
                        sha: &head.sha,
                        rerun_command: &format!("homeboy stack {} {}", rerun_verb, spec.id),
                        policy: conflict_policy,
                    },
                    &message,
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

/// Everything the conflict path needs to explain itself to the operator.
pub(crate) struct ConflictContext<'a> {
    /// Checkout the cherry-pick is paused in.
    pub path: &'a str,
    pub stack_id: &'a str,
    pub pr: &'a StackPrEntry,
    /// Head SHA that failed to apply.
    pub sha: &'a str,
    /// Full homeboy command to re-run once the tree is clean again.
    pub rerun_command: &'a str,
    pub policy: ConflictPolicy,
}

/// Finish a conflicted cherry-pick according to `ctx.policy` and build the
/// operator-facing error.
///
/// Under [`ConflictPolicy::Preserve`] (the default) nothing is run: the
/// conflicted index, the working-tree markers, and `CHERRY_PICK_HEAD` are the
/// exact state the message asks the operator to resolve.
pub(crate) fn conflict_error(ctx: ConflictContext<'_>, message: &str) -> Error {
    if ctx.policy == ConflictPolicy::Abort {
        let _ = run_git(ctx.path, &["cherry-pick", "--abort"]);
    }
    Error::stack_apply_conflict(
        ctx.stack_id,
        ctx.pr.number,
        &ctx.pr.repo,
        conflict_guidance(&ctx, message),
    )
}

/// `true` while `path` has a cherry-pick paused mid-flight (`CHERRY_PICK_HEAD`
/// still present).
pub(crate) fn cherry_pick_in_progress(path: &str) -> bool {
    run_git(
        path,
        &["rev-parse", "--verify", "--quiet", "CHERRY_PICK_HEAD"],
    )
    .map(|output| output.status.success())
    .unwrap_or(false)
}

/// Refuse to rebuild `target` while a cherry-pick is still paused.
///
/// Preserving conflicts (the default) makes a half-resolved pick a reachable
/// state at command entry; `git checkout -B` would either fail cryptically or
/// throw away the operator's in-progress resolution.
pub(crate) fn ensure_no_cherry_pick_in_progress(path: &str, rerun_command: &str) -> Result<()> {
    if !cherry_pick_in_progress(path) {
        return Ok(());
    }
    Err(Error::git_command_failed(format!(
        "a cherry-pick is still in progress in {path}; refusing to rebuild the stack target over \
         it.\n  Finish it: resolve the conflicts, `git add` the files, then run\n    \
         git -C {path} cherry-pick --continue\n  \
         Or discard it: git -C {path} cherry-pick --abort\n  \
         Then re-run `{rerun_command}`."
    )))
}

/// Render the resolution instructions for a conflicted pick. Pure — it names
/// only commands that actually work from the state `ctx.policy` leaves behind.
pub(crate) fn conflict_guidance(ctx: &ConflictContext<'_>, message: &str) -> String {
    match ctx.policy {
        ConflictPolicy::Abort => format!(
            "{message}\n  \
             --abort-on-conflict: ran `git cherry-pick --abort` in {path}, so the checkout is \
             clean and the conflicted state for {sha} is gone.\n  \
             Re-run `{rerun}` once the PR applies cleanly, or omit --abort-on-conflict to pause \
             with the conflict intact and resolve it in place.",
            path = ctx.path,
            sha = ctx.sha,
            rerun = ctx.rerun_command,
        ),
        ConflictPolicy::Preserve => format!(
            "{message}\n  \
             The cherry-pick of {sha} is still in progress in {path} — resolve it there.\n  \
             Resolve: fix the conflicts, `git add` the files, then run\n    \
             git -C {path} cherry-pick --continue\n  \
             and re-run `{rerun}`.\n  \
             Bail out: git -C {path} cherry-pick --abort\n  \
             Pass --abort-on-conflict to have homeboy run that abort for you.",
            path = ctx.path,
            sha = ctx.sha,
            rerun = ctx.rerun_command,
        ),
    }
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
#[path = "../../../../tests/core/stack/apply_test.rs"]
mod apply_test;
