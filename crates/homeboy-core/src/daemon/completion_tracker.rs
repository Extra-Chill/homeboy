//! Diff logic for the daemon's run-completion notifier.
//!
//! The local daemon polls the observation store for in-flight runs. Between
//! polls a run can leave the running set — it finished, failed, or was
//! reconciled to `stale`. [`CompletionTracker`] remembers which run ids were
//! running on the previous poll and reports the ones that have since departed,
//! so the daemon can fire exactly one completion notification per run instead
//! of re-notifying every poll.
//!
//! This is pure state-diff logic with no I/O, clock, or notifier coupling, so
//! it is deterministic and unit-testable. The daemon owns the polling cadence,
//! the store reads, and the notification dispatch around it.

use std::collections::BTreeSet;

/// Tracks the set of run ids observed running on the previous poll.
#[derive(Debug, Default)]
pub struct CompletionTracker {
    running: BTreeSet<String>,
}

impl CompletionTracker {
    /// Record the currently-running run ids and return the ids that completed
    /// since the last call (were running before, are not running now).
    ///
    /// The first call seeds the baseline and reports nothing: the daemon only
    /// pings for runs it actually observed in flight, never for runs that were
    /// already settled when it started watching. Returned ids are sorted and
    /// de-duplicated.
    pub fn observe<I>(&mut self, current_running: I) -> Vec<String>
    where
        I: IntoIterator<Item = String>,
    {
        let current: BTreeSet<String> = current_running.into_iter().collect();
        let completed: Vec<String> = self.running.difference(&current).cloned().collect();
        self.running = current;
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn first_observation_seeds_without_reporting_completions() {
        let mut tracker = CompletionTracker::default();
        let completed = tracker.observe(ids(&["a", "b"]));
        assert!(completed.is_empty());
        // The seeded ids are now tracked: completing them reports them.
        assert_eq!(tracker.observe(Vec::new()), ids(&["a", "b"]));
    }

    #[test]
    fn reports_runs_that_left_the_running_set() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b", "c"]));
        let completed = tracker.observe(ids(&["a"]));
        assert_eq!(completed, ids(&["b", "c"]));
    }

    #[test]
    fn newly_appearing_runs_are_tracked_then_reported_on_departure() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a"]));
        // `d` appears mid-flight; nothing completed yet.
        assert!(tracker.observe(ids(&["a", "d"])).is_empty());
        // `a` finishes; `d` still running.
        assert_eq!(tracker.observe(ids(&["d"])), ids(&["a"]));
        // `d` finishes.
        assert_eq!(tracker.observe(Vec::new()), ids(&["d"]));
    }

    #[test]
    fn a_run_is_reported_only_once() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a"]));
        assert_eq!(tracker.observe(Vec::new()), ids(&["a"]));
        // Subsequent polls without `a` running must not re-report it.
        assert!(tracker.observe(Vec::new()).is_empty());
    }

    #[test]
    fn duplicate_running_ids_collapse() {
        let mut tracker = CompletionTracker::default();
        let completed = tracker.observe(ids(&["a", "a", "b"]));
        assert!(completed.is_empty());
        // Despite the duplicate, each id is reported at most once on departure.
        assert_eq!(tracker.observe(Vec::new()), ids(&["a", "b"]));
    }
}
