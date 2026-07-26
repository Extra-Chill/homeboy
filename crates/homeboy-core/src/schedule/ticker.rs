//! Firing due schedules on a cadence.
//!
//! The ticker wakes on a fixed interval, claims whatever is due, and runs each
//! claim on its own thread. It deliberately does **not** execute schedules
//! inline: a scheduled command that takes ten minutes would otherwise hold the
//! tick loop for ten minutes and starve every other schedule.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::execution::ScheduleCommandRunner;
use super::state::{load_state, save_state};
use super::types::Schedule;

/// A run in flight for longer than this is treated as abandoned — almost
/// always because the daemon was killed between marking a schedule running and
/// recording its result. Without this, a single hard kill would leave a
/// schedule permanently un-runnable under `OverlapPolicy::Skip`.
pub const STALE_RUN_RECLAIM_SECS: i64 = 6 * 60 * 60;

/// Tracks which schedules this process currently has in flight.
///
/// Disk state alone is not enough: a tick can fire again in the window between
/// spawning a run and that run recording itself as running, which would
/// dispatch the same schedule twice. An in-process claim closes that window;
/// the on-disk `running` flag still guards against a second daemon.
#[derive(Clone, Default)]
pub struct ScheduleTicker {
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl ScheduleTicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of runs this process currently has in flight.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.lock().map(|set| set.len()).unwrap_or(0)
    }

    fn claim(&self, id: &str) -> bool {
        self.in_flight
            .lock()
            .map(|mut set| set.insert(id.to_string()))
            .unwrap_or(false)
    }

    fn release(&self, id: &str) {
        if let Ok(mut set) = self.in_flight.lock() {
            set.remove(id);
        }
    }

    /// Schedules that are due now and not already running in this process.
    pub fn claim_due(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<Schedule> {
        let due = match super::due_schedules(now) {
            Ok(due) => due,
            Err(_) => return Vec::new(),
        };
        due.into_iter()
            .filter(|schedule| self.claim(&schedule.id))
            .collect()
    }

    /// Claim everything due and run each on its own thread.
    ///
    /// Returns the ids dispatched. Run threads are intentionally not joined —
    /// the daemon must be able to shut down promptly without waiting on an
    /// arbitrarily long scheduled command. Each run records its own outcome, and
    /// [`reclaim_stale_runs`] recovers state if the process dies mid-run.
    pub fn dispatch_due(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        runner: Arc<dyn ScheduleCommandRunner>,
    ) -> Vec<String> {
        let claimed = self.claim_due(now);
        let mut dispatched = Vec::with_capacity(claimed.len());
        for schedule in claimed {
            let id = schedule.id.clone();
            dispatched.push(id.clone());
            let ticker = self.clone();
            let runner = Arc::clone(&runner);
            let spawned = std::thread::Builder::new()
                .name(format!("homeboy-schedule-{id}"))
                .spawn(move || {
                    super::run_schedule(&schedule, runner.as_ref());
                    ticker.release(&schedule.id);
                });
            if spawned.is_err() {
                // Could not spawn — drop the claim so the next tick retries
                // rather than wedging this schedule forever.
                self.release(&id);
                dispatched.pop();
            }
        }
        dispatched
    }
}

