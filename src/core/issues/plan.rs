//! Pure types for the reconcile contract.
//!
//! Every type here is plain data with no I/O. The [`reconcile`](super::reconcile)
//! function consumes them; [`apply_plan`](super::apply::apply_plan) turns the
//! resulting [`ReconcilePlan`] into tracker calls.

use serde::{Deserialize, Serialize};

use crate::core::code_audit::FindingConfidence;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};

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
    /// Optional confidence tier for this group. Audit callers can pass this
    /// through from the finding stream; when omitted, reconcile falls back to
    /// category-level defaults.
    #[serde(default)]
    pub confidence: Option<FindingConfidence>,
}

/// One issue from the tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
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
}

/// Configuration that affects reconcile decisions but not finding shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileConfig {
    /// When true, also refresh the body of closed-not_planned issues with
    /// the latest finding count + run link. This keeps the closed issue
    /// useful as a "current state" reference even though it stays closed.
    /// Default: true.
    #[serde(default = "default_refresh_closed")]
    pub refresh_closed_not_planned: bool,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            refresh_closed_not_planned: default_refresh_closed(),
        }
    }
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

/// Why the reconciler decided to skip a group. Surfaces in dry-run output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileSkipReason {
    /// A closed-not_planned issue and `refresh_closed_not_planned = false`.
    /// Less common.
    ClosedNotPlannedNoRefresh,
    /// No findings AND no existing open issue → nothing to do.
    NoFindingsNoIssue,
}

/// The full reconciliation plan: every action, in execution order.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcilePlan {
    #[serde(flatten)]
    pub plan: HomeboyPlan,
    pub actions: Vec<ReconcileAction>,
}

impl ReconcilePlan {
    pub fn new(component_id: impl Into<String>, actions: Vec<ReconcileAction>) -> Self {
        let component_id = component_id.into();
        let plan = HomeboyPlan::builder_for_component(PlanKind::IssueReconcile, component_id)
            .steps(actions.iter().enumerate().map(action_step))
            .summarize()
            .build();
        let actions = actions_from_plan(&plan);

        Self { plan, actions }
    }

    pub fn planned_actions(&self) -> Vec<ReconcileAction> {
        actions_from_plan(&self.plan)
    }

    /// Count actions by variant (file_new, update, etc.). Used by the CLI
    /// to render a one-line summary.
    pub fn counts(&self) -> ReconcilePlanCounts {
        let mut c = ReconcilePlanCounts::default();
        for action in self.planned_actions() {
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
        self.planned_actions()
            .into_iter()
            .all(|a| matches!(a, ReconcileAction::Skip { .. }))
    }
}

fn actions_from_plan(plan: &HomeboyPlan) -> Vec<ReconcileAction> {
    plan.steps
        .iter()
        .filter_map(|step| step.input_as("action"))
        .collect()
}

fn action_step((index, action): (usize, &ReconcileAction)) -> PlanStep {
    let action_kind = action_kind(action);
    let id = format!("issues.reconcile.{:03}.{}", index + 1, action_kind);
    let kind = format!("issues.reconcile.{action_kind}");
    let builder = PlanStep::builder(
        id,
        kind,
        if matches!(action, ReconcileAction::Skip { .. }) {
            PlanStepStatus::Skipped
        } else {
            PlanStepStatus::Ready
        },
    )
    .label(action_label(action))
    .blocking(!matches!(action, ReconcileAction::Skip { .. }))
    .inputs(PlanValues::new().json("action", action));

    match action {
        ReconcileAction::Skip { reason, .. } => builder.skip_reason(format!("{:?}", reason)),
        _ => builder,
    }
    .build()
}

fn action_kind(action: &ReconcileAction) -> &'static str {
    match action {
        ReconcileAction::FileNew { .. } => "file_new",
        ReconcileAction::Update { .. } => "update",
        ReconcileAction::UpdateClosed { .. } => "update_closed",
        ReconcileAction::Close { .. } => "close",
        ReconcileAction::CloseDuplicate { .. } => "close_duplicate",
        ReconcileAction::Skip { .. } => "skip",
    }
}

fn action_label(action: &ReconcileAction) -> String {
    match action {
        ReconcileAction::FileNew {
            command,
            component_id,
            category,
            count,
            ..
        } => format!("File new {command} issue for {category} in {component_id} ({count})"),
        ReconcileAction::Update {
            number,
            category,
            count,
            ..
        } => format!("Update {category} issue #{number} ({count})"),
        ReconcileAction::UpdateClosed {
            number,
            category,
            count,
            ..
        } => format!("Refresh closed {category} issue #{number} ({count})"),
        ReconcileAction::Close {
            number, category, ..
        } => {
            format!("Close resolved {category} issue #{number}")
        }
        ReconcileAction::CloseDuplicate {
            number,
            keep,
            category,
            ..
        } => format!("Close duplicate {category} issue #{number}, keeping #{keep}"),
        ReconcileAction::Skip {
            category, reason, ..
        } => format!("Skip {category} ({:?})", reason),
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

#[cfg(test)]
mod tests {
    use super::{action_kind, ReconcileAction, ReconcilePlan, ReconcileSkipReason};

    #[test]
    fn planned_actions_are_projected_from_homeboy_plan_steps() {
        let actions = vec![
            ReconcileAction::FileNew {
                command: "audit".to_string(),
                component_id: "homeboy".to_string(),
                category: "missing_method".to_string(),
                title: "audit: missing method".to_string(),
                body: "body".to_string(),
                labels: vec!["audit".to_string()],
                count: 2,
            },
            ReconcileAction::Skip {
                category: "resolved".to_string(),
                component_id: "homeboy".to_string(),
                reason: ReconcileSkipReason::NoFindingsNoIssue,
            },
        ];

        let plan = ReconcilePlan::new("homeboy", actions.clone());

        assert_eq!(plan.actions.len(), 2);
        assert_eq!(plan.planned_actions().len(), 2);
        assert_eq!(
            action_kind(&plan.planned_actions()[0]),
            action_kind(&actions[0])
        );
        assert_eq!(plan.counts().file_new, 1);
        assert_eq!(plan.counts().skip, 1);
        assert!(!plan.is_noop());
    }
}
