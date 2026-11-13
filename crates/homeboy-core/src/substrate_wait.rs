//! Transient substrate refusals and the bounded wait through them.
//!
//! A Lab runner that is busy (pressure, capacity), whose readiness is not yet
//! observed, whose daemon is mid-promotion, or whose admission endpoint timed
//! out is not broken: re-observing clears it without anyone acting on the
//! runner. Work that already holds its place in the queue waits through these
//! conditions instead of terminalizing (#17765). Every other refusal still
//! fails at once.
use std::time::{Duration, Instant};

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

/// Classify a refusal by its error code and structured details.
pub fn is_transient_refusal(code: &str, details: &serde_json::Value) -> bool {
    let reasons_transient = |value: &serde_json::Value| {
        value
            .as_array()
            .is_some_and(|reasons| reasons_are_transient(reasons.iter().filter_map(|r| r.as_str())))
    };
    matches!(
        ErrorCode::from_str(code),
        Some(ErrorCode::RuntimePromotionContended | ErrorCode::RuntimePromotionWaitTimeout)
    ) || details["execution_step_disposition"] == "runner_ineligible"
        || details["admission_wait"] == crate::parsed_command_preflight::READINESS_UNOBSERVED
        || details["daemon_transport_error"]["kind"] == "timeout"
        || details["substrate_wait"] == "runner_connect"
        || details["substrate_wait"] == "runner_restarting"
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
pub fn wait_through<T>(
    label: &str,
    deadline: Duration,
    cancelled: impl Fn() -> bool,
    mut on_wait: impl FnMut(&Error, Duration),
    mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
    let started = Instant::now();
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let error = match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let remaining = deadline.saturating_sub(started.elapsed());
        if !is_transient_error(&error) || remaining.is_zero() || cancelled() {
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
                true,
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
