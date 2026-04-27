//! `homeboy stack sync` — rebase + auto-drop merged PRs from the spec.
//!
//! Phase 2 follow-up to `apply`. `sync` is the holistic upkeep verb for a
//! combined-fixes branch:
//!
//!   1. Resolve every PR in the spec via `gh pr view` (state, mergedAt,
//!      headRefOid, head repo coordinates).
//!   2. Partition into a **drop list** (PRs upstream-merged AND content
//!      already in base) and a **pick list** (everything else).
//!   3. Persist the spec with drops removed (unless `--dry-run`) BEFORE any
//!      cherry-picks. Rationale: a partial cherry-pick failure leaves a
//!      half-applied target branch but a correctly-pruned spec, so re-running
//!      `sync` is a clean rebuild.
//!   4. Force-recreate `target.branch` from `base.remote/base.branch`.
//!   5. Cherry-pick the pick list in order. On conflict, abort the
//!      in-progress pick and return [`Error::stack_apply_conflict`].
//!
//! Drop semantics:
//!   A PR is droppable iff `state == "MERGED"` AND its content is in base
//!   — either the head SHA is reachable from base
//!   ([`status::commit_reachable`]) OR its patch-id appears in base
//!   ([`status::patch_in_base`], the squash-merge fallback from PR #1573).
//!
//!   Merged-but-content-missing (rebase-and-force-push scenario): keep
//!   the PR, attempt the cherry-pick. We never lose a non-trivial commit
//!   the user explicitly added.
//!
//!   Content-in-base-but-still-OPEN (reviewer cherry-picked to a release
//!   branch): keep the PR. `sync` only drops on official upstream MERGE.

use serde::Serialize;
use std::collections::HashSet;

use crate::error::{Error, Result};

use super::apply::{
    checkout_force, cherry_pick, ensure_head_remote, fetch_remote_branch, fetch_sha, AppliedPr,
    CherryPickResult, PickOutcome,
};
use super::git::run_git;
use super::pr_meta::fetch_pr_meta;
pub(crate) use super::pr_meta::StackPrMeta as PrMeta;
use super::spec::{resolve_existing_component_path, save, StackPrEntry, StackSpec};
use super::status::{commit_reachable, patch_in_base};

/// Output envelope for `homeboy stack sync`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncOutput {
    pub stack_id: String,
    pub component_path: String,
    pub branch: String,
    pub base: String,
    pub target: String,
    /// PRs auto-removed from the spec because they were upstream-merged
    /// AND their content was already in base.
    pub dropped: Vec<DroppedPr>,
    /// PRs cherry-picked onto the rebuilt target branch.
    pub applied: Vec<AppliedPr>,
    /// `true` when called with `--dry-run`: the spec on disk was NOT
    /// mutated and no cherry-picks ran.
    pub dry_run: bool,
    pub picked_count: usize,
    pub skipped_count: usize,
    pub dropped_count: usize,
    pub success: bool,
}

/// One PR auto-removed from the spec.
#[derive(Debug, Clone, Serialize)]
pub struct DroppedPr {
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    /// Human-readable reason — e.g. "merged upstream and content in base".
    pub reason: String,
}

/// Decide whether a PR should be dropped from the spec.
///
/// Pure with respect to the (already-fetched) `PrMeta` — only touches the
/// local git repo to probe reachability and patch-id equivalence. Reuses
/// the same probes `status::candidate_for_drop` uses, so the two verbs
/// agree on what "applied" means.
pub(crate) fn is_droppable(meta: &PrMeta, path: &str, base_ref: &str) -> bool {
    if meta.state != "MERGED" {
        return false;
    }
    if meta.head_sha.is_empty() {
        return false;
    }
    if commit_reachable(path, &meta.head_sha, base_ref) == Some(true) {
        return true;
    }
    patch_in_base(path, &meta.head_sha, base_ref).unwrap_or(false)
}

