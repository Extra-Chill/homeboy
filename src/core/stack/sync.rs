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
use super::status::{commit_reachable, count_revs, git_ref_exists, patch_in_base};

/// Output envelope for `homeboy stack sync`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncOutput {
    #[serde(flatten)]
    pub preview: SyncPreview,
    /// PRs cherry-picked onto the rebuilt target branch.
    pub applied: Vec<AppliedPr>,
    /// `true` when called with `--dry-run`: the spec on disk was NOT
    /// mutated and no cherry-picks ran.
    pub dry_run: bool,
    pub picked_count: usize,
    pub skipped_count: usize,
    pub success: bool,
}

/// Shared read-only sync preview. Used directly by `stack diff` and flattened
/// into `stack sync` output.
#[derive(Debug, Clone, Serialize)]
pub struct SyncPreview {
    pub stack_id: String,
    pub component_path: String,
    pub branch: String,
    pub base: String,
    pub target: String,
    /// PRs auto-removed from the spec because they were upstream-merged
    /// AND their content was already in base.
    pub dropped: Vec<DroppedPr>,
    /// PRs that `sync` would replay (or did replay) after rebuilding target.
    pub replayed: Vec<ReplayedPr>,
    /// PRs that could not be classified because metadata or head-fetching
    /// failed. `sync` refuses to mutate while this list is non-empty.
    pub uncertain: Vec<UncertainPr>,
    /// Whether the local target branch currently exists.
    pub target_exists: bool,
    /// `git rev-list --count <base>..<target>` before sync mutates anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ahead: Option<usize>,
    /// `git rev-list --count <target>..<base>` before sync mutates anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_behind: Option<usize>,
    pub dropped_count: usize,
    pub replayed_count: usize,
    pub uncertain_count: usize,
    pub would_mutate: bool,
    pub blocked: bool,
    pub success: bool,
}

pub type DiffOutput = SyncPreview;

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

/// One PR that would be replayed during `sync`.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayedPr {
    pub repo: String,
    pub number: u64,
    pub sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub reason: String,
}

