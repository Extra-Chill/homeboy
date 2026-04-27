//! Pure types for the reconcile contract.
//!
//! Every type here is plain data with no I/O. The [`reconcile`](super::reconcile)
//! function consumes them; [`apply_plan`](super::apply::apply_plan) turns the
//! resulting [`ReconcilePlan`] into tracker calls.

use serde::{Deserialize, Serialize};

/// One row of incoming findings: "command produced N findings of category X
/// for component Y." This is the input grain reconcile reasons over.
///
/// `command` is `"audit" | "lint" | "test"` etc. — used for label-scoping
/// the tracker query (e.g. only consider open issues labeled `audit`) and
/// for the issue title prefix.
///
/// `category` is the kind/key (e.g. `unreferenced_export`, `god_file`,
/// `missing_test_method`). Empty findings counts (`count = 0`) for a category
/// that previously had open issues drive the close-on-resolved transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueGroup {
    pub command: String,
    pub component_id: String,
    pub category: String,
    pub count: usize,
    /// Human-friendly category label (e.g. `unreferenced export` for
    /// `unreferenced_export`). Used in issue titles. Empty falls back to a
    /// straight `category.replace('_', ' ')` rendering at title time.
    #[serde(default)]
    pub label: String,
    /// Pre-rendered body for new issues. The reconciler does NOT generate
    /// finding tables — the action / caller renders them once and passes
    /// them in. Empty falls back to a minimal "<count> findings" body.
    #[serde(default)]
    pub body: String,
    /// Whether this category is eligible for tracker issue filing by default.
    /// Heuristic audit categories are review-only unless callers explicitly
    /// opt the group into actionable routing.
    #[serde(default)]
    pub routing: IssueGroupRouting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueGroupRouting {
    Actionable,
    ReviewOnly,
}

impl Default for IssueGroupRouting {
    fn default() -> Self {
        Self::Actionable
    }
}

/// One issue from the tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedIssue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: TrackedIssueState,
    pub labels: Vec<String>,
}

/// Tracker-agnostic issue state. Maps directly onto GitHub's
/// `state` + `stateReason` pair; future trackers (GitLab, Linear) implement
/// their analogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackedIssueState {
    Open,
    /// "Resolved." Findings re-appearing → file a new issue (the original
    /// problem returned). GitHub: `state=closed, state_reason=completed`.
    ClosedCompleted,
    /// "We have decided not to fix this / this is a false positive."
    /// Findings re-appearing → refresh the closed issue body, do NOT
    /// re-file. GitHub: `state=closed, state_reason=not_planned`.
    ClosedNotPlanned,
}

impl TrackedIssueState {
    pub fn is_open(self) -> bool {
        matches!(self, TrackedIssueState::Open)
    }
    pub fn is_closed(self) -> bool {
        !self.is_open()
    }
}

/// Configuration that affects reconcile decisions but not finding shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileConfig {
    /// Categories whose findings are unconditionally muted. No new issue is
    /// filed; existing OPEN issues for these categories are left alone.
    /// Existing CLOSED issues stay closed. Sourced from `homeboy.json`'s
    /// `audit.suppressed_categories` (or a CLI override).
    #[serde(default)]
    pub suppressed_categories: Vec<String>,

    /// Labels that, when present on a closed issue, suppress re-filing the
    /// same category. Defaults to `["wontfix", "upstream-bug",
    /// "audit-suppressed"]`. Sourced from `homeboy.json`'s
    /// `issues.suppression_labels`.
    #[serde(default)]
    pub suppression_labels: Vec<String>,

    /// When true, also refresh the body of closed-not_planned issues with
    /// the latest finding count + run link. This keeps the closed issue
    /// useful as a "current state" reference even though it stays closed.
    /// Default: true.
    #[serde(default = "default_refresh_closed")]
    pub refresh_closed_not_planned: bool,
}

fn default_refresh_closed() -> bool {
    true
}

/// One concrete action the reconciler decided on. Order matters in the plan:
/// dedupe-closes run before file-new so race-condition duplicates don't
/// inflate the new-issue count.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReconcileAction {
    /// File a new issue.
    FileNew {
        command: String,
        component_id: String,
        category: String,
        title: String,
        body: String,
        labels: Vec<String>,
        count: usize,
    },
    /// Update an existing OPEN issue's title + body to reflect latest count.
    Update {
        number: u64,
        title: String,
        body: String,
        category: String,
        count: usize,
    },
    /// Refresh a closed-not_planned issue's body. Stays closed.
    UpdateClosed {
        number: u64,
        body: String,
        category: String,
        count: usize,
    },
    /// Close an issue whose findings dropped to zero. Reason is always
    /// `completed` for this action — caller intent is "the underlying
    /// problem was resolved."
    Close {
        number: u64,
        category: String,
        comment: String,
    },
    /// Close a duplicate of another open issue for the same category.
    /// Reason is always `not_planned` — caller intent is "this is the same
    /// thing as #N." Caller keeps the lowest-numbered match.
    CloseDuplicate {
        number: u64,
        keep: u64,
        category: String,
        comment: String,
    },
    /// Skip this group. Diagnostic — never produces a tracker call.
    Skip {
        category: String,
        component_id: String,
        reason: ReconcileSkipReason,
    },
}

/// Why the reconciler decided to skip a group. Surfaces in dry-run output
/// so the user can see whether suppression came from config, label, or
/// close-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileSkipReason {
    /// Category was in `suppressed_categories`.
    SuppressedByConfig,
    /// A closed-not_planned issue carried a `suppression_labels` label.
    SuppressedByLabel,
    /// A closed-not_planned issue without a suppression label, AND
    /// `refresh_closed_not_planned = false`. Less common.
    ClosedNotPlannedNoRefresh,
    /// No findings AND no existing open issue → nothing to do.
    NoFindingsNoIssue,
    /// Findings are still visible in reports, but this category defaults to
    /// review-only routing and should not create or refresh tracker issues.
    ReviewOnlyDefault,
}

/// The full reconciliation plan: every action, in execution order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcilePlan {
    pub actions: Vec<ReconcileAction>,
}

impl ReconcilePlan {
    /// Count actions by variant (file_new, update, etc.). Used by the CLI
    /// to render a one-line summary.
    pub fn counts(&self) -> ReconcilePlanCounts {
        let mut c = ReconcilePlanCounts::default();
        for action in &self.actions {
            match action {
                ReconcileAction::FileNew { .. } => c.file_new += 1,
                ReconcileAction::Update { .. } => c.update += 1,
                ReconcileAction::UpdateClosed { .. } => c.update_closed += 1,
                ReconcileAction::Close { .. } => c.close += 1,
                ReconcileAction::CloseDuplicate { .. } => c.close_duplicate += 1,
                ReconcileAction::Skip { .. } => c.skip += 1,
            }
        }
        c
    }

    pub fn is_noop(&self) -> bool {
        self.actions
            .iter()
            .all(|a| matches!(a, ReconcileAction::Skip { .. }))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReconcilePlanCounts {
    pub file_new: usize,
    pub update: usize,
    pub update_closed: usize,
    pub close: usize,
    pub close_duplicate: usize,
    pub skip: usize,
}
