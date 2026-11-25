//! Generic phase-level timing spans for hot command workflows.
//!
//! Hot Homeboy commands (audit, bench, lint, test, trace, refactor, runner
//! offload) need to record *where* time is spent so perf regressions can be
//! attributed to a phase — core orchestration, extension execution, runner
//! sync, or report rendering — rather than a single wall-clock number.
//!
//! Boundary: this module owns the *contract* (a `PhaseTimer` that captures named
//! phases with durations and finalizes on success or failure), not the phase
//! vocabulary. Callers supply their own phase labels; the recommended generic
//! labels at the core boundary are `admit`, `resolve`, `stage`, `provider`,
//! `gate`, `promote`, and `publish`, and extensions/command modules may add more
//! specific labels under their own namespace (e.g. `detector.structural`).
//!
//! The report serializes into run-dir artifacts / observation metadata without
//! changing any existing structured-output shape: a `PhaseTimingReport` is only
//! materialized when a caller explicitly reads or persists it.

use chrono::{SecondsFormat, Utc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use super::resource_usage::ResourceUsage;

/// Generic run phase vocabulary. Domain sub-phases are namespaced under their
/// owner, e.g. `stage.transfer` or `gate.cargo-test`.
pub const PHASE_ADMIT: &str = "admit";
pub const PHASE_RESOLVE: &str = "resolve";
pub const PHASE_STAGE: &str = "stage";
pub const PHASE_PROVIDER: &str = "provider";
pub const PHASE_GATE: &str = "gate";
pub const PHASE_PROMOTE: &str = "promote";
pub const PHASE_PUBLISH: &str = "publish";
pub const GENERIC_PHASES: [&str; 7] = [
    PHASE_ADMIT,
    PHASE_RESOLVE,
    PHASE_STAGE,
    PHASE_PROVIDER,
    PHASE_GATE,
    PHASE_PROMOTE,
    PHASE_PUBLISH,
];

/// Run event kind carrying a [`PhaseTimingReport`] in a run's durable ledger.
pub const PHASE_SPANS_EVENT_KIND: &str = "run.phase_spans";
/// Maximum spans one ledger event may carry, keeping the event within the
/// run event payload bound.
pub const MAX_PHASE_SPANS_PER_EVENT: usize = 64;

/// Join an owner phase and a domain sub-phase (`stage` + `transfer`).
pub fn sub_phase(owner: &str, name: &str) -> String {
    format!("{owner}.{name}")
}

/// The generic phase that owns `id` (`stage.transfer` -> `stage`).
pub fn owner_phase(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

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
    /// Attempt identity, when carried by the recorded span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub status: PhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// Absolute start on the shared run timeline (RFC 3339, milliseconds).
    pub started_at: String,
    /// Version of the executable recording the span (not the primitive crate).
    #[serde(default)]
    pub homeboy_version: String,
    /// What the step or phase consumed, recorded when it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResourceUsage>,
}

impl PhaseSpan {
    fn new(id: impl Into<String>, status: PhaseStatus, duration: Option<Duration>) -> Self {
        Self {
            id: id.into(),
            attempt_id: None,
            status,
            duration_ms: duration.map(duration_to_millis),
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            homeboy_version: String::new(),
            usage: None,
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

    /// Look up a recorded span by id.
    pub fn span(&self, id: &str) -> Option<&PhaseSpan> {
        self.spans.iter().find(|span| span.id == id)
    }
}

/// Collects phase spans for a single command workflow.
///
/// The timer is the generic primitive every hot command can reuse. It records
/// durations the moment a phase completes, so partial data survives an early
/// return or a hard failure mid-workflow — call `record_failed` so timing is
/// still finalized on the error path, or use a [`PhaseGuard`] to finalize an
/// `ok` span automatically on scope exit.
pub type SpanSink = std::sync::Arc<dyn Fn(&PhaseSpan) + Send + Sync>;

thread_local! {
    static SINK: std::cell::RefCell<Option<SpanSink>> = const { std::cell::RefCell::new(None) };
}

/// Install the run's persistence sink, returning the previous sink for restoration.
pub fn replace_sink(sink: Option<SpanSink>) -> Option<SpanSink> {
    SINK.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), sink))
}

#[derive(Clone)]
pub struct PhaseTimer {
    spans: Vec<PhaseSpan>,
    homeboy_version: String,
    sink: Option<SpanSink>,
    scope: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for PhaseTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseTimer")
            .field("spans", &self.spans)
            .finish_non_exhaustive()
    }
}

