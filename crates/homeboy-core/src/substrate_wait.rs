//! Transient substrate refusals and the bounded wait through them.
//!
//! A Lab runner that is busy (pressure, capacity), whose readiness is not yet
//! observed, whose daemon is mid-promotion, or whose admission endpoint timed
//! out is not broken: re-observing clears it without anyone acting on the
//! runner. Work that already holds its place in the queue waits through these
//! conditions instead of terminalizing (#17765). Every other refusal still
//! fails at once.
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Error, ErrorCode, Result};

/// `RunnerAvailability` reasons that waiting can clear. Any other reason
/// (disconnected, stale or drifted daemon, missing capability) is a hard
/// refusal.
pub const TRANSIENT_ADMISSION_REASONS: &[&str] = &[
    "capacity_reached",
    "capacity_unknown",
    "pressure",
    "daemon_freshness_unavailable",
    "daemon_unverified",
    "active_jobs_unavailable",
    "active_jobs_not_queried",
];

/// How long queued work waits through transient substrate refusals before it
/// fails with the last refusal.
pub const SUBSTRATE_WAIT_DEADLINE: Duration = Duration::from_secs(30 * 60);

const INITIAL_BACKOFF: Duration = if cfg!(test) {
    Duration::from_millis(5)
} else {
    Duration::from_secs(2)
};
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Whether every reason is one that waiting can clear.
pub fn reasons_are_transient<'a>(reasons: impl IntoIterator<Item = &'a str>) -> bool {
    let mut any = false;
    for reason in reasons {
        if !TRANSIENT_ADMISSION_REASONS.contains(&reason) {
            return false;
        }
        any = true;
    }
    any
}

/// `RunnerAvailability` reasons for a configured runner that is down rather
/// than busy. A fresh submission still refuses them at preflight, but work
/// already admitted to the queue waits for the runner to return: an outage
/// must not spend its recovery budget (#18549).
pub const RUNNER_OUTAGE_REASONS: &[&str] = &["not_connected", "stale_daemon"];

/// Whether admitted, queued work waits through this runner refusal instead
/// of failing into recovery.
pub fn admitted_work_waits_through(reason: &str) -> bool {
    reasons_are_transient([reason]) || RUNNER_OUTAGE_REASONS.contains(&reason)
}

/// Reasons a runner reports when its controller tunnel is down. Waiting alone
/// never clears them: someone has to run `homeboy runner connect` (#18744).
pub const RUNNER_DISCONNECTED_REASONS: &[&str] = &[
    "not_connected",
    "active_jobs_not_queried",
    "active_jobs_unavailable",
];

/// Reasons proving a live, reachable runner that is merely busy. A reconnect
/// would not help and must not be attempted.
pub const RUNNER_BUSY_REASONS: &[&str] = &["capacity_reached", "pressure"];

/// Minimum spacing between automatic reconnect attempts for one runner.
pub const RUNNER_RECONNECT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Whether admitted work waiting on a runner should try to reconnect it:
/// the runner is disconnected (by its own flag or a `not_connected` reason)
/// and nothing shows it is merely busy.
pub fn runner_reconnect_warranted<'a>(
    connected: bool,
    reasons: impl IntoIterator<Item = &'a str>,
) -> bool {
    let mut not_connected = !connected;
    for reason in reasons {
        if RUNNER_BUSY_REASONS.contains(&reason) {
            return false;
        }
        not_connected |= reason == "not_connected";
    }
    not_connected
}

/// The operator action that clears a runner wait with this reason.
pub fn runner_wait_next_action(runner: &str, reason: &str) -> String {
    match reason {
        "daemon_incompatible" | "daemon_not_fresh" => {
            format!("homeboy runner refresh-homeboy {runner} --reconnect")
        }
        "stale_jobs" => format!("homeboy runner reconcile {runner}"),
        "capacity_reached" => "wait for runner capacity to become available".to_string(),
        "pressure" => "wait for runner resource pressure to subside".to_string(),
        reason if RUNNER_DISCONNECTED_REASONS.contains(&reason) => {
            format!("homeboy runner connect {runner}")
        }
        _ => format!("inspect runner {runner} eligibility"),
    }
}

/// Whether a reconnect attempt is due given the last recorded attempt.
pub fn reconnect_attempt_due(last: Option<SystemTime>, now: SystemTime) -> bool {
    last.is_none_or(|last| {
        now.duration_since(last)
            .is_ok_and(|elapsed| elapsed >= RUNNER_RECONNECT_INTERVAL)
            // A recorded attempt in the future (clock moved back) must not
            // block reconnects forever.
            || last.duration_since(now).is_ok_and(|ahead| ahead > RUNNER_RECONNECT_INTERVAL)
    })
}

