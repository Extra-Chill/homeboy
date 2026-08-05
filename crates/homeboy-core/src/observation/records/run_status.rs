use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;

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
