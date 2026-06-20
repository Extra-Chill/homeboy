//! Generic phase-level timing spans for hot command workflows.
//!
//! Hot Homeboy commands (audit, bench, lint, test, trace, refactor, runner
//! offload) need to record *where* time is spent so perf regressions can be
//! attributed to a phase — core orchestration, extension execution, runner
//! sync, or report rendering — rather than a single wall-clock number.
//!
//! Boundary: core owns the *contract* (a `PhaseTimer` that captures named
//! phases with durations and finalizes on success or failure), not the phase
//! vocabulary. Callers supply their own phase labels; the recommended generic
//! labels at the core boundary are `resolve`, `prepare`, `execute`, `parse`,
//! `report`, and `persist`, and extensions/command modules may add more
//! specific labels under their own namespace (e.g. `detector.structural`).
//!
//! The report serializes into run-dir artifacts / observation metadata without
//! changing any existing structured-output shape: a `PhaseTimingReport` is only
//! materialized when a caller explicitly reads or persists it.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Outcome of a timed phase.
///
/// `Ok` and `Failed` both carry a duration (work happened, regardless of
/// whether it succeeded). `Skipped` carries no duration because no work ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Ok,
    Skipped,
    Failed,
}

impl PhaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseStatus::Ok => "ok",
            PhaseStatus::Skipped => "skipped",
            PhaseStatus::Failed => "failed",
        }
    }
}

/// A single recorded phase: a caller-supplied id, an outcome, and (when work
/// ran) how long it took in fractional milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseSpan {
    pub id: String,
    pub status: PhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

impl PhaseSpan {
    fn new(id: impl Into<String>, status: PhaseStatus, duration: Option<Duration>) -> Self {
        Self {
            id: id.into(),
            status,
            duration_ms: duration.map(duration_to_millis),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == PhaseStatus::Ok
    }
}

/// An ordered set of recorded phase spans, ready to embed in run metadata.
///
/// Order is preserved in the sequence phases were recorded, which mirrors the
/// execution order of a command workflow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseTimingReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<PhaseSpan>,
}

impl PhaseTimingReport {
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Total wall-clock recorded across phases that ran (`ok` + `failed`).
    ///
    /// Phases nest in many workflows, so this is an upper-bound sum rather than
    /// a strict end-to-end duration; callers that need a true total should
    /// record an explicit top-level phase.
    pub fn total_ms(&self) -> f64 {
        self.spans.iter().filter_map(|span| span.duration_ms).sum()
    }

    /// Look up a recorded span by id.
    pub fn span(&self, id: &str) -> Option<&PhaseSpan> {
        self.spans.iter().find(|span| span.id == id)
    }
}

/// Collects phase spans for a single command workflow.
///
/// The timer is the generic primitive every hot command can reuse. It records
/// durations the moment a phase completes, so partial data survives an early
/// return or a hard failure mid-workflow — call `record_failed` (or use a
/// [`PhaseGuard`]) so timing is still finalized on the error path.
#[derive(Debug, Clone, Default)]
pub struct PhaseTimer {
    spans: Vec<PhaseSpan>,
}

impl PhaseTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed phase that ran successfully.
    pub fn record_ok(&mut self, id: impl Into<String>, duration: Duration) {
        self.spans
            .push(PhaseSpan::new(id, PhaseStatus::Ok, Some(duration)));
    }

    /// Record a phase that ran but failed. The duration is still captured so a
    /// regression on the error path is attributable.
    pub fn record_failed(&mut self, id: impl Into<String>, duration: Duration) {
        self.spans
            .push(PhaseSpan::new(id, PhaseStatus::Failed, Some(duration)));
    }

    /// Record a phase that was intentionally not run (no work, no duration).
    pub fn record_skipped(&mut self, id: impl Into<String>) {
        self.spans
            .push(PhaseSpan::new(id, PhaseStatus::Skipped, None));
    }

    /// Time a closure as a single phase. The duration is recorded whether the
    /// closure returns `Ok` or `Err`, so timing data survives the failure path.
    pub fn time<T, E>(
        &mut self,
        id: impl Into<String>,
        run: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let started = Instant::now();
        let outcome = run();
        let elapsed = started.elapsed();
        let id = id.into();
        match &outcome {
            Ok(_) => self.record_ok(id, elapsed),
            Err(_) => self.record_failed(id, elapsed),
        }
        outcome
    }

    /// Time an infallible closure as a single phase.
    pub fn time_ok<T>(&mut self, id: impl Into<String>, run: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = run();
        self.record_ok(id, started.elapsed());
        value
    }

    /// Start a scoped phase guard. The guard records the phase on drop, so an
    /// early return or panic between `start` and the end of the scope still
    /// finalizes timing. The phase is recorded as `ok` unless explicitly marked
    /// failed (see [`PhaseGuard::mark_failed`]).
    pub fn start(&mut self, id: impl Into<String>) -> PhaseGuard<'_> {
        PhaseGuard {
            timer: self,
            id: id.into(),
            started: Instant::now(),
            status: PhaseStatus::Ok,
            disarmed: false,
        }
    }

    /// Number of recorded phases.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Finalize into a serializable report for run metadata.
    pub fn into_report(self) -> PhaseTimingReport {
        PhaseTimingReport { spans: self.spans }
    }

    /// Borrow the current spans without consuming the timer.
    pub fn report(&self) -> PhaseTimingReport {
        PhaseTimingReport {
            spans: self.spans.clone(),
        }
    }
}

