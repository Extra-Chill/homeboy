//! `homeboy runs watch <run-id>` — block until a run reaches a terminal state.
//!
//! An offloaded `homeboy bench` / `runner exec` either buffers remote output
//! with no progress visibility or, when detached with `--detach-after-handoff`,
//! becomes a ghost: nothing tells the operator it finished. `runs watch` closes
//! that gap. It polls the persisted observation store — the same mirrored record
//! `runs show` reads, so it works for both attached and detached/offloaded runs
//! — streaming status to stderr until the run settles, then exits with a code
//! that reflects pass/fail.
//!
//! Two safety properties keep it from hanging forever:
//! - Each poll runs the existing reconcile pass, so a run whose owner process
//!   died transitions to `stale` and the watch surfaces it instead of waiting.
//! - `--timeout` bounds the total wait; on expiry the watch returns the
//!   last-seen state with a distinct timeout exit code.

use std::time::{Duration, Instant};

use clap::Args;
use serde::Serialize;

use homeboy::core::notify::{self, NotifyEvent, NotifyOutcome};
use homeboy::core::observation::runs_service;
use homeboy::core::observation::{ObservationStore, RunRecord, RunStatus};

use super::common::{parse_duration, RunSummary};
use super::types::RunsOutput;
use super::{reconcile, CmdResult};

/// Exit code returned when the run did not settle before `--timeout`. Matches
/// the GNU `timeout(1)` convention so wrappers can recognize the case.
pub(super) const TIMEOUT_EXIT_CODE: i32 = 124;

#[derive(Args, Clone)]
pub struct RunsWatchArgs {
    /// Observation run id to watch until it reaches a terminal state.
    pub run_id: String,
    /// Maximum time to wait before giving up (e.g. `30m`, `2h`, `7d`).
    /// Unbounded when omitted.
    #[arg(long)]
    pub timeout: Option<String>,
    /// Delay between status polls (e.g. `2s`, `1m`).
    #[arg(long, default_value = "2s")]
    pub interval: String,
    /// Emit a local completion notification when the run reaches a terminal
    /// state. The notifier is whatever `HOMEBOY_NOTIFY_COMMAND` (or
    /// `--notify-command`) points at; Homeboy does not hardcode an OS notifier.
    #[arg(long)]
    pub notify: bool,
    /// Override the notify command template instead of reading
    /// `HOMEBOY_NOTIFY_COMMAND`. Implies `--notify`.
    #[arg(long, requires = "notify")]
    pub notify_command: Option<String>,
}

#[derive(Serialize)]
pub struct RunsWatchOutput {
    pub command: &'static str,
    pub run_id: String,
    pub status: String,
    /// True when the run reached a terminal state; false when the watch
    /// returned because `--timeout` expired first.
    pub terminal: bool,
    pub timed_out: bool,
    pub waited_secs: u64,
    pub poll_count: u64,
    pub run: RunSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<NotifyOutcome>,
}

/// Abstraction over "fetch the current state of this run" so the watch loop is
/// testable without a real store, clock, or sleeps.
pub(super) trait RunPoller {
    fn poll(&self, run_id: &str) -> homeboy::core::Result<RunRecord>;
}

/// Production poller: reconcile dead-owner runs, refresh mirrored runner
/// evidence, then read the freshest local record. Mirrors `runs show` side
/// effects so a detached/offloaded run's status is up to date each poll.
struct StorePoller<'a> {
    store: &'a ObservationStore,
}

impl RunPoller for StorePoller<'_> {
    fn poll(&self, run_id: &str) -> homeboy::core::Result<RunRecord> {
        reconcile::reconcile_owned_stale_running_runs(self.store, 1000)?;
        runs_service::refresh_mirrored_daemon_evidence_best_effort(run_id);
        runs_service::require_run(self.store, run_id)
    }
}

