//! Diff logic for the daemon's run-completion notifier.
//!
//! The local daemon polls the observation store for in-flight runs. Between
//! polls a run can leave the running set — it finished, failed, or was
//! reconciled to `stale`. [`CompletionTracker`] remembers which run ids were
//! running on the previous poll and reports the ones that have since departed,
//! so the daemon can fire exactly one completion notification per run instead
//! of re-notifying every poll.
//!
//! Absence from a poll is **not** proof of completion. The running set arrives
//! as a query result, and any bound or ordering churn in that query can evict a
//! live run from the page. A false completion is not a harmless extra ping: the
//! daemon marks the notification delivered before dispatching, so a run that is
//! wrongly reported burns its exactly-once marker while still running, and the
//! real completion is then suppressed on every path. Departure is therefore a
//! *candidate*, confirmed against the run's actual status by the caller-supplied
//! resolver before it is reported.
//!
//! The diff itself has no I/O, clock, or notifier coupling — the store read is
//! injected — so it stays deterministic and unit-testable. The daemon owns the
//! polling cadence, the store reads, and the notification dispatch around it.

use std::collections::BTreeSet;

/// The confirmed state of a run that disappeared from the running set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTerminality {
    /// The store confirms the run reached a terminal status. Report it.
    Terminal,
    /// The run is still running, carries a status Homeboy does not own, or the
    /// store could not be read. Keep tracking it and re-check on a later poll.
    Unresolved,
    /// The run no longer exists in the store (reaped by retention). Stop
    /// tracking it without reporting: there is nothing left to notify about.
    Vanished,
}

/// Tracks the set of run ids observed running on the previous poll.
#[derive(Debug, Default)]
pub struct CompletionTracker {
    running: BTreeSet<String>,
}

impl CompletionTracker {
    /// Record the currently-running run ids and return the ids that completed
    /// since the last call.
    ///
    /// An id that was running before and is absent now is only *reported* when
    /// `resolve` confirms [`RunTerminality::Terminal`]. An [`RunTerminality::Unresolved`]
    /// id is retained in the tracked set, so a run evicted from a truncated or
    /// re-ordered page is re-checked next poll and still reported when it truly
    /// finishes.
    ///
    /// The first call seeds the baseline and reports nothing: the daemon only
    /// pings for runs it actually observed in flight, never for runs that were
    /// already settled when it started watching. Returned ids are sorted and
    /// de-duplicated.
    pub fn observe<I, F>(&mut self, current_running: I, mut resolve: F) -> Vec<String>
    where
        I: IntoIterator<Item = String>,
        F: FnMut(&str) -> RunTerminality,
    {
        let current: BTreeSet<String> = current_running.into_iter().collect();
        let mut completed = Vec::new();
        let mut unresolved = BTreeSet::new();
        for candidate in self.running.difference(&current) {
            match resolve(candidate) {
                RunTerminality::Terminal => completed.push(candidate.clone()),
                RunTerminality::Unresolved => {
                    unresolved.insert(candidate.clone());
                }
                RunTerminality::Vanished => {}
            }
        }
        self.running = current;
        self.running.extend(unresolved);
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// Resolver for the cases where the running set is authoritative: every
    /// departure really is a completion.
    fn terminal(_run_id: &str) -> RunTerminality {
        RunTerminality::Terminal
    }

    #[test]
    fn first_observation_seeds_without_reporting_completions() {
        let mut tracker = CompletionTracker::default();
        let completed = tracker.observe(ids(&["a", "b"]), terminal);
        assert!(completed.is_empty());
        // The seeded ids are now tracked: completing them reports them.
        assert_eq!(tracker.observe(Vec::new(), terminal), ids(&["a", "b"]));
    }

    #[test]
    fn reports_runs_that_left_the_running_set() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b", "c"]), terminal);
        let completed = tracker.observe(ids(&["a"]), terminal);
        assert_eq!(completed, ids(&["b", "c"]));
    }

