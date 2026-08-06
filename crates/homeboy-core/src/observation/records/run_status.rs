use serde::Serialize;

/// Minutes a `Running` record may go without a heartbeat (`updated_at`) before
/// readers stop counting it as active work.
///
/// This is the *heartbeat liveness* threshold: it measures the age of the last
/// update a record reported, so it answers "has this run checked in recently?".
/// Old observation rows — runner executions and Cooks from hours or days
/// earlier — whose processes are gone must not inflate the `active`/`running`
/// totals operators rely on for cleanup and workload decisions (#9743). It also
/// catches Lab/offloaded runs whose runner process died silently, which would
/// otherwise leave a frozen `running` record trusted indefinitely (#5682).
///
/// A record with **no** `updated_at` at all is deliberately not stale by this
/// rule: absence of a timestamp is not evidence of death, and every reader here
/// leaves such a record for a liveness signal (owner pid, runner job) to judge.
///
/// Deliberately **separate** from [`OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES`]
/// despite sharing a value today. That one measures `started_at`, not
/// `updated_at`, and answers a different question. See its docs.
pub const RUNNING_HEARTBEAT_STALE_MINUTES: i64 = 30;

/// Minutes an *ownerless* `Running` run record stays credible, measured from
/// its `started_at`, before reconciliation may settle it to
/// [`RunStatus::Stale`].
///
/// This is a *grace period for missing provenance*, not a heartbeat measure.
/// It is only ever consulted after a record has been shown to carry no owner
/// pid at all, so a long-lived run with a live owner never reaches it. The
/// conjunction is the point: "ownerless" alone would strike down a run that has
/// only just started and not yet recorded its owner, and "old" alone would
/// strike down healthy long jobs.
///
/// Deliberately **separate** from [`RUNNING_HEARTBEAT_STALE_MINUTES`] despite
/// sharing a value today: a record can be freshly heartbeating and still
/// ownerless, or owned and silent. Tuning one must not move the other.
///
/// Runner-backed ownerless records get a far longer window instead of this one,
/// because a live remote job is authoritative; that exemption and its 24h
/// ceiling are documented at the reconcile call site (#11107).
pub const OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES: i64 = 30;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Pass,
    Fail,
    Error,
    Skipped,
    Stale,
    /// The locally observed work completed by handing execution to a remote
    /// runner job, which continues after this process exits.
    ///
    /// Terminal for *this* record and only this record. The controller-side
    /// work it observes — planning, materialization, dispatch — reached its own
    /// end successfully, and no further write to this run will ever happen,
    /// because the process that owned it is gone. The remote work is tracked by
    /// its own runner job id and durable run id, both recorded in the run's
    /// dispatch metadata alongside the commands that retrieve them.
    ///
    /// Distinct from `Pass` because a handoff proves the dispatch succeeded, not
    /// that the dispatched command did. Distinct from `Running` because leaving
    /// it open produces a phantom that nothing will ever close (#11107).
    HandedOff,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::Stale => "stale",
            Self::HandedOff => "handed_off",
        }
    }

    /// Parse a stored status label back into a [`RunStatus`].
    ///
    /// Returns `None` for labels Homeboy does not own, so callers can treat an
    /// unknown status conservatively rather than guessing terminality.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "running" => Some(Self::Running),
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "error" => Some(Self::Error),
            "skipped" => Some(Self::Skipped),
            "stale" => Some(Self::Stale),
            "handed_off" => Some(Self::HandedOff),
            _ => None,
        }
    }

    /// Whether the run has reached a terminal state. `Running` is the only
    /// non-terminal status; every other status — including `HandedOff` — means
    /// nothing further will be written to this record.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// Whether the record settled without the observed work being proven here.
    ///
    /// `HandedOff` is terminal but its outcome lives on a remote run, so a
    /// reader must be told where to look rather than reading terminality as a
    /// verdict.
    pub fn defers_outcome_to_a_remote_run(self) -> bool {
        matches!(self, Self::HandedOff)
    }
}

/// Pins that each running-staleness threshold is defined exactly once, so the
/// four-way drift that produced these constants cannot re-form.
#[cfg(test)]
#[path = "run_staleness_guard_test.rs"]
mod run_staleness_guard_test;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two thresholds are asserted **separately and never against each
    /// other**. They share a value today and are still two concepts: one
    /// measures `updated_at`, the other `started_at`. Asserting them equal
    /// would re-couple exactly what splitting them was meant to free, so this
    /// pins each value on its own — enough to catch an accidental change during
    /// a refactor, without making a deliberate tune of one a failure.
    #[test]
    fn the_shared_thresholds_hold_their_reconciled_values() {
        assert_eq!(RUNNING_HEARTBEAT_STALE_MINUTES, 30);
        assert_eq!(OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES, 30);
    }

    #[test]
    fn test_as_str() {
        assert_eq!(RunStatus::Running.as_str(), "running");
        assert_eq!(RunStatus::Pass.as_str(), "pass");
        assert_eq!(RunStatus::Fail.as_str(), "fail");
        assert_eq!(RunStatus::Error.as_str(), "error");
        assert_eq!(RunStatus::Skipped.as_str(), "skipped");
        assert_eq!(RunStatus::Stale.as_str(), "stale");
        assert_eq!(RunStatus::HandedOff.as_str(), "handed_off");
    }

    #[test]
    fn from_label_round_trips_known_statuses() {
        for status in [
            RunStatus::Running,
            RunStatus::Pass,
            RunStatus::Fail,
            RunStatus::Error,
            RunStatus::Skipped,
            RunStatus::Stale,
            RunStatus::HandedOff,
        ] {
            assert_eq!(RunStatus::from_label(status.as_str()), Some(status));
        }
        assert_eq!(RunStatus::from_label("something-else"), None);
    }

    #[test]
    fn only_running_is_non_terminal() {
        assert!(!RunStatus::Running.is_terminal());
        assert!(RunStatus::Pass.is_terminal());
        assert!(RunStatus::Fail.is_terminal());
        assert!(RunStatus::Error.is_terminal());
        assert!(RunStatus::Skipped.is_terminal());
        assert!(RunStatus::Stale.is_terminal());
        assert!(RunStatus::HandedOff.is_terminal());
    }

    /// A handoff is settled locally but proves nothing locally. Both halves of
    /// that statement have to be readable, or a consumer will pick one and be
    /// wrong: treat it as open (phantom) or treat it as a pass (false proof).
    #[test]
    fn handed_off_is_terminal_but_defers_its_outcome() {
        assert!(RunStatus::HandedOff.is_terminal());
        assert!(RunStatus::HandedOff.defers_outcome_to_a_remote_run());

        for status in [
            RunStatus::Running,
            RunStatus::Pass,
            RunStatus::Fail,
            RunStatus::Error,
            RunStatus::Skipped,
            RunStatus::Stale,
        ] {
            assert!(
                !status.defers_outcome_to_a_remote_run(),
                "{status:?} owns its own outcome"
            );
        }
    }
}
