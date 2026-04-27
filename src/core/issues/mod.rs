//! Issue reconciliation: finding-stream → tracker.
//!
//! This module turns a structured stream of categorized findings (from
//! `homeboy audit`, `homeboy lint`, `homeboy test`) into a deterministic
//! plan against an issue tracker. The plan can then be printed (dry-run)
//! or executed via a [`Tracker`] implementation.
//!
//! # Why this exists
//!
//! Before this module landed, `homeboy-action`'s `auto-file-categorized-issues.sh`
//! (~809 lines of bash + jq + `gh api`) was the only place this logic lived.
//! That meant:
//!
//! - The reconciliation contract had no real home, tests, or types.
//! - The `gh api ?state=open` query threw away `state_reason` — so a human
//!   closing an audit issue with `state_reason=not_planned` (the GitHub-native
//!   "do not re-file" signal) was invisible to the next CI run, which would
//!   re-file the same category as a brand-new issue.
//! - Every consumer that wanted issue auto-filing (a cron job, a pre-commit
//!   hook, a future `homeboy ci` command, an LLM agent) had to reimplement
//!   the bash from scratch.
//!
//! See homeboy issue #1551 for the full architectural framing.
//!
//! # Module shape
//!
//! - [`plan`]: pure types — [`ReconcilePlan`], [`ReconcileAction`],
//!   [`TrackedIssue`], [`IssueGroup`], [`ReconcileConfig`].
//! - [`reconcile`]: the pure decision function — `(groups, issues, config) →
//!   ReconcilePlan`. Pure means: no I/O, deterministic, fully testable.
//! - [`tracker`]: the I/O seam — [`Tracker`] trait abstracts over GitHub /
//!   future GitLab / future Linear. [`tracker::GithubTracker`] is the default
//!   impl shelling out to `gh` via `core/git/github.rs`.
//! - [`apply`]: walks a [`ReconcilePlan`] and asks the [`Tracker`] to perform
//!   each action. Returns a [`ReconcileResult`] with per-action outcomes.

pub mod apply;
pub mod plan;
pub mod reconcile;
pub mod tracker;

pub use apply::{apply_plan, ReconcileExecution, ReconcileResult};
pub use plan::{
    default_review_only_categories, IssueGroup, ReconcileAction, ReconcileConfig, ReconcilePlan,
    ReconcileSkipReason, TrackedIssue, TrackedIssueState,
};
pub use reconcile::reconcile;
pub use tracker::{GithubTracker, Tracker};
