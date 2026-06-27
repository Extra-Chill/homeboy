//! Shared GitHub status-check classification.
//!
//! `gh pr view --json statusCheckRollup` returns one entry per check run. Three
//! readiness surfaces interpret those entries: the readiness summary
//! ([`super::readiness::classify_pr_ci`]), the fleet rollup
//! ([`super::fleet`]), and the PR-land merge gate
//! ([`crate::core::git::pr_land`]). They previously each re-matched
//! `(status, conclusion)` with subtly different groupings — most visibly, the
//! fleet rollup folded `SKIPPED` into its passed count while the readiness
//! summary tracked it separately. This module is the single place that maps one
//! check entry to a [`CheckClass`], so every surface agrees on what each
//! outcome means.

use serde_json::Value;

/// Terminal/transient classification of a single GitHub status check.
///
/// `SKIPPED` is modeled as its own terminal, **non-blocking** outcome
/// ([`CheckClass::Skipped`]): it never counts as a failure and never counts as a
/// pending check, but it is tracked separately from [`CheckClass::Passed`] so
/// reports can distinguish "ran and passed" from "skipped". The non-terminal
/// state is split into [`CheckClass::Queued`], [`CheckClass::Running`], and
/// [`CheckClass::Pending`] so the readiness summary can describe *why* a check
/// has not settled; callers that only need a binary terminal/pending signal
/// collapse the three via [`CheckClass::is_waiting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core::git) enum CheckClass {
    /// Completed successfully (`SUCCESS`/`NEUTRAL`).
    Passed,
    /// Completed as `SKIPPED` — terminal and non-blocking.
    Skipped,
    /// Completed with a hard failure (`FAILURE`/`ACTION_REQUIRED`).
    Failed,
    /// Completed with a transient failure worth re-running
    /// (`CANCELLED`/`TIMED_OUT`/`STARTUP_FAILURE`).
    Rerunnable,
    /// Completed with an unrecognized conclusion, or `COMPLETED` with no
    /// conclusion at all — blocks merge until a human inspects it.
    Unknown,
    /// Not started yet (`QUEUED`/`REQUESTED`/`WAITING`).
    Queued,
    /// Currently executing (`IN_PROGRESS`).
    Running,
    /// Reported but not yet terminal in any recognized state.
    Pending,
}

impl CheckClass {
    /// True for terminal failure-shaped outcomes (hard or rerunnable) plus
    /// `Unknown`, which blocks merge until a human inspects it.
    pub(in crate::core::git) fn is_blocking(self) -> bool {
        matches!(
            self,
            CheckClass::Failed | CheckClass::Rerunnable | CheckClass::Unknown
        )
    }

    /// True while the check has not reached a terminal state.
    pub(in crate::core::git) fn is_waiting(self) -> bool {
        matches!(
            self,
            CheckClass::Queued | CheckClass::Running | CheckClass::Pending
        )
    }
}

/// Classify one `statusCheckRollup` entry by its `status` + `conclusion`.
///
/// Both fields are upper-cased before matching: GitHub's GraphQL enums are
/// upper-case in practice, and normalizing keeps the classifier robust to any
/// lower-cased payloads the previous hand-rolled classifiers tolerated.
pub(in crate::core::git) fn classify_check(check: &Value) -> CheckClass {
    let status = upper_field(check, "status");
    let conclusion = upper_field(check, "conclusion");
    match (status.as_deref(), conclusion.as_deref()) {
        (_, Some("FAILURE" | "ACTION_REQUIRED")) => CheckClass::Failed,
        (_, Some("CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE")) => CheckClass::Rerunnable,
        (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL")) => CheckClass::Passed,
        (Some("COMPLETED"), Some("SKIPPED")) => CheckClass::Skipped,
        (Some("COMPLETED"), Some(_) | None) => CheckClass::Unknown,
        (Some("QUEUED" | "REQUESTED" | "WAITING"), _) => CheckClass::Queued,
        (Some("IN_PROGRESS"), _) => CheckClass::Running,
        _ => CheckClass::Pending,
    }
}

fn upper_field(check: &Value, key: &str) -> Option<String> {
    check
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: &str, conclusion: &str) -> Value {
        serde_json::json!({ "status": status, "conclusion": conclusion })
    }

    #[test]
    fn classifies_terminal_outcomes() {
        assert_eq!(classify_check(&check("COMPLETED", "SUCCESS")), CheckClass::Passed);
        assert_eq!(classify_check(&check("COMPLETED", "NEUTRAL")), CheckClass::Passed);
        assert_eq!(classify_check(&check("COMPLETED", "SKIPPED")), CheckClass::Skipped);
        assert_eq!(classify_check(&check("COMPLETED", "FAILURE")), CheckClass::Failed);
        assert_eq!(
            classify_check(&check("COMPLETED", "ACTION_REQUIRED")),
            CheckClass::Failed
        );
        assert_eq!(
            classify_check(&check("COMPLETED", "CANCELLED")),
            CheckClass::Rerunnable
        );
        assert_eq!(
            classify_check(&check("COMPLETED", "TIMED_OUT")),
            CheckClass::Rerunnable
        );
        assert_eq!(classify_check(&check("COMPLETED", "BOGUS")), CheckClass::Unknown);
    }

    #[test]
    fn classifies_waiting_outcomes() {
        assert_eq!(classify_check(&check("QUEUED", "")), CheckClass::Queued);
        assert_eq!(classify_check(&check("REQUESTED", "")), CheckClass::Queued);
        assert_eq!(classify_check(&check("IN_PROGRESS", "")), CheckClass::Running);
        assert_eq!(classify_check(&check("PENDING", "")), CheckClass::Pending);
        assert_eq!(
            classify_check(&serde_json::json!({})),
            CheckClass::Pending
        );
    }

    #[test]
    fn completed_without_conclusion_is_unknown() {
        assert_eq!(
            classify_check(&serde_json::json!({ "status": "COMPLETED" })),
            CheckClass::Unknown
        );
    }

    #[test]
    fn skipped_is_terminal_and_non_blocking() {
        let skipped = classify_check(&check("COMPLETED", "SKIPPED"));
        assert!(!skipped.is_blocking());
        assert!(!skipped.is_waiting());
    }

    #[test]
    fn matches_are_case_insensitive() {
        assert_eq!(classify_check(&check("completed", "failure")), CheckClass::Failed);
        assert_eq!(classify_check(&check("in_progress", "")), CheckClass::Running);
    }

    #[test]
    fn blocking_and_waiting_partition_failure_and_pending() {
        assert!(CheckClass::Failed.is_blocking());
        assert!(CheckClass::Rerunnable.is_blocking());
        assert!(CheckClass::Unknown.is_blocking());
        assert!(!CheckClass::Passed.is_blocking());
        assert!(CheckClass::Queued.is_waiting());
        assert!(CheckClass::Running.is_waiting());
        assert!(CheckClass::Pending.is_waiting());
        assert!(!CheckClass::Passed.is_waiting());
    }
}
