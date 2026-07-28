//! Bounded remote probes for read-only control-plane inspection (#10418).
//!
//! Read-only inspection commands (`runner status`, `runs list`,
//! `agent-task status`) reach the runner over SSH to enrich their answer. Those
//! probes used to run with no wall-clock bound at all, so a wedged Lab — one
//! whose filesystem or login shell is blocked behind a long-running prune or
//! cook — made the diagnostic surface unavailable exactly when an operator
//! needed it. A status command that never returns is strictly worse than one
//! that returns a partial answer naming what it could not reach.
//!
//! This module is the shared bound. It reuses the existing
//! [`SshClient::execute_with_timeout`] mechanism (wall-clock deadline plus
//! process-group termination of the SSH child) rather than inventing a new one,
//! and adds a per-invocation ledger so a probe that hits its deadline is
//! *reported* instead of being silently swallowed into "nothing to report".

use std::cell::RefCell;
use std::time::Duration;

use serde::Serialize;

/// Default wall-clock bound for a single read-only remote probe.
///
/// Matches the existing `REMOTE_DAEMON_STATUS_TIMEOUT` used by the remote
/// daemon status probe so the read-only surface has one consistent budget.
pub const DEFAULT_READONLY_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Environment override for [`readonly_probe_timeout`], in whole seconds.
pub const READONLY_PROBE_TIMEOUT_ENV: &str = "HOMEBOY_READONLY_PROBE_TIMEOUT_SECONDS";

/// The wall-clock bound applied to each read-only remote probe.
///
/// A zero or unparseable override is ignored: a read command must always have a
/// deadline, and "0" would otherwise reintroduce the unbounded hang.
pub fn readonly_probe_timeout() -> Duration {
    readonly_probe_timeout_from(std::env::var(READONLY_PROBE_TIMEOUT_ENV).ok().as_deref())
}

/// Resolve the probe bound from a raw override value. Split out from the
/// environment read so the "never unbounded" contract is deterministically
/// testable.
fn readonly_probe_timeout_from(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_READONLY_PROBE_TIMEOUT)
}

/// One read-only probe that did not complete within its bound.
///
/// Emitted alongside the (partial) inspection result so an operator can tell
/// "the Lab is wedged" from "there is nothing to report".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadOnlyProbeDegradation {
    /// Stable label for the probe that was bounded, e.g. `runner_homeboy_identity`.
    pub probe: String,
    /// The runner whose remote endpoint was being inspected, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// Machine-readable classification of the degradation.
    pub reason_code: &'static str,
    /// The bound that fired, in seconds. `0` when the probe never reached its
    /// bound because it could not run at all
    /// (see [`REASON_PROBE_UNAVAILABLE`]).
    pub timeout_seconds: u64,
    /// Operator-facing explanation of what is missing from the result.
    pub detail: String,
}

/// Probe exceeded its wall-clock bound; the SSH child process group was killed.
pub const REASON_PROBE_TIMEOUT: &str = "readonly_probe.timeout";
/// Probe was cut short because the caller cancelled (SIGINT/SIGTERM).
pub const REASON_PROBE_INTERRUPTED: &str = "readonly_probe.interrupted";
/// Probe could not run at all in the caller's environment — a required tool is
/// missing, or the ambient context it reads (a git checkout, say) is absent.
/// Distinct from a timeout: no bound ever applied, so `timeout_seconds` is `0`.
/// The subprocess diagnostic that explains it belongs in `detail`, so a
/// read-only command can drop the ambient failure from its *streams* without
/// dropping it from its *answer* (#10525).
pub const REASON_PROBE_UNAVAILABLE: &str = "readonly_probe.unavailable";

// Per-thread ledger, mirroring the existing `ACTIVE_PROBE_LIMITS` design in
// `homeboy-core`'s SSH client. Inspection commands probe and drain on the same
// thread, so a degradation always travels with the answer that lost it.
thread_local! {
    static DEGRADATIONS: RefCell<Vec<ReadOnlyProbeDegradation>> =
        const { RefCell::new(Vec::new()) };
}

/// Record that a bounded read-only probe degraded. Deduplicated by
/// (probe, runner_id) so a repeated probe against one wedged runner reports
/// once rather than flooding the result.
pub fn record_degradation(degradation: ReadOnlyProbeDegradation) {
    DEGRADATIONS.with(|ledger| {
        let mut ledger = ledger.borrow_mut();
        if ledger.iter().any(|recorded| {
            recorded.probe == degradation.probe && recorded.runner_id == degradation.runner_id
        }) {
            return;
        }
        ledger.push(degradation);
    });
}