struct WatchConfig {
    interval: Duration,
    timeout: Option<Duration>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WatchConclusion {
    Terminal,
    TimedOut,
}

pub(super) struct WatchResult {
    run: RunRecord,
    conclusion: WatchConclusion,
    poll_count: u64,
    waited: Duration,
}

pub fn watch_run(args: RunsWatchArgs) -> CmdResult<RunsOutput> {
    let interval = parse_duration(&args.interval)?;
    let timeout = args.timeout.as_deref().map(parse_duration).transpose()?;
    let config = WatchConfig { interval, timeout };

    let store = ObservationStore::open_initialized()?;
    let poller = StorePoller { store: &store };

    let started = Instant::now();
    let run_id = args.run_id.clone();
    let result = run_watch_loop(
        &poller,
        &args.run_id,
        &config,
        std::thread::sleep,
        || started.elapsed(),
        |run, poll_count| emit_progress(&run_id, run, poll_count),
    )?;

    let notify = maybe_notify(&args, &result);
    let (output, exit_code) = build_output(&args.run_id, result, notify);
    Ok((RunsOutput::Watch(output), exit_code))
}

/// The core poll loop, generic over the run source, the sleep function, and the
/// clock so tests can drive it deterministically.
fn run_watch_loop<P, S, C>(
    poller: &P,
    run_id: &str,
    config: &WatchConfig,
    mut sleep: S,
    elapsed: C,
    mut progress: impl FnMut(&RunRecord, u64),
) -> homeboy::core::Result<WatchResult>
where
    P: RunPoller,
    S: FnMut(Duration),
    C: Fn() -> Duration,
{
    let mut poll_count: u64 = 0;
    loop {
        let run = poller.poll(run_id)?;
        poll_count += 1;
        progress(&run, poll_count);

        if is_terminal_status(&run.status) {
            return Ok(WatchResult {
                run,
                conclusion: WatchConclusion::Terminal,
                poll_count,
                waited: elapsed(),
            });
        }

        if let Some(timeout) = config.timeout {
            if elapsed() >= timeout {
                return Ok(WatchResult {
                    run,
                    conclusion: WatchConclusion::TimedOut,
                    poll_count,
                    waited: elapsed(),
                });
            }
        }

        sleep(config.interval);
    }
}

/// A run is terminal when it is not `running`. An unrecognized status is treated
/// as terminal so the watch surfaces it rather than blocking on a status it
/// cannot reason about.
fn is_terminal_status(status: &str) -> bool {
    RunStatus::from_label(status)
        .map(RunStatus::is_terminal)
        .unwrap_or(true)
}

/// Map a terminal run status to a process exit code: `pass`/`skipped` succeed,
/// every other settled status (including `stale` ghosts and unknown statuses)
/// fails.
fn exit_code_for_status(status: &str) -> i32 {
    match RunStatus::from_label(status) {
        Some(RunStatus::Pass) | Some(RunStatus::Skipped) => 0,
        _ => 1,
    }
}

fn build_output(
    run_id: &str,
    result: WatchResult,
    notify: Option<NotifyOutcome>,
) -> (RunsWatchOutput, i32) {
    let timed_out = result.conclusion == WatchConclusion::TimedOut;
    let status = result.run.status.clone();
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else {
        exit_code_for_status(&status)
    };

    (
        RunsWatchOutput {
            command: "runs.watch",
            run_id: run_id.to_string(),
            status,
            terminal: !timed_out,
            timed_out,
            waited_secs: result.waited.as_secs(),
            poll_count: result.poll_count,
            run: super::run_summary(result.run),
            notify,
        },
        exit_code,
    )
}

fn maybe_notify(args: &RunsWatchArgs, result: &WatchResult) -> Option<NotifyOutcome> {
    if !args.notify || result.conclusion != WatchConclusion::Terminal {
        return None;
    }
    let event = NotifyEvent::run_completed(&args.run_id, &result.run.status);
    Some(notify::dispatch(&event, args.notify_command.as_deref()))
}

fn emit_progress(run_id: &str, run: &RunRecord, poll_count: u64) {
    let note = reconcile::running_status_note(run)
        .map(|note| format!(" ({note})"))
        .unwrap_or_default();
    eprintln!(
        "homeboy runs watch {run_id}: poll {poll_count} status={}{note}",
        run.status
    );
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;

    use super::*;

    struct ScriptedPoller {
        states: std::cell::RefCell<VecDeque<&'static str>>,
    }

    impl ScriptedPoller {
        fn new(states: &[&'static str]) -> Self {
            Self {
                states: std::cell::RefCell::new(states.iter().copied().collect()),
            }
        }
    }

    impl RunPoller for ScriptedPoller {
        fn poll(&self, run_id: &str) -> homeboy::core::Result<RunRecord> {
            let mut states = self.states.borrow_mut();
            // Hold the last scripted state once the queue is down to one entry,
            // so a "never terminal" script can be polled indefinitely.
            let status = if states.len() > 1 {
                states.pop_front().unwrap()
            } else {
                *states.front().expect("at least one scripted status")
            };
            Ok(run_record(run_id, status))
        }
    }

    fn run_record(run_id: &str, status: &str) -> RunRecord {
        RunRecord {
            id: run_id.to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-05-02T16:46:46Z".to_string(),
            finished_at: (status != "running").then(|| "2026-05-02T16:50:00Z".to_string()),
            status: status.to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/tmp/homeboy-fixture".to_string()),
            homeboy_version: Some("test-version".to_string()),
            git_sha: Some("abc123".to_string()),
            rig_id: Some("studio".to_string()),
            metadata_json: serde_json::json!({}),
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
    fn run_loop(
        poller: &ScriptedPoller,
        cfg: &WatchConfig,
    ) -> homeboy::core::Result<WatchResult> {
        let clock = Cell::new(Duration::ZERO);
        let advance = |by: Duration| clock.set(clock.get() + by);
        run_watch_loop(
            poller,
            "run-1",
            cfg,
            |d| advance(d),
            || clock.get(),
            |_run, _poll| {},
        )
    }

    #[test]
    fn watch_reaches_terminal_pass_with_zero_exit() {
        let poller = ScriptedPoller::new(&["running", "running", "pass"]);
        let result = run_loop(&poller, &config(None)).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::Terminal);
        assert_eq!(result.poll_count, 3);

        let (output, exit_code) = build_output("run-1", result, None);
        assert_eq!(exit_code, 0);
        assert!(output.terminal);
        assert!(!output.timed_out);
        assert_eq!(output.status, "pass");
    }

    #[test]
    fn watch_fail_status_exits_nonzero() {
        let poller = ScriptedPoller::new(&["running", "fail"]);
        let result = run_loop(&poller, &config(None)).expect("loop");
        let (output, exit_code) = build_output("run-1", result, None);
        assert_eq!(exit_code, 1);
        assert_eq!(output.status, "fail");
    }

    #[test]
    fn watch_surfaces_stale_ghost_as_terminal_failure() {
        // A reconciled ghost run settles to `stale`; the watch must exit, not
        // hang, and report failure.
        let poller = ScriptedPoller::new(&["running", "stale"]);
        let result = run_loop(&poller, &config(None)).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::Terminal);
        let (output, exit_code) = build_output("run-1", result, None);
        assert_eq!(exit_code, 1);
        assert_eq!(output.status, "stale");
    }

    #[test]
    fn watch_times_out_when_run_never_settles() {
        let poller = ScriptedPoller::new(&["running"]);
        let result = run_loop(&poller, &config(Some(3))).expect("loop");
        assert_eq!(result.conclusion, WatchConclusion::TimedOut);

        let (output, exit_code) = build_output("run-1", result, None);
        assert_eq!(exit_code, TIMEOUT_EXIT_CODE);
        assert!(!output.terminal);
        assert!(output.timed_out);
        assert_eq!(output.status, "running");
    }

    #[test]
    fn unknown_status_is_treated_as_terminal_failure() {
        assert!(is_terminal_status("definitely-not-a-status"));
        assert_eq!(exit_code_for_status("definitely-not-a-status"), 1);
    }

    #[test]
    fn skipped_status_exits_zero() {
        assert!(is_terminal_status("skipped"));
        assert_eq!(exit_code_for_status("skipped"), 0);
    }
}
