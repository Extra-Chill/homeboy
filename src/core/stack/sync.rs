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

use crate::core::error::{Error, Result};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};

const STACK_SYNC_DROP_KIND: &str = "stack.sync.drop";
const STACK_SYNC_REPLAY_KIND: &str = "stack.sync.replay";
const STACK_SYNC_UNCERTAIN_KIND: &str = "stack.sync.uncertain";

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
    #[serde(flatten)]
    pub plan: HomeboyPlan,
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
        let Some(meta) = record_uncertain(&mut uncertain, pr, fetch_pr_meta(pr)) else {
            continue;
        };

        let Some(head) = record_uncertain(&mut uncertain, pr, meta.require_head(pr)) else {
            continue;
        };

        let Some(head_remote) = record_uncertain(
            &mut uncertain,
            pr,
            ensure_head_remote(&path, pr, &head, &mut ensured_remotes),
        ) else {
            continue;
        };

        if record_uncertain(
            &mut uncertain,
            pr,
            fetch_sha(&path, &head_remote, &meta.head_sha),
        )
        .is_none()
        {
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

    let plan = sync_homeboy_plan(
        spec,
        &dropped,
        &replayed,
        &uncertain,
        target_exists,
        target_ahead,
        target_behind,
    );
    let view = SyncPlanView::new(&plan);
    let dropped = view.dropped_prs();
    let replayed = view.replayed_prs();
    let uncertain = view.uncertain_prs();
    let dropped_count = view.action_count(STACK_SYNC_DROP_KIND);
    let replayed_count = view.action_count(STACK_SYNC_REPLAY_KIND);
    let uncertain_count = view.action_count(STACK_SYNC_UNCERTAIN_KIND);
    let would_mutate = view.would_mutate();
    let blocked = view.blocked();

    Ok(SyncPlan {
        preview: SyncPreview {
            plan,
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

fn sync_homeboy_plan(
    spec: &StackSpec,
    dropped: &[DroppedPr],
    replayed: &[ReplayedPr],
    uncertain: &[UncertainPr],
    target_exists: bool,
    target_ahead: Option<usize>,
    target_behind: Option<usize>,
) -> HomeboyPlan {
    let mut steps = Vec::new();
    for pr in dropped {
        let mut inputs = PlanValues::new()
            .string("repo", pr.repo.clone())
            .number("number", pr.number)
            .string("reason", pr.reason.clone());
        if let Some(title) = &pr.title {
            inputs = inputs.string("title", title.clone());
        }
        if let Some(merged_at) = &pr.merged_at {
            inputs = inputs.string("merged_at", merged_at.clone());
        }

        steps.push(sync_pr_step(
            STACK_SYNC_DROP_KIND,
            "drop",
            &pr.repo,
            pr.number,
            PlanStepStatus::Skipped,
            &pr.reason,
            inputs,
        ));
    }
    for pr in replayed {
        let mut inputs = PlanValues::new()
            .string("repo", pr.repo.clone())
            .number("number", pr.number)
            .string("sha", pr.sha.clone())
            .string("reason", pr.reason.clone());
        if let Some(title) = &pr.title {
            inputs = inputs.string("title", title.clone());
        }
        if let Some(url) = &pr.url {
            inputs = inputs.string("url", url.clone());
        }
        if let Some(upstream_state) = &pr.upstream_state {
            inputs = inputs.string("upstream_state", upstream_state.clone());
        }
        if let Some(note) = &pr.note {
            inputs = inputs.string("note", note.clone());
        }

        steps.push(sync_pr_step(
            STACK_SYNC_REPLAY_KIND,
            "replay",
            &pr.repo,
            pr.number,
            PlanStepStatus::Ready,
            &pr.reason,
            inputs,
        ));
    }
    for pr in uncertain {
        let mut inputs = PlanValues::new()
            .string("repo", pr.repo.clone())
            .number("number", pr.number)
            .string("error", pr.error.clone())
            .string("reason", pr.error.clone());
        if let Some(note) = &pr.note {
            inputs = inputs.string("note", note.clone());
        }

        steps.push(sync_pr_step(
            STACK_SYNC_UNCERTAIN_KIND,
            "uncertain",
            &pr.repo,
            pr.number,
            PlanStepStatus::Missing,
            &pr.error,
            inputs,
        ));
    }

    let dropped_count = steps
        .iter()
        .filter(|step| step.kind == STACK_SYNC_DROP_KIND)
        .count();
    let replayed_count = steps
        .iter()
        .filter(|step| step.kind == STACK_SYNC_REPLAY_KIND)
        .count();
    let blocked = steps
        .iter()
        .any(|step| step.blocking && step.status == PlanStepStatus::Missing);
    let would_mutate = sync_would_mutate_from_parts(
        target_exists,
        target_ahead,
        target_behind,
        dropped_count,
        replayed_count,
    );

    HomeboyPlan::builder_for_description(PlanKind::StackSync, spec.id.clone())
        .inputs(
            PlanValues::new()
                .string("stack_id", spec.id.clone())
                .bool("target_exists", target_exists)
                .json("target_ahead", target_ahead)
                .json("target_behind", target_behind),
        )
        .policy_value("would_mutate", serde_json::Value::Bool(would_mutate))
        .policy_value("blocked", serde_json::Value::Bool(blocked))
        .steps(steps)
        .summarize()
        .build()
}

fn sync_pr_step(
    kind: &str,
    action: &str,
    repo: &str,
    number: u64,
    status: PlanStepStatus,
    reason: &str,
    inputs: PlanValues,
) -> PlanStep {
    let builder = PlanStep::builder(
        format!("stack.sync.{action}.{repo}#{number}"),
        kind.to_string(),
        status.clone(),
    )
    .label(format!("{action} {repo}#{number}"))
    .blocking(status == PlanStepStatus::Missing)
    .scope(vec![format!("{repo}#{number}")])
    .inputs(inputs);

    if status == PlanStepStatus::Skipped {
        builder.skip_reason(reason.to_string()).build()
    } else {
        builder.build()
    }
}

pub(crate) fn sync_plan_would_mutate(plan: &HomeboyPlan) -> bool {
    SyncPlanView::new(plan).would_mutate()
}

struct SyncPlanView<'a> {
    plan: &'a HomeboyPlan,
}

impl<'a> SyncPlanView<'a> {
    fn new(plan: &'a HomeboyPlan) -> Self {
        Self { plan }
    }

    fn dropped_prs(&self) -> Vec<DroppedPr> {
        self.steps(STACK_SYNC_DROP_KIND)
            .map(|step| DroppedPr {
                repo: self.string_input(step, "repo"),
                number: self.u64_input(step, "number"),
                title: self.optional_string_input(step, "title"),
                merged_at: self.optional_string_input(step, "merged_at"),
                reason: self.string_input(step, "reason"),
            })
            .collect()
    }

    fn replayed_prs(&self) -> Vec<ReplayedPr> {
        self.steps(STACK_SYNC_REPLAY_KIND)
            .map(|step| ReplayedPr {
                repo: self.string_input(step, "repo"),
                number: self.u64_input(step, "number"),
                sha: self.string_input(step, "sha"),
                title: self.optional_string_input(step, "title"),
                url: self.optional_string_input(step, "url"),
                upstream_state: self.optional_string_input(step, "upstream_state"),
                note: self.optional_string_input(step, "note"),
                reason: self.string_input(step, "reason"),
            })
            .collect()
    }

    fn uncertain_prs(&self) -> Vec<UncertainPr> {
        self.steps(STACK_SYNC_UNCERTAIN_KIND)
            .map(|step| UncertainPr {
                repo: self.string_input(step, "repo"),
                number: self.u64_input(step, "number"),
                note: self.optional_string_input(step, "note"),
                error: self.string_input(step, "error"),
            })
            .collect()
    }

    fn action_count(&self, kind: &str) -> usize {
        self.steps(kind).count()
    }

    fn would_mutate(&self) -> bool {
        self.bool_policy("would_mutate").unwrap_or_else(|| {
            sync_would_mutate_from_parts(
                self.plan
                    .inputs
                    .get("target_exists")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                self.usize_input("target_ahead"),
                self.usize_input("target_behind"),
                self.action_count(STACK_SYNC_DROP_KIND),
                self.action_count(STACK_SYNC_REPLAY_KIND),
            )
        })
    }

    fn blocked(&self) -> bool {
        self.bool_policy("blocked").unwrap_or_else(|| {
            self.plan
                .summary
                .as_ref()
                .map(|summary| summary.blocked > 0)
                .unwrap_or_else(|| {
                    self.plan
                        .steps
                        .iter()
                        .any(|step| step.blocking && step.status == PlanStepStatus::Missing)
                })
        })
    }

    fn steps(&self, kind: &'a str) -> impl Iterator<Item = &'a PlanStep> {
        self.plan.steps.iter().filter(move |step| step.kind == kind)
    }

    fn bool_policy(&self, key: &str) -> Option<bool> {
        self.plan
            .policy
            .get(key)
            .and_then(serde_json::Value::as_bool)
    }

    fn usize_input(&self, key: &str) -> Option<usize> {
        self.plan
            .inputs
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    }

    fn string_input(&self, step: &PlanStep, key: &str) -> String {
        self.optional_string_input(step, key).unwrap_or_default()
    }

    fn optional_string_input(&self, step: &PlanStep, key: &str) -> Option<String> {
        step.inputs
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    fn u64_input(&self, step: &PlanStep, key: &str) -> u64 {
        step.inputs
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    }
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

fn record_uncertain<T>(
    uncertain: &mut Vec<UncertainPr>,
    pr: &StackPrEntry,
    result: Result<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            uncertain.push(uncertain_pr(pr, error.to_string()));
            None
        }
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

pub(crate) fn sync_would_mutate_from_parts(
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