impl Default for PhaseTimer {
    fn default() -> Self {
        Self {
            spans: Vec::new(),
            homeboy_version: homeboy_product_identity::product_version().to_string(),
            sink: None,
            scope: None,
        }
    }
}

impl PhaseTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record into the active run instead of collecting a buffered report.
    pub fn current() -> Self {
        Self {
            sink: SINK.with(|slot| slot.borrow().clone()),
            ..Self::default()
        }
    }

    /// Keep a run context alive for the lifetime of this timer and its guard.
    pub fn with_scope(mut self, scope: impl std::any::Any + Send + Sync) -> Self {
        self.scope = Some(std::sync::Arc::new(scope));
        self
    }

    pub fn into_guard(self, id: impl Into<String>) -> PhaseGuard<'static> {
        PhaseGuard {
            timer: GuardTimer::Owned(self),
            id: id.into(),
            started: Instant::now(),
            usage: Some(super::resource_usage::usage_scope()),
        }
    }

    /// Record work measured outside a closure.
    pub fn finish(&mut self, id: impl Into<String>, started: Instant, failed: bool) {
        self.record(
            id,
            if failed {
                PhaseStatus::Failed
            } else {
                PhaseStatus::Ok
            },
            Some(started.elapsed()),
        );
    }

    /// Time a result whose successful value may encode a domain failure.
    pub fn time_with_outcome<T, E>(
        &mut self,
        id: impl Into<String>,
        run: impl FnOnce() -> Result<T, E>,
        failed: impl FnOnce(&T) -> bool,
    ) -> Result<T, E> {
        let mut guard = self.start(id);
        let result = run();
        guard.complete(result.as_ref().map(failed).unwrap_or(true));
        result
    }

    /// Create a timer attributed to the recording executable's version.
    pub fn with_version(version: impl Into<String>) -> Self {
        Self {
            homeboy_version: version.into(),
            ..Self::default()
        }
    }

    fn record(&mut self, id: impl Into<String>, status: PhaseStatus, duration: Option<Duration>) {
        self.record_with_usage(id, status, duration, None);
    }

    fn record_with_usage(
        &mut self,
        id: impl Into<String>,
        status: PhaseStatus,
        duration: Option<Duration>,
        usage: Option<ResourceUsage>,
    ) {
        let mut span = PhaseSpan::new(id, status, duration);
        span.usage = usage;
        span.started_at = (Utc::now()
            - chrono::Duration::from_std(duration.unwrap_or_default()).unwrap_or_default())
        .to_rfc3339_opts(SecondsFormat::Millis, true);
        span.homeboy_version = self.homeboy_version.clone();
        if let Some(sink) = &self.sink {
            sink(&span);
        } else {
            self.spans.push(span);
        }
    }

    /// Record a completed phase that ran successfully.
    pub fn record_ok(&mut self, id: impl Into<String>, duration: Duration) {
        self.record(id, PhaseStatus::Ok, Some(duration));
    }

    /// Record a phase that ran but failed. The duration is still captured so a
    /// regression on the error path is attributable.
    pub fn record_failed(&mut self, id: impl Into<String>, duration: Duration) {
        self.record(id, PhaseStatus::Failed, Some(duration));
    }

    /// Record a phase that was intentionally not run (no work, no duration).
    pub fn record_skipped(&mut self, id: impl Into<String>) {
        self.record(id, PhaseStatus::Skipped, None);
    }

    /// Time a closure as a single phase. The duration is recorded whether the
    /// closure returns `Ok` or `Err`, so timing data survives the failure path.
    pub fn time<T, E>(
        &mut self,
        id: impl Into<String>,
        run: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        self.time_with_outcome(id, run, |_| false)
    }

    /// Time an infallible closure as a single phase.
    pub fn time_ok<T>(&mut self, id: impl Into<String>, run: impl FnOnce() -> T) -> T {
        let _guard = self.start(id);
        run()
    }

    /// Start a scoped phase guard. The guard records the phase on drop, so an
    /// early return or panic between `start` and the end of the scope still
    /// finalizes timing. The phase is recorded as `ok`, or dropped without
    /// recording via [`PhaseGuard::disarm`].
    pub fn start(&mut self, id: impl Into<String>) -> PhaseGuard<'_> {
        PhaseGuard {
            timer: GuardTimer::Borrowed(self),
            id: id.into(),
            started: Instant::now(),
            usage: Some(super::resource_usage::usage_scope()),
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
/// requirement for the error path. Records `ok` on drop, `failed` during unwinding, or
/// drops without recording via [`disarm`].
///
/// [`disarm`]: PhaseGuard::disarm
enum GuardTimer<'a> {
    Borrowed(&'a mut PhaseTimer),
    Owned(PhaseTimer),
}

pub struct PhaseGuard<'a> {
    timer: GuardTimer<'a>,
    id: String,
    started: Instant,
    /// `None` once disarmed.
    usage: Option<super::resource_usage::UsageScope>,
}

