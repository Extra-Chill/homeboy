//! Plan executor: walk a [`ReconcilePlan`] and call the [`Tracker`].
//!
//! Pure separation: the plan is decided in [`reconcile`](super::reconcile),
//! the I/O is performed here. Each action's outcome is captured so dry-run
//! and apply runs share the same output shape (the plan + per-action result).

use serde::Serialize;

use crate::core::error::{Error, Result};

use super::plan::{ReconcileAction, ReconcilePlan, ReconcilePlanCounts};
use super::tracker::{CloseReason, Tracker};

/// Execute a [`ReconcilePlan`] against a [`Tracker`]. Returns a
/// [`ReconcileResult`] with the original plan plus per-action outcomes.
///
/// Failure semantics: each action is best-effort. A failed action is recorded
/// in the result and the run continues. The overall return is `Ok(...)` as
/// long as we produced a result; callers inspect `failed_count` to decide
/// whether to exit non-zero.
pub fn apply_plan(plan: ReconcilePlan, tracker: &dyn Tracker) -> Result<ReconcileResult> {
    let actions = plan.planned_actions();
    let mut executions: Vec<ReconcileExecution> = Vec::with_capacity(actions.len());

    for action in &actions {
        let exec = execute_action(action, tracker);
        executions.push(exec);
    }

    let counts = plan.counts();
    let failed_count = executions
        .iter()
        .filter(|e| matches!(e.outcome, ExecutionOutcome::Failed { .. }))
        .count();

    Ok(ReconcileResult {
        plan,
        executions,
        counts,
        failed_count,
    })
}

fn execute_action(action: &ReconcileAction, tracker: &dyn Tracker) -> ReconcileExecution {
    let summary = summary_for(action);
    let outcome = match action {
        ReconcileAction::FileNew {
            title,
            body,
            labels,
            ..
        } => match tracker.create_issue(title, body, labels) {
            Ok(number) => ExecutionOutcome::Filed { number },
            Err(e) => ExecutionOutcome::failed(&e),
        },
        ReconcileAction::Update {
            number,
            title,
            body,
            ..
        } => match tracker.update_issue(*number, Some(title), Some(body)) {
            Ok(()) => ExecutionOutcome::Updated { number: *number },
            Err(e) => ExecutionOutcome::failed(&e),
        },
        ReconcileAction::UpdateClosed { number, body, .. } => {
            match tracker.update_issue(*number, None, Some(body)) {
                Ok(()) => ExecutionOutcome::UpdatedClosed { number: *number },
                Err(e) => ExecutionOutcome::failed(&e),
            }
        }
        ReconcileAction::Close {
            number, comment, ..
        } => match tracker.close_issue(*number, CloseReason::Completed, Some(comment)) {
            Ok(()) => ExecutionOutcome::Closed { number: *number },
            Err(e) => ExecutionOutcome::failed(&e),
        },
        ReconcileAction::CloseDuplicate {
            number,
            keep,
            comment,
            ..
        } => match tracker.close_issue(*number, CloseReason::NotPlanned, Some(comment)) {
            Ok(()) => ExecutionOutcome::ClosedDuplicate {
                number: *number,
                keep: *keep,
            },
            Err(e) => ExecutionOutcome::failed(&e),
        },
        ReconcileAction::Skip { .. } => ExecutionOutcome::Skipped,
    };

    ReconcileExecution { summary, outcome }
}

fn summary_for(action: &ReconcileAction) -> String {
    match action {
        ReconcileAction::FileNew {
            command,
            component_id,
            category,
            count,
            ..
        } => format!(
            "file_new      {}: {} in {} ({})",
            command, category, component_id, count
        ),
        ReconcileAction::Update {
            number,
            category,
            count,
            ..
        } => format!("update        {} ({} → #{})", category, count, number),
        ReconcileAction::UpdateClosed {
            number,
            category,
            count,
            ..
        } => format!(
            "update_closed {} ({} → #{}) [stays closed]",
            category, count, number
        ),
        ReconcileAction::Close {
            number, category, ..
        } => format!("close         {} → #{}", category, number),
        ReconcileAction::CloseDuplicate {
            number,
            keep,
            category,
            ..
        } => format!(
            "dedupe        {} → keep #{} close #{}",
            category, keep, number
        ),
        ReconcileAction::Skip {
            category, reason, ..
        } => format!("skip          {} ({:?})", category, reason),
    }
}

/// Per-action outcome. Pairs with the original action for traceability.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileExecution {
    pub summary: String,
    pub outcome: ExecutionOutcome,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Filed { number: u64 },
    Updated { number: u64 },
    UpdatedClosed { number: u64 },
    Closed { number: u64 },
    ClosedDuplicate { number: u64, keep: u64 },
    Skipped,
    Failed { error: String },
}

impl ExecutionOutcome {
    fn failed(err: &Error) -> Self {
        ExecutionOutcome::Failed {
            error: err.to_string(),
        }
    }
}

/// Full reconcile output: plan + per-action results + summary counts.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileResult {
    pub plan: ReconcilePlan,
    pub executions: Vec<ReconcileExecution>,
    pub counts: ReconcilePlanCounts,
    pub failed_count: usize,
}

