//! One poll loop for every `homeboy ... watch` command.
//!
//! `activity watch` and `runs watch` each grew their own copy of the same
//! bounded poll loop, the same `124` timeout exit code, and the same
//! terminal/timeout conclusion shape (#10315). They are **not** the same
//! command — see [`WatchPoller::Item`] — but the loop between them is
//! identical, so it lives here once and each command supplies only what is
//! genuinely its own: what it polls, when that is terminal, and how it renders
//! the result.
//!
//! The loop is generic over the poller, the sleep function, and the clock so a
//! test can drive it deterministically without a store, a real clock, or real
//! sleeps.

use std::time::Duration;

/// Exit code returned when a watch did not settle before its timeout.
///
/// Matches the GNU `timeout(1)` convention so wrappers can recognize the case.
/// Every watch surface shares this constant; it used to be declared once per
/// watch command, which is exactly how two commands drift.
pub const TIMEOUT_EXIT_CODE: i32 = 124;

/// "Fetch the current state of the thing being watched", abstracted so the loop
/// is testable without a real store, clock, or sleeps.
pub trait WatchPoller {
    /// The polled snapshot.
    ///
    /// This is an associated type rather than one shared record because the
    /// watch surfaces poll genuinely different domains: `runs watch` polls a
    /// `RunRecord` out of the observation store, while `activity watch` polls
    /// an `ActivityItem` — a projection across the observation store, the
    /// agent-task store, runner sessions, and daemon jobs, which has no
    /// `RunRecord` behind it at all.
    type Item;

    /// Read the current snapshot for `id`.
    fn poll(&self, id: &str) -> homeboy::core::Result<Self::Item>;

    /// True when the snapshot has settled and the watch should return.
    ///
    /// Owned by the poller because "terminal" is a per-domain judgement: a run
    /// status the caller cannot classify is treated as terminal so the watch
    /// surfaces it rather than blocking forever, and an activity item is
    /// terminal exactly when it is no longer queued or running.
    fn is_terminal(&self, item: &Self::Item) -> bool;
}

/// The two bounds every watch loop runs under.
#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    /// Delay between polls.
    pub interval: Duration,
    /// Total wall-clock bound, or `None` for an intentional indefinite watch.
    pub timeout: Option<Duration>,
}

/// Why the loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchConclusion {
    /// The watched item reached a terminal state.
    Terminal,
    /// [`WatchConfig::timeout`] expired first.
    TimedOut,
}

/// The last-seen snapshot plus the loop's accounting.
pub struct WatchResult<T> {
    pub item: T,
    pub conclusion: WatchConclusion,
    pub poll_count: u64,
    pub waited: Duration,
}

impl<T> WatchResult<T> {
    pub fn timed_out(&self) -> bool {
        self.conclusion == WatchConclusion::TimedOut
    }
}