/// Clear `running` markers left behind by a process that died mid-run.
///
/// Runs once at ticker start, matching how the daemon reconciles expired
/// reservations and admissions when it opens its job store.
pub fn reclaim_stale_runs(now: chrono::DateTime<chrono::Utc>) -> Vec<String> {
    let Ok(schedules) = super::list() else {
        return Vec::new();
    };
    let mut reclaimed = Vec::new();
    for schedule in schedules {
        let state = load_state(&schedule.id);
        if !state.running {
            continue;
        }
        let stale = state
            .started_at
            .as_deref()
            .and_then(|started| chrono::DateTime::parse_from_rfc3339(started).ok())
            .map(|started| {
                (now - started.with_timezone(&chrono::Utc)).num_seconds() > STALE_RUN_RECLAIM_SECS
            })
            // A running marker with no start time cannot be aged, and would
            // otherwise block the schedule forever.
            .unwrap_or(true);
        if !stale {
            continue;
        }
        let recovered = super::ScheduleState {
            running: false,
            started_at: None,
            ..state
        };
        if save_state(&schedule.id, &recovered).is_ok() {
            reclaimed.push(schedule.id);
        }
    }
    reclaimed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::types::{Cadence, NotifyPolicy, OverlapPolicy};
    use crate::schedule::ScheduleState;

    fn schedule(id: &str) -> Schedule {
        Schedule {
            id: id.to_string(),
            command: Some(vec!["triage".to_string()]),
            exec: None,
            steps: Vec::new(),
            every: Cadence::from_seconds(3_600).expect("cadence"),
            notify_on: NotifyPolicy::default(),
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: None,
            enabled: true,
            description: None,
            aliases: Vec::new(),
        }
    }

    struct CountingRunner {
        calls: Arc<Mutex<Vec<String>>>,
        block: Option<Arc<std::sync::Barrier>>,
    }

    impl ScheduleCommandRunner for CountingRunner {
        fn run(
            &self,
            command: crate::schedule::types::ScheduledCommand<'_>,
        ) -> crate::Result<crate::schedule::execution::ScheduleCommandResult> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(format!("{command:?}"));
            }
            if let Some(barrier) = &self.block {
                barrier.wait();
            }
            Ok(crate::schedule::execution::ScheduleCommandResult::Envelope(
                serde_json::json!({ "status": "succeeded", "exit_code": 0 }),
            ))
        }
    }

    #[test]
    fn dispatches_a_due_schedule() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("due-now")).expect("save");
            let calls = Arc::new(Mutex::new(Vec::new()));
            let runner = Arc::new(CountingRunner {
                calls: Arc::clone(&calls),
                block: None,
            });

            let ticker = ScheduleTicker::new();
            let dispatched = ticker.dispatch_due(chrono::Utc::now(), runner);
            assert_eq!(dispatched, vec!["due-now".to_string()]);

            // Let the run thread finish before asserting on its effects.
            for _ in 0..200 {
                if ticker.in_flight_count() == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(calls.lock().expect("calls").len(), 1);
            assert!(!load_state("due-now").running, "state must be settled");
        });
    }

    #[test]
    fn skips_a_schedule_that_is_not_due() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("just-ran")).expect("save");
            save_state(
                "just-ran",
                &ScheduleState {
                    last_run_at: Some(chrono::Utc::now().to_rfc3339()),
                    ..Default::default()
                },
            )
            .expect("save state");

            let runner = Arc::new(CountingRunner {
                calls: Arc::new(Mutex::new(Vec::new())),
                block: None,
            });
            let dispatched = ScheduleTicker::new().dispatch_due(chrono::Utc::now(), runner);
            assert!(dispatched.is_empty());
        });
    }

    /// The window this closes: a second tick firing before the first run has
    /// written its `running` marker to disk would otherwise dispatch the same
    /// schedule twice.
    #[test]
    fn a_second_tick_does_not_dispatch_a_run_already_in_flight() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("slow")).expect("save");

            let barrier = Arc::new(std::sync::Barrier::new(2));
            let calls = Arc::new(Mutex::new(Vec::new()));
            let runner = Arc::new(CountingRunner {
                calls: Arc::clone(&calls),
                block: Some(Arc::clone(&barrier)),
            });

            let ticker = ScheduleTicker::new();
            let first = ticker.dispatch_due(chrono::Utc::now(), Arc::clone(&runner) as Arc<_>);
            assert_eq!(first.len(), 1, "the first tick dispatches");

            let second = ticker.dispatch_due(chrono::Utc::now(), Arc::clone(&runner) as Arc<_>);
            assert!(
                second.is_empty(),
                "a tick must not dispatch a schedule already running in this process"
            );

            barrier.wait(); // release the blocked run
            for _ in 0..200 {
                if ticker.in_flight_count() == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(calls.lock().expect("calls").len(), 1, "exactly one run");
        });
    }

    /// Once a run completes, its claim is released so the schedule can fire
    /// again on a later tick.
    #[test]
    fn a_completed_run_releases_its_claim() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("repeatable")).expect("save");
            let runner = Arc::new(CountingRunner {
                calls: Arc::new(Mutex::new(Vec::new())),
                block: None,
            });

            let ticker = ScheduleTicker::new();
            ticker.dispatch_due(chrono::Utc::now(), runner);
            for _ in 0..200 {
                if ticker.in_flight_count() == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(ticker.in_flight_count(), 0, "claim must be released");
        });
    }

    /// A hard kill between "marked running" and "recorded result" would
    /// otherwise leave the schedule permanently blocked under skip-on-overlap.
    #[test]
    fn reclaims_a_run_abandoned_by_a_dead_process() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("abandoned")).expect("save");
            let now = chrono::Utc::now();
            save_state(
                "abandoned",
                &ScheduleState {
                    running: true,
                    started_at: Some(
                        (now - chrono::Duration::seconds(STALE_RUN_RECLAIM_SECS + 60)).to_rfc3339(),
                    ),
                    ..Default::default()
                },
            )
            .expect("save state");

            let reclaimed = reclaim_stale_runs(now);
            assert_eq!(reclaimed, vec!["abandoned".to_string()]);
            assert!(!load_state("abandoned").running);
        });
    }

    #[test]
    fn does_not_reclaim_a_run_that_is_merely_slow() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("still-going")).expect("save");
            let now = chrono::Utc::now();
            save_state(
                "still-going",
                &ScheduleState {
                    running: true,
                    started_at: Some((now - chrono::Duration::minutes(5)).to_rfc3339()),
                    ..Default::default()
                },
            )
            .expect("save state");

            assert!(reclaim_stale_runs(now).is_empty());
            assert!(load_state("still-going").running, "slow is not abandoned");
        });
    }

    /// A running marker with no start time cannot be aged out, so it is
    /// reclaimed rather than blocking the schedule forever.
    #[test]
    fn reclaims_a_running_marker_with_no_start_time() {
        crate::test_support::with_isolated_home(|_| {
            super::super::save(&schedule("no-start")).expect("save");
            save_state(
                "no-start",
                &ScheduleState {
                    running: true,
                    started_at: None,
                    ..Default::default()
                },
            )
            .expect("save state");

            assert_eq!(
                reclaim_stale_runs(chrono::Utc::now()),
                vec!["no-start".to_string()]
            );
        });
    }
}