/// One PR whose sync outcome could not be decided safely.
#[derive(Debug, Clone, Serialize)]
pub struct UncertainPr {
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SyncPlan {
    pub preview: SyncPreview,
    #[serde(skip)]
    kept_entries: Vec<StackPrEntry>,
    #[serde(skip)]
    kept_metas: Vec<PrMeta>,
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

/// Build the shared read-only plan consumed by `stack diff`, `sync --dry-run`,
/// and the mutating `sync` path.
pub(crate) fn plan_sync(spec: &StackSpec) -> Result<SyncPlan> {
    let path = resolve_existing_component_path(spec)?;

    // Fetch base so ahead/behind and droppability checks are honest. This
    // updates remote-tracking refs only; it does not touch target or the spec.
    fetch_remote_branch(&path, &spec.base.remote, &spec.base.branch)?;
    // Best-effort fetch target; a fresh stack may not have pushed it yet.
    let _ = fetch_remote_branch(&path, &spec.target.remote, &spec.target.branch);

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

    let mut ensured_remotes: HashSet<String> = HashSet::new();
    let mut dropped = Vec::new();
    let mut replayed = Vec::new();
    let mut uncertain = Vec::new();
    let mut kept_entries = Vec::new();
    let mut kept_metas = Vec::new();

    for pr in &spec.prs {
        let meta = match fetch_pr_meta(pr) {
            Ok(meta) => meta,
            Err(e) => {
                uncertain.push(uncertain_pr(pr, e.to_string()));
                continue;
            }
        };

        let head = match meta.require_head(pr) {
            Ok(head) => head,
            Err(e) => {
                uncertain.push(uncertain_pr(pr, e.to_string()));
                continue;
            }
        };

        let head_remote = match ensure_head_remote(&path, pr, &head, &mut ensured_remotes) {
            Ok(remote) => remote,
            Err(e) => {
                uncertain.push(uncertain_pr(pr, e.to_string()));
                continue;
            }
        };

        if let Err(e) = fetch_sha(&path, &head_remote, &meta.head_sha) {
            uncertain.push(uncertain_pr(pr, e.to_string()));
            continue;
        }

        if is_droppable(&meta, &path, &base_ref) {
            dropped.push(DroppedPr {
                repo: pr.repo.clone(),
                number: pr.number,
                title: meta.title.clone(),
                merged_at: meta.merged_at.clone(),
                reason: "merged upstream and content in base".to_string(),
            });
        } else {
            replayed.push(ReplayedPr {
                repo: pr.repo.clone(),
                number: pr.number,
                sha: meta.head_sha.clone(),
                title: meta.title.clone(),
                url: meta.url.clone(),
                upstream_state: Some(meta.state.clone()),
                note: pr.note.clone(),
                reason: replay_reason(&meta).to_string(),
            });
            kept_entries.push(pr.clone());
            kept_metas.push(meta);
        }
    }

    let dropped_count = dropped.len();
    let replayed_count = replayed.len();
    let uncertain_count = uncertain.len();
    let blocked = uncertain_count > 0;
    let would_mutate = sync_would_mutate(
        target_exists,
        target_ahead,
        target_behind,
        dropped_count,
        replayed_count,
    );

    Ok(SyncPlan {
        preview: SyncPreview {
            stack_id: spec.id.clone(),
            component_path: path,
            branch: spec.target.branch.clone(),
            base: spec.base.display(),
            target: spec.target.display(),
            target_exists,
            target_ahead,
            target_behind,
            dropped,
            replayed,
            uncertain,
            dropped_count,
            replayed_count,
            uncertain_count,
            would_mutate,
            blocked,
            success: true,
        },
        kept_entries,
        kept_metas,
    })
}

/// Read-only preview for `homeboy stack diff`.
pub fn diff(spec: &StackSpec) -> Result<DiffOutput> {
    let plan = plan_sync(spec)?;
    Ok(plan.preview)
}

/// Sync a stack: rebuild target from base, auto-drop merged PRs, replay
/// the rest.
pub fn sync(spec: &mut StackSpec, dry_run: bool) -> Result<SyncOutput> {
    let plan = plan_sync(spec)?;

    if dry_run {
        // Report what WOULD happen; mutate nothing.
        return Ok(sync_output(plan, Vec::new(), true, 0, 0));
    }

    if plan.preview.blocked {
        let summary = plan
            .preview
            .uncertain
            .iter()
            .map(|p| format!("{}#{}: {}", p.repo, p.number, p.error))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::git_command_failed(format!(
            "stack sync {} is blocked by uncertain PR metadata: {}",
            spec.id, summary
        )));
    }

    // 4. Persist the pruned spec BEFORE any cherry-picks. A partial pick
    //    failure leaves a half-applied target but a correct spec — re-run
    //    cleanly rebuilds.
    if plan.preview.dropped_count > 0 {
        spec.prs = plan.kept_entries.clone();
        save(spec)?;
    } else {
        // No spec mutation needed — but keep `spec.prs` aligned with the
        // plan so the pick loop has consistent indexing.
        spec.prs = plan.kept_entries.clone();
    }

    // 5. Force-recreate target locally from base.
    let base_ref = format!("{}/{}", spec.base.remote, spec.base.branch);
    checkout_force(&plan.preview.component_path, &spec.target.branch, &base_ref)?;

    // 6. Cherry-pick the kept PRs.
    let mut applied: Vec<AppliedPr> = Vec::with_capacity(plan.kept_entries.len());
    let mut picked = 0usize;
    let mut skipped = 0usize;

    for (pr, meta) in plan.kept_entries.iter().zip(plan.kept_metas.iter()) {
        match cherry_pick(&plan.preview.component_path, &meta.head_sha)? {
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
                let _ = run_git(&plan.preview.component_path, &["cherry-pick", "--abort"]);

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

    Ok(sync_output(plan, applied, false, picked, skipped))
}

fn sync_output(
    plan: SyncPlan,
    applied: Vec<AppliedPr>,
    dry_run: bool,
    picked_count: usize,
    skipped_count: usize,
) -> SyncOutput {
    SyncOutput {
        preview: plan.preview,
        applied,
        dry_run,
        picked_count,
        skipped_count,
        success: true,
    }
}

fn uncertain_pr(pr: &StackPrEntry, error: String) -> UncertainPr {
    UncertainPr {
        repo: pr.repo.clone(),
        number: pr.number,
        note: pr.note.clone(),
        error,
    }
}

fn replay_reason(meta: &PrMeta) -> &'static str {
    if meta.state == "MERGED" {
        "merged upstream but content is not in base"
    } else {
        "not merged upstream"
    }
}

pub(crate) fn sync_would_mutate(
    target_exists: bool,
    target_ahead: Option<usize>,
    target_behind: Option<usize>,
    dropped_count: usize,
    replayed_count: usize,
) -> bool {
    !target_exists
        || target_ahead.unwrap_or(0) > 0
        || target_behind.unwrap_or(0) > 0
        || dropped_count > 0
        || replayed_count > 0
}

#[cfg(test)]
#[path = "../../../tests/core/stack/sync_test.rs"]
mod sync_test;
