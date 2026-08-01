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

/// Every duration unit any homeboy flag accepts, and what one of it is worth.
///
/// Four commands had grown their own copy of this table with three different
/// unit sets, so `triage --timeout 7d` errored while `runs watch --timeout 7d`
/// worked, and `activity --duration 500ms` errored while `observe --duration
/// 500ms` did not. This is the union of all four: every command now accepts
/// every unit.
const DURATION_UNITS: &[(&str, u64)] = &[
    ("ms", 1),
    ("s", 1_000),
    ("sec", 1_000),
    ("secs", 1_000),
    ("second", 1_000),
    ("seconds", 1_000),
    ("m", 60 * 1_000),
    ("min", 60 * 1_000),
    ("mins", 60 * 1_000),
    ("minute", 60 * 1_000),
    ("minutes", 60 * 1_000),
    ("h", 60 * 60 * 1_000),
    ("hr", 60 * 60 * 1_000),
    ("hrs", 60 * 60 * 1_000),
    ("hour", 60 * 60 * 1_000),
    ("hours", 60 * 60 * 1_000),
    ("d", 24 * 60 * 60 * 1_000),
    ("day", 24 * 60 * 60 * 1_000),
    ("days", 24 * 60 * 60 * 1_000),
];

/// Human-readable list of accepted units, for `--help` and error messages.
pub const DURATION_UNITS_HINT: &str = "ms, s, m, h, or d";

/// Parse a duration like `500ms`, `30s`, `5m`, `2h`, or `7d`.
///
/// Returns the plain message on failure; [`parse_duration`] wraps it into a
/// structured argument error and [`parse_duration_arg`] hands it to clap.
///
/// A bare number with no unit is rejected, as it was by all four parsers this
/// replaces -- `--timeout 30` is ambiguous and always was an error.
fn parse_duration_parts(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(split);

    if amount.is_empty() || unit.is_empty() {
        return Err(format!(
            "expected duration like 500ms, 30s, 5m, 2h, or 7d (unit required: {DURATION_UNITS_HINT})"
        ));
    }

    let amount = amount
        .parse::<u64>()
        .map_err(|_| "duration amount must be a positive integer".to_string())?;

    // Preserved from three of the four parsers. `activity` was the one that
    // lacked it, so `activity --interval 0s` used to spin with no delay.
    if amount == 0 {
        return Err("duration amount must be greater than zero".to_string());
    }

    let millis_per_unit = DURATION_UNITS
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, millis)| *millis)
        .ok_or_else(|| format!("duration unit must be one of {DURATION_UNITS_HINT}"))?;

    // The parsers this replaces multiplied unchecked, so `9999999999999999999d`
    // panicked in debug builds. Report it instead.
    amount
        .checked_mul(millis_per_unit)
        .map(Duration::from_millis)
        .ok_or_else(|| "duration is too large".to_string())
}

/// Parse a duration into a structured argument error attributed to `field`.
///
/// `field` is the name the user typed (`since`, `duration`, `--timeout`) so the
/// error points at the flag that was wrong.
pub fn parse_duration(field: &str, raw: &str) -> homeboy::core::Result<Duration> {
    parse_duration_parts(raw).map_err(|message| {
        homeboy::core::Error::validation_invalid_argument(
            field,
            message,
            Some(raw.to_string()),
            None,
        )
    })
}