// ---------------------------------------------------------------------------
// Tests — apply_plan with a mock Tracker
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::issues::plan::{ReconcileAction, ReconcileSkipReason};
    use std::cell::RefCell;

    /// Mock tracker: records every call, returns canned IDs for create_issue.
    /// Each operation can be set to fail by toggling the matching flag.
    struct MockTracker {
        calls: RefCell<Vec<String>>,
        fail_create: bool,
        fail_close: bool,
    }

    impl MockTracker {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_create: false,
                fail_close: false,
            }
        }
    }

    impl Tracker for MockTracker {
        fn list_issues(
            &self,
            _label: &str,
            _limit: usize,
        ) -> Result<Vec<crate::core::issues::TrackedIssue>> {
            unimplemented!("apply_plan does not call list_issues")
        }
        fn create_issue(&self, title: &str, _body: &str, _labels: &[String]) -> Result<u64> {
            self.calls.borrow_mut().push(format!("create:{}", title));
            if self.fail_create {
                Err(Error::internal_io("create failed", None))
            } else {
                Ok(42)
            }
        }
        fn update_issue(
            &self,
            number: u64,
            title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("update:#{}:{}", number, title.unwrap_or("-")));
            Ok(())
        }
        fn close_issue(
            &self,
            number: u64,
            reason: CloseReason,
            _comment: Option<&str>,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("close:#{}:{:?}", number, reason));
            if self.fail_close {
                Err(Error::internal_io("close failed", None))
            } else {
                Ok(())
            }
        }
    }

    fn file_new(category: &str) -> ReconcileAction {
        ReconcileAction::FileNew {
            command: "audit".into(),
            component_id: "c".into(),
            category: category.into(),
            title: format!("audit: {} in c (5)", category),
            body: "body".into(),
            labels: vec!["audit".into()],
            count: 5,
        }
    }

    #[test]
    fn applies_file_new_via_tracker() {
        let plan = ReconcilePlan::new("c", vec![file_new("x")]);
        let tracker = MockTracker::new();
        let result = apply_plan(plan, &tracker).unwrap();

        assert_eq!(result.executions.len(), 1);
        assert!(matches!(
            result.executions[0].outcome,
            ExecutionOutcome::Filed { number: 42 }
        ));
        assert_eq!(result.failed_count, 0);
        assert_eq!(tracker.calls.borrow().len(), 1);
    }

    #[test]
    fn applies_update_close_dedupe_in_order() {
        let plan = ReconcilePlan::new(
            "c",
            vec![
                ReconcileAction::Update {
                    number: 100,
                    title: "audit: x in c (3)".into(),
                    body: "b".into(),
                    category: "x".into(),
                    count: 3,
                },
                ReconcileAction::Close {
                    number: 200,
                    category: "y".into(),
                    comment: "resolved".into(),
                },
                ReconcileAction::CloseDuplicate {
                    number: 300,
                    keep: 100,
                    category: "x".into(),
                    comment: "dupe of #100".into(),
                },
            ],
        );
        let tracker = MockTracker::new();
        let result = apply_plan(plan, &tracker).unwrap();

        assert_eq!(result.executions.len(), 3);
        assert!(matches!(
            result.executions[0].outcome,
            ExecutionOutcome::Updated { number: 100 }
        ));
        assert!(matches!(
            result.executions[1].outcome,
            ExecutionOutcome::Closed { number: 200 }
        ));
        assert!(matches!(
            result.executions[2].outcome,
            ExecutionOutcome::ClosedDuplicate {
                number: 300,
                keep: 100,
            }
        ));

        let calls = tracker.calls.borrow();
        assert_eq!(calls[0], "update:#100:audit: x in c (3)");
        assert_eq!(calls[1], "close:#200:Completed");
        assert_eq!(calls[2], "close:#300:NotPlanned");
    }

    #[test]
    fn skip_actions_make_no_tracker_calls() {
        let plan = ReconcilePlan::new(
            "c",
            vec![
                ReconcileAction::Skip {
                    category: "x".into(),
                    component_id: "c".into(),
                    reason: ReconcileSkipReason::NoFindingsNoIssue,
                },
                ReconcileAction::Skip {
                    category: "y".into(),
                    component_id: "c".into(),
                    reason: ReconcileSkipReason::ClosedNotPlannedNoRefresh,
                },
            ],
        );
        let tracker = MockTracker::new();
        let result = apply_plan(plan, &tracker).unwrap();

        assert!(tracker.calls.borrow().is_empty());
        assert!(matches!(
            result.executions[0].outcome,
            ExecutionOutcome::Skipped
        ));
        assert!(matches!(
            result.executions[1].outcome,
            ExecutionOutcome::Skipped
        ));
        assert_eq!(result.failed_count, 0);
    }

    #[test]
    fn failed_actions_recorded_but_run_continues() {
        let plan = ReconcilePlan::new(
            "c",
            vec![
                file_new("a"),
                ReconcileAction::Close {
                    number: 1,
                    category: "b".into(),
                    comment: "c".into(),
                },
                file_new("c"),
            ],
        );
        let mut tracker = MockTracker::new();
        tracker.fail_close = true;

        let result = apply_plan(plan, &tracker).unwrap();
        assert_eq!(result.executions.len(), 3);
        assert_eq!(result.failed_count, 1);
        // First and third (FileNew) succeed; second (Close) fails.
        assert!(matches!(
            result.executions[0].outcome,
            ExecutionOutcome::Filed { .. }
        ));
        assert!(matches!(
            result.executions[1].outcome,
            ExecutionOutcome::Failed { .. }
        ));
        assert!(matches!(
            result.executions[2].outcome,
            ExecutionOutcome::Filed { .. }
        ));
    }

    #[test]
    fn update_closed_passes_no_title_to_tracker() {
        // UpdateClosed should refresh body only — the title carrying
        // an outdated count is fine because the issue is closed and the
        // body holds the latest count + run link.
        let plan = ReconcilePlan::new(
            "c",
            vec![ReconcileAction::UpdateClosed {
                number: 50,
                body: "fresh".into(),
                category: "x".into(),
                count: 99,
            }],
        );
        let tracker = MockTracker::new();
        apply_plan(plan, &tracker).unwrap();
        assert_eq!(tracker.calls.borrow()[0], "update:#50:-");
    }
}