    #[test]
    fn newly_appearing_runs_are_tracked_then_reported_on_departure() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a"]), terminal);
        // `d` appears mid-flight; nothing completed yet.
        assert!(tracker.observe(ids(&["a", "d"]), terminal).is_empty());
        // `a` finishes; `d` still running.
        assert_eq!(tracker.observe(ids(&["d"]), terminal), ids(&["a"]));
        // `d` finishes.
        assert_eq!(tracker.observe(Vec::new(), terminal), ids(&["d"]));
    }

    #[test]
    fn a_run_is_reported_only_once() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a"]), terminal);
        assert_eq!(tracker.observe(Vec::new(), terminal), ids(&["a"]));
        // Subsequent polls without `a` running must not re-report it.
        assert!(tracker.observe(Vec::new(), terminal).is_empty());
    }

    #[test]
    fn duplicate_running_ids_collapse() {
        let mut tracker = CompletionTracker::default();
        let completed = tracker.observe(ids(&["a", "a", "b"]), terminal);
        assert!(completed.is_empty());
        // Despite the duplicate, each id is reported at most once on departure.
        assert_eq!(tracker.observe(Vec::new(), terminal), ids(&["a", "b"]));
    }

    #[test]
    fn a_run_truncated_out_of_the_page_is_not_reported_completed() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b"]), terminal);
        // The page drops `b` — bound hit, ordering churn — but `b` is still
        // running. Reporting it here would burn its exactly-once delivery
        // marker and permanently suppress the real completion.
        let completed = tracker.observe(ids(&["a"]), |_| RunTerminality::Unresolved);
        assert!(completed.is_empty());
    }

    #[test]
    fn a_truncated_run_is_still_reported_when_it_actually_completes() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b"]), terminal);
        // `b` is evicted from the page for two polls while still running.
        assert!(tracker
            .observe(ids(&["a"]), |_| RunTerminality::Unresolved)
            .is_empty());
        assert!(tracker
            .observe(ids(&["a"]), |_| RunTerminality::Unresolved)
            .is_empty());
        // It never came back to the page, but the tracker kept it: once the
        // store confirms terminality it is reported exactly once.
        assert_eq!(tracker.observe(ids(&["a"]), terminal), ids(&["b"]));
        assert!(tracker.observe(ids(&["a"]), terminal).is_empty());
    }

    #[test]
    fn a_truncated_run_that_returns_to_the_page_is_not_double_tracked() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b"]), terminal);
        assert!(tracker
            .observe(ids(&["a"]), |_| RunTerminality::Unresolved)
            .is_empty());
        // `b` re-appears in the next page, then genuinely completes.
        assert!(tracker.observe(ids(&["a", "b"]), terminal).is_empty());
        assert_eq!(tracker.observe(ids(&["a"]), terminal), ids(&["b"]));
    }

    #[test]
    fn only_the_departed_ids_are_resolved() {
        let resolved = RefCell::new(Vec::new());
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a", "b", "c"]), terminal);
        resolved.borrow_mut().clear();
        tracker.observe(ids(&["a"]), |run_id| {
            resolved.borrow_mut().push(run_id.to_string());
            RunTerminality::Terminal
        });
        // Runs still present in the page are never re-read from the store.
        assert_eq!(*resolved.borrow(), ids(&["b", "c"]));
    }

    #[test]
    fn a_vanished_run_is_dropped_without_being_reported() {
        let mut tracker = CompletionTracker::default();
        tracker.observe(ids(&["a"]), terminal);
        // The record was reaped by retention: nothing to notify about, and it
        // must not be retained forever either.
        assert!(tracker
            .observe(Vec::new(), |_| RunTerminality::Vanished)
            .is_empty());
        // Dropped, not retained: a later poll does not re-resolve it.
        let mut resolved = 0_usize;
        assert!(tracker
            .observe(Vec::new(), |_| {
                resolved += 1;
                RunTerminality::Terminal
            })
            .is_empty());
        assert_eq!(resolved, 0);
    }
}
