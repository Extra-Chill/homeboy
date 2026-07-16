//! Read-only triage reports for component sets.
//!
//! The primitive resolves a scope (component/project/fleet/rig/path/workspace) to component
//! references, then overlays GitHub issue/PR state. It intentionally keeps the
//! GitHub calls read-only so `homeboy triage ...` is safe as a dashboard verb.
//! The separate `triage --watch --auto-merge` path is the explicit opt-in
//! exception for state-transition automation.

mod gh;
mod landing;
mod observation;
mod report;
mod shared;
mod types;
mod watch;

pub use crate::scope::Scope as TriageTarget;

pub use landing::landing;
pub use report::{parse_issue_numbers_file, parse_stale_days, run};
pub use types::{
    TriageAction, TriageCheckFailure, TriageCiCheckStateCounts, TriageCiReadiness,
    TriageCiReadinessBuckets, TriageCommandOutput, TriageComponentReport, TriageIssueBucket,
    TriageIssueItem, TriageLandingCheckState, TriageLandingClassification,
    TriageLandingLocalWorktree, TriageLandingMergeabilityState, TriageLandingOptions,
    TriageLandingOutput, TriageLandingPr, TriageLandingRebasePlan, TriageLandingSummary,
    TriageLinkedPr, TriageObservationChangedItem, TriageObservationComparison,
    TriageObservationItemRef, TriageObservationOutput, TriageOptions, TriageOutput, TriagePrBucket,
    TriagePrItem, TriageRepo, TriageRepoRef, TriageSummary, TriageUnresolved,
};

pub use watch::{
    run as watch, TriageWatchEvent, TriageWatchItemState, TriageWatchOptions, TriageWatchOutput,
    TriageWatchTargetOutput,
};

// Internal re-exports so sibling submodules can continue to use `super::X` paths.
pub(super) use gh::{non_empty, run_gh, summarize_checks};
pub(super) use report::triage_command;

// Test-only re-exports consumed by `tests` via `use super::*;`. The glob pulls every
// `pub(super)` helper from each concern submodule into this module's namespace so the
// pre-split test suite keeps resolving the same bare function/type names.
#[cfg(test)]
mod test_reexports {
    pub(super) use chrono::{DateTime, Utc};
    pub(super) use serde_json::Value;

    pub(super) use crate::git::release_download::GitHubRepo;
    pub(super) use crate::observation::TriagePullRequestSignals;

    pub(super) use super::landing::{
        annotate_local_worktrees, annotate_ordered_dependent_rebases, branch_matches,
        classify_landing_pr, dedupe_landing_prs_preserving_order, dependent_rebase_plan,
        is_bare_pr_number, landing_check_state, landing_mergeability_state, parse_landing_pr,
        parse_landing_pr_ref, LandingPrRef,
    };
    pub(super) use super::report::{
        build_actions, dedupe_refs_by_repo, fetch_component_report, issue_bucket,
        parse_github_parent_repo, parse_issue, parse_issue_numbers, parse_issues, parse_linked_prs,
        parse_prs, resolve_and_dedupe_refs_with_parent_resolver, resolve_priority_labels,
        resolve_repo, resolve_repo_with_parent_resolver, resolve_target_components, run_batched,
        summarize, summarize_ci_readiness, summarize_unresolved, DEFAULT_PRIORITY_LABELS,
    };
    pub(super) use super::types::ComponentRef;
}

#[cfg(test)]
use crate::observation::{NewTriageItemRecord, TriageItemRecord};
#[cfg(test)]
use observation::{compare_triage_observations, triage_observation_metadata};
#[cfg(test)]
use test_reexports::*;

#[cfg(test)]
mod tests;