/// RAII scope that records a phase span when dropped.
///
/// Guarantees timing is finalized even on an early return or a panic — the key
/// requirement for the error path. Defaults to `ok`; call [`mark_failed`] to
/// record the phase as failed instead, or [`disarm`] to drop without recording.
///
/// [`mark_failed`]: PhaseGuard::mark_failed
/// [`disarm`]: PhaseGuard::disarm
pub struct PhaseGuard<'a> {
    timer: &'a mut PhaseTimer,
    id: String,
    started: Instant,
    status: PhaseStatus,
    disarmed: bool,
}

impl PhaseGuard<'_> {
    /// Mark the in-flight phase as failed; it will be recorded as `failed`
    /// (with its duration) when the guard is dropped.
    pub fn mark_failed(&mut self) {
        self.status = PhaseStatus::Failed;
    }

    /// Drop the guard without recording a span (e.g. the phase was skipped
    /// after the guard was created).
    pub fn disarm(mut self) {
        self.disarmed = true;
    }
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let elapsed = self.started.elapsed();
        let id = std::mem::take(&mut self.id);
        match self.status {
            PhaseStatus::Failed => self.timer.record_failed(id, elapsed),
            // Skipped never carries a duration; a guard always timed work.
            _ => self.timer.record_ok(id, elapsed),
        }
    }
}

fn duration_to_millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_ok_and_skipped_in_order() {
        let mut timer = PhaseTimer::new();
        timer.record_ok("execute", Duration::from_millis(5));
        timer.record_skipped("report");

        let report = timer.into_report();
        let ids: Vec<&str> = report.spans.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["execute", "report"]);
        assert_eq!(report.spans[0].status, PhaseStatus::Ok);
        assert!(report.spans[0].duration_ms.unwrap() >= 5.0);
        assert_eq!(report.spans[1].status, PhaseStatus::Skipped);
        assert_eq!(report.spans[1].duration_ms, None);
    }

    #[test]
    fn time_records_duration_on_success() {
        let mut timer = PhaseTimer::new();
        let value: Result<i32, ()> = timer.time("execute", || Ok(7));
        assert_eq!(value, Ok(7));
        let report = timer.into_report();
        assert_eq!(report.spans.len(), 1);
        assert_eq!(report.spans[0].status, PhaseStatus::Ok);
    }

    #[test]
    fn time_finalizes_timing_on_error_path() {
        let mut timer = PhaseTimer::new();
        let value: Result<i32, &str> = timer.time("execute", || Err("boom"));
        assert_eq!(value, Err("boom"));
        let report = timer.into_report();
        assert_eq!(report.spans.len(), 1);
        assert_eq!(report.spans[0].status, PhaseStatus::Failed);
        // Failed phases still carry a duration so regressions are attributable.
        assert!(report.spans[0].duration_ms.is_some());
    }

    #[test]
    fn guard_records_even_when_scope_exits_early() {
        // Simulates a workflow that bails out mid-phase: the guarded scope is
        // left without explicitly recording anything, yet the phase is still
        // finalized on drop — the key requirement for the error path.
        fn prepare(timer: &mut PhaseTimer, fail_early: bool) -> bool {
            let _guard = timer.start("prepare");
            if fail_early {
                // Drop runs here on the early-exit path, finalizing `prepare`.
                return false;
            }
            true
        }

        let mut early_timer = PhaseTimer::new();
        assert!(!prepare(&mut early_timer, true));
        let early = early_timer.into_report();
        assert_eq!(early.spans.len(), 1);
        assert_eq!(early.spans[0].id, "prepare");
        assert_eq!(early.spans[0].status, PhaseStatus::Ok);

        let mut full_timer = PhaseTimer::new();
        assert!(prepare(&mut full_timer, false));
        full_timer.record_ok("execute", Duration::from_millis(1));
        let full = full_timer.into_report();
        let ids: Vec<&str> = full.spans.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["prepare", "execute"]);
    }

    #[test]
    fn guard_can_be_marked_failed() {
        let mut timer = PhaseTimer::new();
        {
            let mut guard = timer.start("execute");
            guard.mark_failed();
        }
        let report = timer.into_report();
        assert_eq!(report.spans[0].status, PhaseStatus::Failed);
    }

    #[test]
    fn disarmed_guard_records_nothing() {
        let mut timer = PhaseTimer::new();
        timer.start("execute").disarm();
        assert!(timer.is_empty());
    }

    #[test]
    fn report_total_and_lookup() {
        let mut timer = PhaseTimer::new();
        timer.record_ok("resolve", Duration::from_millis(2));
        timer.record_ok("execute", Duration::from_millis(3));
        timer.record_skipped("report");
        let report = timer.into_report();

        assert!((report.total_ms() - 5.0).abs() < 1.0);
        assert!(report.span("resolve").is_some());
        assert!(report.span("missing").is_none());
        assert_eq!(report.span("report").unwrap().status, PhaseStatus::Skipped);
    }

    #[test]
    fn report_round_trips_through_json() {
        let mut timer = PhaseTimer::new();
        timer.record_ok("execute", Duration::from_millis(4));
        timer.record_skipped("persist");
        let report = timer.into_report();

        let json = serde_json::to_string(&report).unwrap();
        let restored: PhaseTimingReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, restored);
    }

    #[test]
    fn empty_report_serializes_without_spans_field() {
        let report = PhaseTimingReport::default();
        let json = serde_json::to_string(&report).unwrap();
        assert_eq!(json, "{}");
    }
}
