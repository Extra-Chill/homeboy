//! What a repair driver should do next.
//!
//! Separated from [`super::repair`], which talks to runners, so the decision is
//! a pure function of a doctor report plus what has already been tried. The
//! interesting failures of a repair loop -- looping forever, re-running an
//! action that did nothing, stopping while a fixable check is still red -- are
//! all decisions, and none of them need a runner to test.
//!
//! # Why a loop is needed at all
//!
//! `repair::apply` runs once. One branch re-probes by hand -- it drops the
//! `daemon.exec` check and re-runs that single probe -- but there is no general
//! re-probe, and several paths `return` immediately after acting. So nothing
//! observes whether a repair worked, and a second fault behind the first is
//! left for the operator.
//!
//! That is why `doctor` appears three times in
//! `docs/workflows/set-up-lab-runners.md`: the operator is the loop (#13551).
//!
//! # Termination
//!
//! Three ways to stop, and they are deliberately distinct in the output. An
//! operator needs to know whether the runner is healthy, whether homeboy ran out
//! of attempts, or whether it applied a fix that changed nothing -- the last is
//! the one that means a human is required, and reporting it as "exhausted"
//! would send them to raise a budget that is not the problem.

use serde::Serialize;

use super::types::{RunnerCheck, RunnerDoctorStatus, RunnerRepairAction};

/// The default attempt ceiling.
///
/// Small on purpose. Each attempt is a network round trip plus a full re-probe,
/// and a runner needing more than a handful of distinct repairs is not a
/// convergence problem -- it is a runner an operator should look at.
pub(crate) const DEFAULT_MAX_ATTEMPTS: usize = 4;

/// The driver's next move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RepairStep {
    /// Run this action, then re-probe.
    Apply { action: RunnerRepairAction },
    /// No check that is failing carries an automatic repair. This is success
    /// *for the loop*, not necessarily a healthy runner: checks needing a human
    /// decision carry no action and land here.
    Converged,
    /// The attempt ceiling was reached with work still outstanding.
    Exhausted { attempts: usize },
    /// An action was applied and its check is still failing. Re-running it
    /// would not help, and this is the state that actually needs a person.
    Ineffective { action: RunnerRepairAction },
}

/// Actions offered by checks that are currently failing.
///
/// `Ok` checks are skipped even when they carry an action, so a repair is never
/// run against something already healthy. Order follows the report, so the
/// driver applies fixes in the order the probes found them; duplicates are
/// dropped because two checks can legitimately want the same repair and running
/// it twice in one pass would waste an attempt on the second.
fn outstanding(checks: &[RunnerCheck]) -> Vec<RunnerRepairAction> {
    let mut seen: Vec<RunnerRepairAction> = Vec::new();
    for check in checks {
        if check.status == RunnerDoctorStatus::Ok {
            continue;
        }
        let Some(action) = check.remediation_action.clone() else {
            continue;
        };
        if !seen.contains(&action) {
            seen.push(action);
        }
    }
    seen
}

