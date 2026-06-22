//! Component-aware GitHub primitives: issue and PR CRUD via the `gh` CLI.
//!
//! Shells out to `gh` (no new deps), mirroring the existing pattern used by
//! `core/release/executor::run_github_release`. All operations are scoped to a
//! component ID — the component's `remote_url` (or `git remote get-url origin`
//! fallback) resolves the GitHub owner/repo automatically.
//!
//! # Why this lives in `core/git`
//!
//! These operations are component-scoped git-graph operations, same shape as
//! `git commit`, `git push`, `git tag`. Grouping them under `git` keeps the
//! CLI surface coherent (`homeboy git issue create`, `homeboy git pr create`)
//! and reuses the existing `resolve_target` component → path resolution.
//!
//! # Error model
//!
//! When `gh` is missing, not authenticated, or fails, these functions return
//! a structured error with recovery hints. Callers get a real failure instead
//! of a silent skip — different from `run_github_release`, which soft-fails
//! because the tag is already pushed by that point.
//!
//! # Module layout
//!
//! This module was split from a single 2200-line file into focused submodules
//! (mechanical move; public API preserved via the re-exports below):
//! - [`client`] — component → owner/repo resolution, `gh` readiness/run
//!   wrappers, token discovery, shared parsing helpers.
//! - [`issues`] — issue create/comment/close/edit/find.
//! - [`pulls`] — PR create/edit/find/view/files/merge.
//! - [`fleet`] — batch PR reporting and landing.
//! - [`readiness`] — CI-check classification and merge-readiness reasoning.

mod body_file;
mod client;
mod fleet;
mod issues;
mod pulls;
mod readiness;

// Re-export the shared GitHub output/option types at this module path so
// existing `super::github::X` paths (parent `git` module + `github_pr_comments`)
// keep resolving after the split. This matches the exact type set the original
// single-file module exposed through `git::github::`.
pub use super::github_types::{
    GithubFindItem, GithubFindOutput, GithubIssueOutput, GithubPrOutput, GithubPrReadinessOutput,
    GithubPrView, IssueCloseOptions, IssueCloseReason, IssueCommentOptions, IssueCreateOptions,
    IssueEditOptions, IssueFindOptions, IssueState, PrCreateOptions, PrEditOptions, PrFindOptions,
    PrMergeOptions, PrMergeReadiness, PrMergeabilityReconcileOptions,
    PrMergeabilityReconcileOutput, PrReadinessBlocker, PrState,
};

// Internal helpers shared with sibling modules (e.g. `github_pr_comments`),
// re-exported at this module path so existing `super::github::X` paths keep
// resolving after the split.
pub(in crate::core) use body_file::push_markdown_body_file_arg;
pub(in crate::core::git) use client::{ensure_gh_ready, resolve_component_github, run_gh};

// Public probe/token helpers.
pub use client::{gh_probe_succeeds, github_token_from_env_or_gh};

// Issue operations.
pub use issues::{issue_close, issue_comment, issue_create, issue_edit, issue_find};

// Pull-request operations.
pub use pulls::{pr_create, pr_edit, pr_files, pr_find, pr_find_by_commit, pr_merge, pr_view};

// Fleet operations.
pub use fleet::pr_fleet;

// Readiness / mergeability reasoning.
pub use readiness::{pr_readiness, pr_reconcile_mergeability};