impl PhaseGuard<'_> {
    fn complete(&mut self, failed: bool) {
        let Some(usage) = self.usage.take() else {
            return;
        };
        let timer = match &mut self.timer {
            GuardTimer::Borrowed(timer) => &mut **timer,
            GuardTimer::Owned(timer) => timer,
        };
        timer.record_with_usage(
            std::mem::take(&mut self.id),
            if failed {
                PhaseStatus::Failed
            } else {
                PhaseStatus::Ok
            },
            Some(self.started.elapsed()),
            usage.finish(),
        );
    }

    /// Drop the guard without recording a span (e.g. the phase was skipped
    /// after the guard was created).
    pub fn disarm(mut self) {
        if let Some(usage) = self.usage.take() {
            usage.discard();
        }
    }
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        self.complete(std::thread::panicking());
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
        let execute = report.span("execute").unwrap();
        assert_eq!(execute.status, PhaseStatus::Ok);
        assert!(execute.duration_ms.unwrap() >= 5.0);
        let skipped = report.span("report").unwrap();
        assert_eq!(skipped.status, PhaseStatus::Skipped);
        assert_eq!(skipped.duration_ms, None);
        assert!(report.span("missing").is_none());
    }

    #[test]
    fn time_records_status_and_duration_on_success_and_error() {
        let mut timer = PhaseTimer::new();
        let ok: Result<i32, &str> = timer.time("execute", || Ok(7));
        let err: Result<i32, &str> = timer.time("persist", || Err("boom"));
        assert_eq!(ok, Ok(7));
        assert_eq!(err, Err("boom"));
        let report = timer.into_report();
        let statuses: Vec<_> = report.spans.iter().map(|span| span.status).collect();
        assert_eq!(statuses, vec![PhaseStatus::Ok, PhaseStatus::Failed]);
        // Failed phases still carry a duration so regressions are attributable.
        assert!(report.spans[1].duration_ms.is_some());
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
    fn disarmed_guard_records_nothing() {
        let mut timer = PhaseTimer::new();
        timer.start("execute").disarm();
        assert!(timer.is_empty());
    }

    #[test]
    fn report_round_trips_through_json_and_omits_empty_spans() {
        let mut timer = PhaseTimer::new();
        timer.record_ok("execute", Duration::from_millis(4));
        timer.record_skipped("persist");
        let report = timer.into_report();

        let json = serde_json::to_string(&report).unwrap();
        let restored: PhaseTimingReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, restored);
        assert_eq!(
            serde_json::to_string(&PhaseTimingReport::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn spans_carry_absolute_start_and_version() {
        let mut timer = PhaseTimer::with_version("9.9.9");
        timer.time_ok(PHASE_ADMIT, || std::thread::sleep(Duration::from_millis(2)));
        timer.time_ok(sub_phase(PHASE_STAGE, "transfer"), || ());
        let report = timer.into_report();
        assert_eq!(report.spans[0].homeboy_version, "9.9.9");
        assert!(report.spans[1].started_at >= report.spans[0].started_at);
        assert_eq!(owner_phase(&report.spans[1].id), PHASE_STAGE);
    }

    #[test]
    fn spans_require_absolute_start() {
        assert!(serde_json::from_str::<PhaseSpan>(
            r#"{"id":"execute","status":"ok","duration_ms":1.0}"#
        )
        .is_err());
        let mut timer = PhaseTimer::new();
        timer.record_ok("execute", Duration::from_secs(1));
        let span = &timer.report().spans[0];
        assert!(span.started_at.ends_with('Z'));
        assert_eq!(span.started_at.split('.').nth(1).unwrap().len(), 4);
    }
}
