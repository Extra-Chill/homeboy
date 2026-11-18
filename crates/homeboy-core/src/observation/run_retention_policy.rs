//! Per-kind age floors for terminal-run retention.
//!
//! High-volume internal run kinds (one row per CLI invocation, cleanup pass or
//! daemon pass) have no value after a couple of days, while user-facing kinds
//! (reviews, agent tasks, releases, ...) keep the configured window. This is
//! the single table that says which kinds age out early; nothing else should
//! name these kinds for retention purposes.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// Age floor, in days, for the high-volume internal run kinds.
pub const INTERNAL_RUN_KIND_AGE_DAYS: i64 = 2;

/// Run kinds that age out on their own floor instead of the configured
/// terminal-run window. A floor never extends retention past the configured
/// default; it only shortens it.
pub const TERMINAL_RUN_KIND_AGE_FLOORS: &[(&str, i64)] = &[
    ("cli-invocation", INTERNAL_RUN_KIND_AGE_DAYS),
    ("cleanup", INTERNAL_RUN_KIND_AGE_DAYS),
    ("cleanup.artifacts", INTERNAL_RUN_KIND_AGE_DAYS),
    ("daemon-pass", INTERNAL_RUN_KIND_AGE_DAYS),
];

/// The effective age window per run kind for one retention pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TerminalRunAgePolicy {
    /// Window for every kind without its own floor (configured
    /// `retention.terminal_run_days`, or the operator override).
    pub default_days: i64,
    /// `true` when an operator passed `--older-than-days`; that one window
    /// then applies to every kind and the per-kind floors are ignored.
    pub operator_override: bool,
}

/// Resolved `finished_at` cutoffs for one retention pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRunCutoffs {
    /// Cutoff for every kind not listed in `per_kind`.
    pub default_before: String,
    /// Kinds with their own cutoff.
    pub per_kind: Vec<(String, String)>,
}

impl TerminalRunAgePolicy {
    /// Policy using the configured window plus the per-kind floors.
    pub fn configured(default_days: i64) -> Self {
        Self {
            default_days,
            operator_override: false,
        }
    }

    /// Policy for a resolved window: an explicit operator `--older-than-days`
    /// wins for every kind, otherwise the configured window plus floors.
    pub fn resolve(configured_days: i64, operator_days: Option<i64>) -> Self {
        match operator_days {
            Some(days) => Self {
                default_days: days,
                operator_override: true,
            },
            None => Self::configured(configured_days),
        }
    }

    /// Age window, in days, for one run kind.
    pub fn days_for_kind(&self, kind: &str) -> i64 {
        if self.operator_override {
            return self.default_days;
        }
        TERMINAL_RUN_KIND_AGE_FLOORS
            .iter()
            .find(|(floor_kind, _)| *floor_kind == kind)
            .map(|(_, days)| (*days).min(self.default_days))
            .unwrap_or(self.default_days)
    }

    /// `finished_at` cutoffs relative to `now`.
    pub fn cutoffs(&self, now: DateTime<Utc>) -> TerminalRunCutoffs {
        let before = |days: i64| (now - Duration::days(days)).to_rfc3339();
        let per_kind = if self.operator_override {
            Vec::new()
        } else {
            TERMINAL_RUN_KIND_AGE_FLOORS
                .iter()
                .map(|(kind, _)| (kind.to_string(), before(self.days_for_kind(kind))))
                .collect()
        };
        TerminalRunCutoffs {
            default_before: before(self.default_days),
            per_kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_kinds_use_floor_and_override_wins_for_every_kind() {
        let policy = TerminalRunAgePolicy::configured(30);
        assert_eq!(policy.days_for_kind("cli-invocation"), 2);
        assert_eq!(policy.days_for_kind("review"), 30);
        // A floor never extends retention past the configured window.
        assert_eq!(
            TerminalRunAgePolicy::configured(1).days_for_kind("cleanup"),
            1
        );
        let overridden = TerminalRunAgePolicy::resolve(30, Some(7));
        assert_eq!(overridden.days_for_kind("cli-invocation"), 7);
        assert_eq!(overridden.days_for_kind("review"), 7);
        assert!(overridden.cutoffs(Utc::now()).per_kind.is_empty());
    }
}