/// Duration parser for clap `value_parser` attributes.
pub fn parse_duration_arg(raw: &str) -> Result<Duration, String> {
    parse_duration_parts(raw)
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
mod duration_tests {
    use super::*;

    /// The full accepted grid. Every one of these must work on *every* command
    /// that takes a duration -- that is the point of the consolidation.
    const ACCEPTED: &[(&str, u64)] = &[
        ("1ms", 1),
        ("500ms", 500),
        ("1s", 1_000),
        ("30s", 30_000),
        ("30sec", 30_000),
        ("30secs", 30_000),
        ("30second", 30_000),
        ("30seconds", 30_000),
        ("5m", 300_000),
        ("5min", 300_000),
        ("5mins", 300_000),
        ("5minute", 300_000),
        ("5minutes", 300_000),
        ("2h", 7_200_000),
        ("2hr", 7_200_000),
        ("2hrs", 7_200_000),
        ("2hour", 7_200_000),
        ("2hours", 7_200_000),
        ("7d", 604_800_000),
        ("7day", 604_800_000),
        ("7days", 604_800_000),
        // Surrounding whitespace was tolerated by three of the four parsers.
        ("  10m  ", 600_000),
    ];

    #[test]
    fn every_unit_parses_to_the_same_value_everywhere() {
        for (raw, expected_millis) in ACCEPTED {
            assert_eq!(
                parse_duration_arg(raw),
                Ok(Duration::from_millis(*expected_millis)),
                "{raw} should parse to {expected_millis}ms"
            );
        }
    }

    /// The regressions this consolidation fixes. Before it, `triage` rejected
    /// `d` and `ms`, and `activity`/`runs` rejected `ms`.
    #[test]
    fn units_that_used_to_be_rejected_per_command_now_parse() {
        assert_eq!(
            parse_duration("--timeout", "7d").expect("triage now accepts days"),
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(
            parse_duration("duration", "500ms").expect("activity now accepts millis"),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn zero_is_rejected_for_every_unit() {
        for unit in ["ms", "s", "m", "h", "d"] {
            let raw = format!("0{unit}");
            assert_eq!(
                parse_duration_arg(&raw),
                Err("duration amount must be greater than zero".to_string()),
                "0{unit} must be rejected"
            );
        }
    }

    #[test]
    fn a_bare_number_is_rejected_because_the_unit_is_ambiguous() {
        for raw in ["30", "0", ""] {
            assert!(parse_duration_arg(raw).is_err(), "{raw:?} must be rejected");
        }
    }

    #[test]
    fn unknown_units_are_rejected_with_the_full_unit_list() {
        let error = parse_duration_arg("5w").expect_err("weeks are not a unit");
        assert!(error.contains(DURATION_UNITS_HINT), "unexpected: {error}");
        assert!(parse_duration_arg("5 s").is_err());
        assert!(parse_duration_arg("-5s").is_err());
        assert!(parse_duration_arg("1.5h").is_err());
    }

    /// The parsers this replaced multiplied unchecked and panicked in debug.
    #[test]
    fn an_overflowing_duration_is_an_error_not_a_panic() {
        assert_eq!(
            parse_duration_arg("99999999999999999999999d"),
            Err("duration amount must be a positive integer".to_string()),
        );
        assert_eq!(
            parse_duration_arg(&format!("{}d", u64::MAX)),
            Err("duration is too large".to_string()),
        );
    }

    #[test]
    fn the_structured_error_is_attributed_to_the_field_that_was_wrong() {
        let error = parse_duration("--timeout", "5w").expect_err("weeks are not a unit");
        assert_eq!(
            error.code,
            homeboy::core::ErrorCode::ValidationInvalidArgument
        );
        assert!(
            error.message.contains("--timeout"),
            "unexpected: {}",
            error.message
        );
        // The offending value is carried structurally, not only in the message.
        assert_eq!(error.details["id"], serde_json::json!("5w"));
        assert_eq!(error.details["field"], serde_json::json!("--timeout"));
    }

    /// Guards the table itself: a unit added to `DURATION_UNITS` with a bad
    /// multiplier is caught here rather than in whichever command hits it.
    #[test]
    fn the_unit_table_is_internally_consistent() {
        for (name, millis) in DURATION_UNITS {
            assert!(*millis > 0, "{name} must have a positive multiplier");
            assert_eq!(
                parse_duration_arg(&format!("1{name}")),
                Ok(Duration::from_millis(*millis)),
                "1{name} must equal its table entry"
            );
        }
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