/// Decide the next step from the current report and the attempts already made.
///
/// `history` is every action applied so far, in order.
pub(crate) fn next_step(
    checks: &[RunnerCheck],
    history: &[RunnerRepairAction],
    max_attempts: usize,
) -> RepairStep {
    let outstanding = outstanding(checks);
    let Some(action) = outstanding.into_iter().next() else {
        return RepairStep::Converged;
    };

    // Ineffectiveness is checked before the budget. A repair that already ran
    // and left its check failing is a human's problem regardless of how many
    // attempts remain, and reporting it as `Exhausted` would point the operator
    // at a ceiling that is not what stopped them.
    if history.contains(&action) {
        return RepairStep::Ineffective { action };
    }

    if history.len() >= max_attempts {
        return RepairStep::Exhausted {
            attempts: history.len(),
        };
    }

    RepairStep::Apply { action }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn check(
        id: &str,
        status: RunnerDoctorStatus,
        action: Option<RunnerRepairAction>,
    ) -> RunnerCheck {
        RunnerCheck {
            id: id.to_string(),
            status,
            message: String::new(),
            remediation: None,
            remediation_action: action,
            details: BTreeMap::new(),
        }
    }

    fn refresh() -> RunnerRepairAction {
        RunnerRepairAction::RefreshHomeboy {
            git_ref: Some("abc".to_string()),
            allow_downgrade: false,
        }
    }

    #[test]
    fn the_wire_vocabulary_is_pinned() {
        let step = |s| serde_json::to_value(s).expect("serialize");
        assert_eq!(step(RepairStep::Converged)["step"], "converged");
        assert_eq!(
            step(RepairStep::Exhausted { attempts: 1 })["step"],
            "exhausted"
        );
        assert_eq!(
            step(RepairStep::Apply { action: refresh() })["step"],
            "apply"
        );
        assert_eq!(
            step(RepairStep::Ineffective { action: refresh() })["step"],
            "ineffective"
        );
    }

    #[test]
    fn nothing_failing_is_converged() {
        let checks = vec![check("a", RunnerDoctorStatus::Ok, Some(refresh()))];
        assert_eq!(next_step(&checks, &[], 4), RepairStep::Converged);
    }

    /// A failing check with no automatic repair is convergence for the loop, not
    /// an error. Plenty of checks are fixed by a decision homeboy should not
    /// make, and spinning on them would be worse than stopping.
    #[test]
    fn a_failing_check_without_an_action_is_converged() {
        let checks = vec![check("a", RunnerDoctorStatus::Error, None)];
        assert_eq!(next_step(&checks, &[], 4), RepairStep::Converged);
    }

    /// An `Ok` check must never have its action run. Doctor emits actions on
    /// healthy checks in some paths, and repairing something that is working is
    /// the most expensive possible way to waste an attempt.
    #[test]
    fn an_ok_check_never_offers_its_action() {
        let checks = vec![
            check("healthy", RunnerDoctorStatus::Ok, Some(refresh())),
            check("broken", RunnerDoctorStatus::Error, None),
        ];
        assert_eq!(next_step(&checks, &[], 4), RepairStep::Converged);
    }

    #[test]
    fn a_failing_check_with_an_action_is_applied() {
        let checks = vec![check("a", RunnerDoctorStatus::Warning, Some(refresh()))];
        assert_eq!(
            next_step(&checks, &[], 4),
            RepairStep::Apply { action: refresh() }
        );
    }

    /// The property that stops an infinite loop: an action that already ran and
    /// left its check failing is reported as ineffective rather than retried.
    #[test]
    fn an_action_that_already_ran_and_did_not_help_is_ineffective() {
        let checks = vec![check("a", RunnerDoctorStatus::Error, Some(refresh()))];
        assert_eq!(
            next_step(&checks, &[refresh()], 4),
            RepairStep::Ineffective { action: refresh() }
        );
    }

    /// Ineffectiveness outranks the budget. Both stop the loop, but they tell
    /// the operator to do different things, and "raise the ceiling" is the wrong
    /// advice when the repair simply does not work.
    #[test]
    fn ineffective_is_reported_even_with_no_budget_left() {
        let checks = vec![check("a", RunnerDoctorStatus::Error, Some(refresh()))];
        assert_eq!(
            next_step(&checks, &[refresh(), refresh()], 1),
            RepairStep::Ineffective { action: refresh() }
        );
    }

    #[test]
    fn a_distinct_action_past_the_ceiling_is_exhausted() {
        let checks = vec![check("a", RunnerDoctorStatus::Error, Some(refresh()))];
        let history = vec![RunnerRepairAction::Reconnect; 4];
        assert_eq!(
            next_step(&checks, &history, 4),
            RepairStep::Exhausted { attempts: 4 }
        );
    }

    /// Two checks wanting the same repair must not consume two attempts.
    #[test]
    fn one_repair_wanted_by_two_checks_is_offered_once() {
        let checks = vec![
            check("a", RunnerDoctorStatus::Error, Some(refresh())),
            check("b", RunnerDoctorStatus::Error, Some(refresh())),
        ];
        assert_eq!(outstanding(&checks).len(), 1);
    }

    /// Distinct repairs are applied in the order the probes reported them,
    /// which keeps a run reproducible.
    #[test]
    fn distinct_repairs_are_offered_in_report_order() {
        let checks = vec![
            check(
                "a",
                RunnerDoctorStatus::Error,
                Some(RunnerRepairAction::Reconnect),
            ),
            check("b", RunnerDoctorStatus::Error, Some(refresh())),
        ];
        assert_eq!(
            outstanding(&checks),
            vec![RunnerRepairAction::Reconnect, refresh()]
        );
        assert_eq!(
            next_step(&checks, &[], 4),
            RepairStep::Apply {
                action: RunnerRepairAction::Reconnect
            }
        );
    }

    /// A full run: two distinct faults, each repaired once, then converged.
    #[test]
    fn a_two_fault_run_converges() {
        let mut history: Vec<RunnerRepairAction> = Vec::new();
        let mut checks = vec![
            check(
                "a",
                RunnerDoctorStatus::Error,
                Some(RunnerRepairAction::Reconnect),
            ),
            check("b", RunnerDoctorStatus::Error, Some(refresh())),
        ];
        for _ in 0..DEFAULT_MAX_ATTEMPTS {
            match next_step(&checks, &history, DEFAULT_MAX_ATTEMPTS) {
                RepairStep::Apply { action } => {
                    // Applying a repair clears the check that asked for it.
                    checks.retain(|c| c.remediation_action.as_ref() != Some(&action));
                    history.push(action);
                }
                other => {
                    assert_eq!(other, RepairStep::Converged);
                    assert_eq!(history.len(), 2, "each fault repaired exactly once");
                    return;
                }
            }
        }
        panic!("loop did not converge within the attempt ceiling");
    }
}