/// Poll `id` until it settles or the timeout expires.
///
/// Ordering is load-bearing and is the shape both commands already had: poll,
/// report progress, check terminal, *then* check the timeout. A snapshot that
/// is terminal on the poll that also exhausts the timeout is reported as
/// terminal, not as a timeout.
pub fn watch_loop<P, S, C>(
    poller: &P,
    id: &str,
    config: &WatchConfig,
    mut sleep: S,
    elapsed: C,
    mut progress: impl FnMut(&P::Item, u64),
) -> homeboy::core::Result<WatchResult<P::Item>>
where
    P: WatchPoller,
    S: FnMut(Duration),
    C: Fn() -> Duration,
{
    let mut poll_count: u64 = 0;
    loop {
        let item = poller.poll(id)?;
        poll_count += 1;
        progress(&item, poll_count);

        if poller.is_terminal(&item) {
            return Ok(WatchResult {
                item,
                conclusion: WatchConclusion::Terminal,
                poll_count,
                waited: elapsed(),
            });
        }

        if let Some(timeout) = config.timeout {
            if elapsed() >= timeout {
                return Ok(WatchResult {
                    item,
                    conclusion: WatchConclusion::TimedOut,
                    poll_count,
                    waited: elapsed(),
                });
            }
        }

        sleep(config.interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    /// A poller over a trivial item type, so these tests exercise the loop
    /// itself rather than either command's domain model.
    struct ScriptedPoller {
        states: RefCell<VecDeque<&'static str>>,
        polled_ids: RefCell<Vec<String>>,
    }

    impl ScriptedPoller {
        fn new(states: &[&'static str]) -> Self {
            Self {
                states: RefCell::new(states.iter().copied().collect()),
                polled_ids: RefCell::new(Vec::new()),
            }
        }
    }

    impl WatchPoller for ScriptedPoller {
        type Item = &'static str;

        fn poll(&self, id: &str) -> homeboy::core::Result<Self::Item> {
            self.polled_ids.borrow_mut().push(id.to_string());
            let mut states = self.states.borrow_mut();
            // Hold the last scripted state once the queue is down to one entry,
            // so a "never terminal" script can be polled indefinitely.
            if states.len() > 1 {
                Ok(states.pop_front().expect("non-empty script"))
            } else {
                Ok(*states.front().expect("at least one scripted state"))
            }
        }

        fn is_terminal(&self, item: &Self::Item) -> bool {
            *item != "running"
        }
    }

    fn config(timeout_secs: Option<u64>) -> WatchConfig {
        WatchConfig {
            interval: Duration::from_secs(1),
            timeout: timeout_secs.map(Duration::from_secs),
        }
    }

    /// Drive the loop with a virtual clock: each simulated sleep advances time,
    /// so timeouts are exercised without real waiting.
    fn drive(
        poller: &ScriptedPoller,
        cfg: &WatchConfig,
    ) -> homeboy::core::Result<WatchResult<&'static str>> {
        let clock = Cell::new(Duration::ZERO);
        watch_loop(
            poller,
            "watched-1",
            cfg,
            |by| clock.set(clock.get() + by),
            || clock.get(),
            |_item, _poll| {},
        )
    }

    #[test]
    fn loop_returns_on_the_first_terminal_snapshot() {
        let poller = ScriptedPoller::new(&["running", "running", "pass"]);
        let result = drive(&poller, &config(None)).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::Terminal);
        assert!(!result.timed_out());
        assert_eq!(result.poll_count, 3);
        assert_eq!(result.item, "pass");
    }

    #[test]
    fn loop_times_out_when_the_item_never_settles() {
        let poller = ScriptedPoller::new(&["running"]);
        let result = drive(&poller, &config(Some(3))).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::TimedOut);
        assert!(result.timed_out());
        assert_eq!(result.item, "running");
        assert!(result.waited >= Duration::from_secs(3));
    }

    #[test]
    fn a_terminal_snapshot_wins_over_an_expired_timeout() {
        // Terminal is checked before the timeout, so an item that settles on the
        // same poll that exhausts the budget is reported as terminal.
        let poller = ScriptedPoller::new(&["pass"]);
        let result = drive(&poller, &config(Some(0))).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::Terminal);
        assert_eq!(result.poll_count, 1);
    }

    #[test]
    fn an_already_terminal_item_never_sleeps() {
        let poller = ScriptedPoller::new(&["pass"]);
        let slept = Cell::new(0u32);
        let clock = Cell::new(Duration::ZERO);
        let result = watch_loop(
            &poller,
            "watched-1",
            &config(None),
            |_| slept.set(slept.get() + 1),
            || clock.get(),
            |_item, _poll| {},
        )
        .expect("loop");
        assert_eq!(result.poll_count, 1);
        assert_eq!(slept.get(), 0);
    }

    #[test]
    fn progress_is_reported_once_per_poll_with_a_monotonic_count() {
        let poller = ScriptedPoller::new(&["running", "running", "fail"]);
        let clock = Cell::new(Duration::ZERO);
        let observed = RefCell::new(Vec::new());
        watch_loop(
            &poller,
            "watched-1",
            &config(None),
            |by| clock.set(clock.get() + by),
            || clock.get(),
            |item, poll| observed.borrow_mut().push((*item, poll)),
        )
        .expect("loop");
        assert_eq!(
            observed.into_inner(),
            vec![("running", 1), ("running", 2), ("fail", 3)]
        );
    }

    #[test]
    fn every_poll_uses_the_requested_id() {
        let poller = ScriptedPoller::new(&["running", "running", "pass"]);
        drive(&poller, &config(None)).expect("loop");
        assert_eq!(
            poller.polled_ids.into_inner(),
            vec!["watched-1".to_string(); 3]
        );
    }

    #[test]
    fn a_poll_error_aborts_the_loop() {
        struct FailingPoller;
        impl WatchPoller for FailingPoller {
            type Item = ();

            fn poll(&self, _id: &str) -> homeboy::core::Result<Self::Item> {
                Err(homeboy::core::Error::internal_unexpected("poll failed"))
            }

            fn is_terminal(&self, _item: &Self::Item) -> bool {
                unreachable!("a failing poll never yields an item")
            }
        }

        let clock = Cell::new(Duration::ZERO);
        let result = watch_loop(
            &FailingPoller,
            "watched-1",
            &config(None),
            |_| {},
            || clock.get(),
            |_item, _poll| {},
        );
        assert!(result.is_err());
    }
}
