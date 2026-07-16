use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Pass,
    Fail,
    Error,
    Skipped,
    Stale,
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
            _ => None,
        }
    }

    /// Whether the run has reached a terminal state. `Running` is the only
    /// non-terminal status; every other status means the run is settled.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
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
    }
}