/// Sync a stack: rebuild target from base, auto-drop merged PRs, replay
/// the rest.
pub fn sync(spec: &mut StackSpec, dry_run: bool) -> Result<SyncOutput> {
    let path = resolve_existing_component_path(spec)?;

    // 1. Fetch base — must succeed so droppability checks are honest.
    fetch_remote_branch(&path, &spec.base.remote, &spec.base.branch)?;
    // Best-effort fetch target; failure is fine on a fresh stack.
    let _ = fetch_remote_branch(&path, &spec.target.remote, &spec.target.branch);

    let base_ref = format!("{}/{}", spec.base.remote, spec.base.branch);

    // 2. Resolve metadata for every PR up front. We need head SHAs locally
    //    BEFORE deciding droppability — `commit_reachable` and
    //    `patch_in_base` both require the SHA to be in the object store.
    //    Use a temp remote per fork (same machinery as `apply`).
    let mut ensured_remotes: HashSet<String> = HashSet::new();
    let mut metas: Vec<PrMeta> = Vec::with_capacity(spec.prs.len());

    for pr in &spec.prs {
        let meta = fetch_pr_meta(pr)?;
        let head = meta.require_head(pr)?;
        // Fetch the head SHA into the local object store before asking
        // git about reachability/patch-id.
        let head_remote = ensure_head_remote(&path, pr, &head, &mut ensured_remotes)?;
        if !meta.head_sha.is_empty() {
            // Best-effort fetch — a 404 here means the SHA is gone from
            // the head repo (force-pushed away). is_droppable() will then
            // return false and the cherry-pick path will surface the real
            // error.
            let _ = fetch_sha(&path, &head_remote, &meta.head_sha);
        }
        metas.push(meta);
    }

    // 3. Partition into drop list + pick list, preserving spec order.
    let mut dropped: Vec<DroppedPr> = Vec::new();
    let mut keep_indices: Vec<usize> = Vec::new();
    for (idx, (pr, meta)) in spec.prs.iter().zip(metas.iter()).enumerate() {
        if is_droppable(meta, &path, &base_ref) {
            dropped.push(DroppedPr {
                repo: pr.repo.clone(),
                number: pr.number,
                title: meta.title.clone(),
                merged_at: meta.merged_at.clone(),
                reason: "merged upstream and content in base".to_string(),
            });
        } else {
            keep_indices.push(idx);
        }
    }

    let dropped_count = dropped.len();

    if dry_run {
        // Report what WOULD happen; mutate nothing.
        return Ok(sync_output(
            spec,
            path,
            dropped,
            Vec::new(),
            true,
            0,
            0,
            dropped_count,
        ));
    }

    // 4. Persist the pruned spec BEFORE any cherry-picks. A partial pick
    //    failure leaves a half-applied target but a correct spec — re-run
    //    cleanly rebuilds.
    let kept: Vec<StackPrEntry> = keep_indices.iter().map(|i| spec.prs[*i].clone()).collect();
    let kept_metas: Vec<PrMeta> = keep_indices.iter().map(|i| metas[*i].clone()).collect();
    if dropped_count > 0 {
        spec.prs = kept.clone();
        save(spec)?;
    } else {
        // No spec mutation needed — but keep `kept`/`kept_metas` so the
        // pick loop has consistent indexing.
        spec.prs = kept.clone();
    }

    // 5. Force-recreate target locally from base.
    checkout_force(&path, &spec.target.branch, &base_ref)?;

    // 6. Cherry-pick the kept PRs.
    let mut applied: Vec<AppliedPr> = Vec::with_capacity(kept.len());
    let mut picked = 0usize;
    let mut skipped = 0usize;

    for (pr, meta) in kept.iter().zip(kept_metas.iter()) {
        match cherry_pick(&path, &meta.head_sha)? {
            CherryPickResult::Picked => {
                picked += 1;
                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: meta.head_sha.clone(),
                    outcome: PickOutcome::Picked,
                    note: pr.note.clone(),
                });
            }
            CherryPickResult::Empty => {
                skipped += 1;
                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: meta.head_sha.clone(),
                    outcome: PickOutcome::SkippedEmpty,
                    note: Some("PR changes already present in base — skipped".to_string()),
                });
            }
            CherryPickResult::Conflict(message) => {
                let _ = run_git(&path, &["cherry-pick", "--abort"]);

                applied.push(AppliedPr {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    sha: meta.head_sha.clone(),
                    outcome: PickOutcome::Conflict,
                    note: Some(message.clone()),
                });

                return Err(Error::stack_apply_conflict(
                    &spec.id,
                    pr.number,
                    &pr.repo,
                    format!(
                        "{}\n  Resolve manually with standard git tools, then re-run \
                         `homeboy stack sync {}`. (Phase 3 will add `--continue`.)",
                        message, spec.id
                    ),
                ));
            }
        }
    }

    Ok(sync_output(
        spec,
        path,
        dropped,
        applied,
        false,
        picked,
        skipped,
        dropped_count,
    ))
}

fn sync_output(
    spec: &StackSpec,
    path: String,
    dropped: Vec<DroppedPr>,
    applied: Vec<AppliedPr>,
    dry_run: bool,
    picked_count: usize,
    skipped_count: usize,
    dropped_count: usize,
) -> SyncOutput {
    SyncOutput {
        stack_id: spec.id.clone(),
        component_path: path,
        branch: spec.target.branch.clone(),
        base: spec.base.display(),
        target: spec.target.display(),
        dropped,
        applied,
        dry_run,
        picked_count,
        skipped_count,
        dropped_count,
        success: true,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/stack/sync_test.rs"]
mod sync_test;