/// Claim the right to attempt one automatic reconnect of `runner` at `now`.
///
/// The last attempt time lives in `<dir>/<runner>.auto-reconnect`, guarded by
/// an exclusive non-blocking file lock so concurrent waiters (threads or
/// processes) never stampede: a contended lock or a recent attempt returns
/// `false`. A `true` return has already recorded `now` as the attempt.
pub fn claim_runner_reconnect_attempt(dir: &Path, runner: &str, now: SystemTime) -> Result<bool> {
    use fs4::fs_std::FileExt;
    use std::io::{Read, Seek, Write};

    let io = |error: std::io::Error| {
        Error::internal_io(
            format!("runner {runner} reconnect throttle: {error}"),
            Some(dir.display().to_string()),
        )
    };
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{runner}.auto-reconnect")))
        .map_err(io)?;
    if !file.try_lock_exclusive().map_err(io)? {
        return Ok(false);
    }
    let mut recorded = String::new();
    file.read_to_string(&mut recorded).map_err(io)?;
    let last = recorded
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
    if !reconnect_attempt_due(last, now) {
        return Ok(false);
    }
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    file.set_len(0).map_err(io)?;
    file.rewind().map_err(io)?;
    file.write_all(secs.to_string().as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(true)
}

/// Classify a refusal by its error code and structured details.
pub fn is_transient_refusal(code: &str, details: &serde_json::Value) -> bool {
    // Admission already consumed its entire budget. An outer substrate wait
    // must not silently start another promotion wait.
    if ErrorCode::from_str(code) == Some(ErrorCode::RuntimePromotionWaitTimeout) {
        return false;
    }
    let reasons_transient = |value: &serde_json::Value| {
        value
            .as_array()
            .is_some_and(|reasons| reasons_are_transient(reasons.iter().filter_map(|r| r.as_str())))
    };
    matches!(
        ErrorCode::from_str(code),
        Some(ErrorCode::RuntimePromotionContended)
    ) || details["execution_step_disposition"] == "runner_ineligible"
        || details["admission_wait"] == crate::parsed_command_preflight::READINESS_UNOBSERVED
        || details["daemon_transport_error"]["kind"] == "timeout"
        || details["substrate_wait"] == "runner_connect"
        || details["substrate_wait"] == "runner_restarting"
        || details["kind"] == "runner_state_lock_timeout"
        || reasons_transient(&details["runner_availability"]["reasons"])
        || reasons_transient(&details["readiness_reasons"])
        // A Lab gate admission refusal states its own verdict: the
        // submission re-check can see pressure return after the controller
        // already waited it out (#18058).
        || details["transient"] == true
}

pub fn is_transient_error(error: &Error) -> bool {
    is_transient_refusal(error.code.as_str(), &error.details)
}

/// Run `attempt` until it succeeds, fails with a refusal that is not
/// transient, `cancelled` reports true, or `deadline` passes. Backoff starts
/// at 2s and is capped at 30s; `on_wait` runs before each sleep (progress,
/// heartbeats). The final error records how long it waited.
///
/// Inside a [`crate::foreground_wait`] scope each refusal is reported as what
/// the client waits for, and the scope's signal or admission bound stops the
/// wait like `cancelled` (#18042).
pub fn wait_through<T>(
    label: &str,
    deadline: Duration,
    cancelled: impl Fn() -> bool,
    mut on_wait: impl FnMut(&Error, Duration),
    mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
    let cancelled = || cancelled() || crate::foreground_wait::stop_requested().is_some();
    let started = Instant::now();
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let error = match attempt() {
            Ok(value) => {
                crate::foreground_wait::admitted();
                return Ok(value);
            }
            Err(error) => error,
        };
        let remaining = deadline.saturating_sub(started.elapsed());
        if !is_transient_error(&error) || remaining.is_zero() || cancelled() {
            return Err(record_wait(error, started.elapsed()));
        }
        crate::foreground_wait::waiting(crate::foreground_wait::WaitSubject::from_refusal(
            label, &error,
        ));
        if cancelled() {
            return Err(record_wait(error, started.elapsed()));
        }
        on_wait(&error, started.elapsed());
        crate::log_status!(
            "wait",
            "{label}: {} — retrying in {}s (waited {}s)",
            error.message,
            backoff.min(remaining).as_secs(),
            started.elapsed().as_secs()
        );
        let wake = Instant::now() + backoff.min(remaining);
        while Instant::now() < wake && !cancelled() {
            std::thread::sleep(
                wake.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(250)),
            );
        }
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

fn record_wait(mut error: Error, waited: Duration) -> Error {
    if waited.is_zero() {
        return error;
    }
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["substrate_waited_seconds"] = serde_json::json!(waited.as_secs());
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn refusal(code: ErrorCode, details: serde_json::Value) -> Error {
        Error::new(code, "refused", details)
    }

    /// #18744: a disconnected runner warrants a reconnect; a busy one never.
    #[test]
    fn reconnect_triggers_on_disconnection_not_on_capacity_or_pressure() {
        for (connected, reasons, expected) in [
            (true, vec!["not_connected"], true),
            (false, vec!["active_jobs_not_queried"], true),
            (
                false,
                vec!["active_jobs_unavailable", "capacity_unknown"],
                true,
            ),
            (false, vec![], true),
            (true, vec!["active_jobs_not_queried"], false),
            (true, vec!["capacity_reached"], false),
            (true, vec!["pressure"], false),
            (false, vec!["pressure"], false),
            (true, vec!["not_connected", "capacity_reached"], false),
            (true, vec![], false),
        ] {
            assert_eq!(
                runner_reconnect_warranted(connected, reasons.iter().copied()),
                expected,
                "connected={connected} reasons={reasons:?}"
            );
        }
    }

    #[test]
    fn reconnect_attempts_are_rate_limited_per_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t0 = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let claim = |runner: &str, at: SystemTime| {
            claim_runner_reconnect_attempt(dir.path(), runner, at).expect("claim")
        };
        assert!(claim("lab", t0));
        assert!(!claim("lab", t0));
        assert!(!claim("lab", t0 + Duration::from_secs(299)));
        // Another runner has its own budget.
        assert!(claim("other", t0 + Duration::from_secs(1)));
        assert!(claim("lab", t0 + RUNNER_RECONNECT_INTERVAL));
        assert!(!claim(
            "lab",
            t0 + RUNNER_RECONNECT_INTERVAL + Duration::from_secs(10)
        ));
        // A clock that moved far back does not block reconnects forever.
        assert!(claim("lab", t0 - Duration::from_secs(3600)));
    }

    #[test]
    fn concurrent_reconnect_claims_do_not_stampede() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::now();
        let claimed = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    if claim_runner_reconnect_attempt(dir.path(), "lab", now).expect("claim") {
                        claimed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(claimed.into_inner(), 1);
    }

    #[test]
    fn disconnected_runner_wait_names_runner_connect() {
        for reason in [
            "not_connected",
            "active_jobs_not_queried",
            "active_jobs_unavailable",
        ] {
            assert_eq!(
                runner_wait_next_action("homeboy-lab", reason),
                "homeboy runner connect homeboy-lab"
            );
        }
        assert_eq!(
            runner_wait_next_action("homeboy-lab", "pressure"),
            "wait for runner resource pressure to subside"
        );
        assert_eq!(
            runner_wait_next_action("homeboy-lab", "daemon_not_fresh"),
            "homeboy runner refresh-homeboy homeboy-lab --reconnect"
        );
    }

    /// #18549 holds: a runner outage is a wait for admitted work, never a
    /// bounded wait that exhausts into recovery.
    #[test]
    fn runner_outage_does_not_consume_recovery_budget() {
        for reason in RUNNER_DISCONNECTED_REASONS {
            assert!(admitted_work_waits_through(reason), "{reason}");
            assert!(
                !crate::daemon::step_job::runner_wait_is_bounded(reason),
                "{reason}"
            );
        }
    }

    /// #17765: each transient substrate class waits and then succeeds; a hard
    /// refusal fails on the first observation.
    #[test]
    fn transient_substrate_refusals_wait_and_hard_refusals_fail_at_once() {
        let pressure = crate::runner_pressure::RunnerPressure {
            admission_reason: Some("pressure".into()),
            ..Default::default()
        }
        .admission_refusal();
        let cases = [
            (pressure, true),
            (
                refusal(
                    ErrorCode::InternalIoError,
                    serde_json::json!({"kind": "runner_state_lock_timeout"}),
                ),
                true,
            ),
            (
                refusal(
                    ErrorCode::InternalUnexpected,
                    serde_json::json!({"daemon_transport_error": {"kind": "timeout", "path": "/admissions"}}),
                ),
                true,
            ),
            (
                refusal(
                    ErrorCode::RuntimePromotionWaitTimeout,
                    serde_json::json!({}),
                ),
                false,
            ),
            (
                refusal(
                    ErrorCode::ValidationInvalidArgument,
                    serde_json::json!({"runner_availability": {"reasons": ["capacity_reached", "pressure"]}}),
                ),
                true,
            ),
            (
                refusal(
                    ErrorCode::ValidationInvalidArgument,
                    serde_json::json!({"readiness_reasons": ["daemon_freshness_unavailable", "daemon_unverified"]}),
                ),
                true,
            ),
            // #18058: the Lab gate's submission re-check refusal.
            (
                refusal(
                    ErrorCode::RunnerLabTransportFailure,
                    serde_json::json!({"stage": "admission", "reasons": ["pressure"], "freshness": "fresh", "transient": true}),
                ),
                true,
            ),
            (
                refusal(
                    ErrorCode::RunnerLabTransportFailure,
                    serde_json::json!({"stage": "admission", "reasons": ["not_connected"], "freshness": "fresh", "transient": false}),
                ),
                false,
            ),
            (
                refusal(
                    ErrorCode::ValidationInvalidArgument,
                    serde_json::json!({"runner_availability": {"reasons": ["pressure", "not_connected"]}}),
                ),
                false,
            ),
            (
                refusal(ErrorCode::ValidationInvalidArgument, serde_json::json!({})),
                false,
            ),
        ];
        for (error, transient) in cases {
            let calls = Cell::new(0);
            let result = wait_through(
                "test",
                Duration::from_secs(5),
                || false,
                |_, _| {},
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() < 3 {
                        Err(error.clone())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.is_ok(), transient, "{:?}", error.details);
            assert_eq!(calls.get(), if transient { 3 } else { 1 });
        }
    }
}