/// Record a degradation for a bounded probe whose output reports a timeout or a
/// caller cancellation. Returns `true` when something was recorded, so callers
/// can branch on "this answer is partial".
pub fn record_probe_outcome(
    probe: &str,
    runner_id: Option<&str>,
    timeout: Duration,
    output: &homeboy_core::server::CommandOutput,
) -> bool {
    // `stderr_with_interruption` is Homeboy's own marker for a cancelled child;
    // matching it keeps cancellation distinguishable from a plain non-zero exit
    // without widening `CommandOutput`.
    let interrupted = output.stderr.contains("Homeboy interrupted by signal");
    if !output.timed_out && !interrupted {
        return false;
    }
    let reason_code = if output.timed_out {
        REASON_PROBE_TIMEOUT
    } else {
        REASON_PROBE_INTERRUPTED
    };
    let detail = if output.timed_out {
        format!(
            "read-only probe `{probe}` exceeded its {}s bound and was terminated; the runner did not answer, so this result is partial. Set {READONLY_PROBE_TIMEOUT_ENV} to change the bound.",
            timeout.as_secs()
        )
    } else {
        format!("read-only probe `{probe}` was cancelled by the caller before the runner answered; this result is partial.")
    };
    record_degradation(ReadOnlyProbeDegradation {
        probe: probe.to_string(),
        runner_id: runner_id.map(str::to_string),
        reason_code,
        timeout_seconds: timeout.as_secs(),
        detail,
    });
    true
}

/// Read the degradations recorded so far without clearing them.
pub fn degradations() -> Vec<ReadOnlyProbeDegradation> {
    DEGRADATIONS.with(|ledger| ledger.borrow().clone())
}

/// Drain the recorded degradations. Inspection commands call this once while
/// assembling their output so the partial-result reason travels with the
/// answer.
pub fn take_degradations() -> Vec<ReadOnlyProbeDegradation> {
    DEGRADATIONS.with(|ledger| std::mem::take(&mut *ledger.borrow_mut()))
}

#[cfg(test)]
pub(crate) fn clear_degradations() {
    let _ = take_degradations();
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::server::CommandOutput;

    fn output(timed_out: bool, stderr: &str) -> CommandOutput {
        CommandOutput {
            stdout: String::new(),
            stderr: stderr.to_string(),
            success: false,
            exit_code: if timed_out { 124 } else { -1 },
            timed_out,
            child_resource: None,
        }
    }

    #[test]
    fn a_timed_out_probe_is_recorded_with_an_operator_facing_reason() {
        clear_degradations();

        let recorded = record_probe_outcome(
            "runner_homeboy_identity",
            Some("homeboy-lab"),
            Duration::from_secs(15),
            &output(true, "Homeboy remote probe timed out after 15000ms"),
        );

        assert!(recorded);
        let degradations = take_degradations();
        assert_eq!(degradations.len(), 1);
        assert_eq!(degradations[0].probe, "runner_homeboy_identity");
        assert_eq!(degradations[0].runner_id.as_deref(), Some("homeboy-lab"));
        assert_eq!(degradations[0].reason_code, REASON_PROBE_TIMEOUT);
        assert_eq!(degradations[0].timeout_seconds, 15);
        assert!(degradations[0].detail.contains("partial"));
    }

    #[test]
    fn a_cancelled_probe_is_distinguished_from_a_timeout() {
        clear_degradations();

        assert!(record_probe_outcome(
            "runner_homeboy_identity",
            Some("homeboy-lab"),
            Duration::from_secs(15),
            &output(
                false,
                "Homeboy interrupted by signal 15; terminated child process group"
            ),
        ));

        let degradations = take_degradations();
        assert_eq!(degradations.len(), 1);
        assert_eq!(degradations[0].reason_code, REASON_PROBE_INTERRUPTED);
    }

    #[test]
    fn a_healthy_probe_records_nothing() {
        clear_degradations();

        assert!(!record_probe_outcome(
            "runner_homeboy_identity",
            Some("homeboy-lab"),
            Duration::from_secs(15),
            &output(false, ""),
        ));
        assert!(take_degradations().is_empty());
    }

    #[test]
    fn repeated_degradations_for_one_runner_report_once() {
        clear_degradations();

        for _ in 0..3 {
            record_probe_outcome(
                "runner_homeboy_identity",
                Some("homeboy-lab"),
                Duration::from_secs(15),
                &output(true, ""),
            );
        }

        assert_eq!(take_degradations().len(), 1);
    }

    #[test]
    fn the_probe_bound_is_never_unbounded_and_honors_a_positive_override() {
        assert_eq!(
            readonly_probe_timeout_from(None),
            DEFAULT_READONLY_PROBE_TIMEOUT
        );
        // "0" would reintroduce the unbounded hang this bound exists to prevent.
        assert_eq!(
            readonly_probe_timeout_from(Some("0")),
            DEFAULT_READONLY_PROBE_TIMEOUT
        );
        assert_eq!(
            readonly_probe_timeout_from(Some("not-a-number")),
            DEFAULT_READONLY_PROBE_TIMEOUT
        );
        assert_eq!(
            readonly_probe_timeout_from(Some(" 3 ")),
            Duration::from_secs(3)
        );
    }
}
