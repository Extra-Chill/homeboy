//! Per-component deploy phase timings (#16804).
//!
//! Each deploy step records how long it took on the component's
//! [`PhaseTimer`] and emits a `phase=<id> … duration_ms=<n>` status line, so a
//! slow deploy can be attributed from `results[].phase_timings` or from CI
//! logs (`HOMEBOY_STATUS=1`).

use std::time::Instant;

use homeboy_core::phase_timing::PhaseTimer;

/// Record `phase` as finished now, started at `started`. `subject` identifies
/// what was timed (e.g. `component=foo` or `target=/srv/site/foo`).
pub(crate) fn record(timer: &mut PhaseTimer, subject: &str, phase: &str, started: Instant) {
    let elapsed = started.elapsed();
    homeboy_core::log_status!(
        "deploy",
        "phase={phase} {subject} duration_ms={}",
        elapsed.as_millis()
    );
    timer.record_ok(phase, elapsed);
}

/// Time `work` as `phase` on `timer`.
pub(crate) fn time<T>(
    timer: &mut PhaseTimer,
    subject: &str,
    phase: &str,
    work: impl FnOnce() -> T,
) -> T {
    let started = Instant::now();
    let value = work();
    record(timer, subject, phase, started);
    value
}

/// Status-line subject for a component.
pub(crate) fn component_subject(id: &str) -> String {
    format!("component={id}")
}
