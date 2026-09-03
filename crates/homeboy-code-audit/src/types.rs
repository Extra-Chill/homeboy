//! Core audit result types and timing primitives.
//!
//! Mechanically split out of `mod.rs`; the public API is preserved by the
//! re-export in the module root.

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;

use super::fingerprint;

// Audit result value types now live in the shared audit contract so both the
// audit engine (producer) and the refactor engine (consumer/reconstructor) can
// depend on them without a cross-engine edge. Re-exported here so existing
// `crate::types::X` and `crate::X` paths keep resolving.
pub use homeboy_audit_contract::{
    AuditSummary, CodeAuditResult, ConventionReport, DirectoryConvention, DirectoryOutlier,
};

/// Shared analysis state built during an audit run and reused by downstream
/// consumers that would otherwise re-walk and re-fingerprint the codebase.
#[derive(Debug, Clone, Default)]
pub(crate) struct AuditAnalysisContext {
    pub(crate) fingerprints: Vec<fingerprint::FileFingerprint>,
    pub(crate) dead_code_references: Option<super::reference::DeadCodeReferenceAnalysis>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuditWithAnalysis {
    pub(crate) result: CodeAuditResult,
    pub(crate) analysis: AuditAnalysisContext,
    pub timing: AuditTiming,
}

/// Audit phase timing — a thin command-specific view over the generic core
/// [`PhaseTimer`](homeboy_engine_primitives::phase_timing::PhaseTimer) primitive.
///
/// Core owns the timing *contract*; audit supplies the phase vocabulary
/// (`source_snapshot`, `discovery_fingerprinting`, `detectors`,
/// `detector.<name>`, `baseline_comparison`, `report_assembly`). The serialized
/// shape (`spans[].{id,status,duration_ms}`) is preserved for the observation
/// metadata consumers in `commands/audit.rs`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AuditTiming {
    pub spans: Vec<AuditTimingSpan>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AuditTimingSpan {
    pub id: String,
    pub status: String,
    pub duration_ms: Option<f64>,
}

impl From<homeboy_engine_primitives::phase_timing::PhaseSpan> for AuditTimingSpan {
    fn from(span: homeboy_engine_primitives::phase_timing::PhaseSpan) -> Self {
        AuditTimingSpan {
            id: span.id,
            status: span.status.as_str().to_string(),
            duration_ms: span.duration_ms,
        }
    }
}

impl AuditTiming {
    /// Time a phase around a closure, recording its duration in the audit
    /// timing report. Used by the workflow layer to capture coarse phases
    /// (baseline comparison, report assembly) that sit outside the detector
    /// loop. Generic timing semantics are owned by
    /// [`PhaseTimer`](homeboy_engine_primitives::phase_timing::PhaseTimer).
    pub(crate) fn time_phase<T>(&mut self, id: impl Into<String>, run: impl FnOnce() -> T) -> T {
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::new();
        let value = timer.time_ok(id, run);
        self.extend_from_timer(timer);
        value
    }

    pub(super) fn push_ok(&mut self, id: impl Into<String>, duration: Duration) {
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::new();
        timer.record_ok(id, duration);
        self.extend_from_timer(timer);
    }

    pub(super) fn push_skipped(&mut self, id: impl Into<String>) {
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::new();
        timer.record_skipped(id);
        self.extend_from_timer(timer);
    }

    /// Drain a generic phase timer into the audit-facing span list.
    fn extend_from_timer(&mut self, timer: homeboy_engine_primitives::phase_timing::PhaseTimer) {
        self.spans.extend(
            timer
                .into_report()
                .spans
                .into_iter()
                .map(AuditTimingSpan::from),
        );
    }

    /// Emit the recorded detector spans to stderr, slowest first.
    ///
    /// These spans were already collected and already serialized — the audit
    /// result carries them as `timing.spans[]` (see
    /// [`AuditCommandOutput`](crate::report::AuditCommandOutput)) and the CLI
    /// copies them into observation metadata as `timing.spans`. Both are
    /// post-mortem artifacts: when the detector phase overruns its CI budget the
    /// run is cancelled and neither is ever written, which is how a 507-second
    /// audit managed to report nothing at all about where the 507 seconds went
    /// (#12583). This prints as soon as the phase ends, on the stream an operator
    /// is already watching.
    ///
    /// Deliberately `eprintln!` rather than `log_status!`: that macro is gated on
    /// stderr being a terminal (see `audit_log.rs`), and CI — the one place this
    /// measurement is needed — is never a terminal. The `[audit] ` prefix and
    /// phrasing match `log_status!("audit", ...)` so the lines read identically
    /// alongside the rest of the audit's progress output, and it matches how
    /// [`time_audit_detector`] already emits its per-detector lines.
    ///
    /// Only `detector.*` spans are ranked. The coarse phases
    /// (`discovery_fingerprinting`, `detectors`, `report`) are aggregates that
    /// contain the detector spans, so mixing them into one ranking would
    /// double-count. The `detectors` aggregate is reported separately as the
    /// phase's wall time, which is what the CI budget is spent against.
    pub(crate) fn log_detector_summary(&self) {
        let mut ranked: Vec<(&str, f64)> = self
            .spans
            .iter()
            .filter(|span| span.id.starts_with(DETECTOR_SPAN_PREFIX))
            .filter_map(|span| span.duration_ms.map(|ms| (span.id.as_str(), ms)))
            .collect();
        if ranked.is_empty() {
            return;
        }

        let skipped = self
            .spans
            .iter()
            .filter(|span| span.id.starts_with(DETECTOR_SPAN_PREFIX) && span.status == "skipped")
            .count();
        // Duration descending, then id ascending. The id tiebreak is what keeps
        // the ranking stable when several detectors report the same duration —
        // without it, equal-cost spans would order by whichever thread happened
        // to record first.
        ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));

        let summed: f64 = ranked.iter().map(|(_, ms)| *ms).sum();
        let wall = self
            .spans
            .iter()
            .rev()
            .find(|span| span.id == DETECTOR_PHASE_SPAN_ID)
            .and_then(|span| span.duration_ms);

        match wall {
            // `summed` exceeding `wall` is the expected shape once detectors run
            // concurrently, and the gap between the two IS the parallel speedup.
            Some(wall) => eprintln!(
                "[audit] Detector timing: {} wall, {} summed across {} span(s) ({} skipped)",
                format_millis(wall),
                format_millis(summed),
                ranked.len(),
                skipped
            ),
            None => eprintln!(
                "[audit] Detector timing: {} summed across {} span(s) ({} skipped)",
                format_millis(summed),
                ranked.len(),
                skipped
            ),
        }

        for (rank, (id, ms)) in ranked.iter().take(DETECTOR_SUMMARY_TOP_N).enumerate() {
            eprintln!(
                "[audit] Detector timing:   {}. {} {}",
                rank + 1,
                id,
                format_millis(*ms)
            );
        }

        let remaining = ranked.len().saturating_sub(DETECTOR_SUMMARY_TOP_N);
        if remaining > 0 {
            let tail: f64 = ranked
                .iter()
                .skip(DETECTOR_SUMMARY_TOP_N)
                .map(|(_, ms)| *ms)
                .sum();
            eprintln!(
                "[audit] Detector timing:   + {remaining} more span(s), {} total",
                format_millis(tail)
            );
        }
    }
}

/// Timing-id prefix every per-detector span shares.
const DETECTOR_SPAN_PREFIX: &str = "detector.";

/// Timing id of the aggregate span covering the whole detector phase.
const DETECTOR_PHASE_SPAN_ID: &str = "detectors";

/// How many detector spans [`AuditTiming::log_detector_summary`] names
/// individually. The full audit records 40+ detector spans; one line each would
/// bury the hotspot the summary exists to expose, so the slowest few are named
/// and the rest are totalled on one line. Every span is still in the result JSON.
const DETECTOR_SUMMARY_TOP_N: usize = 8;

/// Render a fractional-millisecond duration at a scale an operator can read:
/// seconds once a span is expensive enough to matter to a phase budget, and a
/// decimal below 10ms so a sub-millisecond detector does not read as `0ms`.
fn format_millis(millis: f64) -> String {
    if millis >= 1000.0 {
        format!("{:.1}s", millis / 1000.0)
    } else if millis >= 10.0 {
        format!("{millis:.0}ms")
    } else {
        format!("{millis:.1}ms")
    }
}

#[derive(Debug)]
pub(super) struct ScopedAuditExecution<'a> {
    pub(super) file_filter: Option<&'a [String]>,
    pub(super) git_ref: Option<&'a str>,
    pub(super) changed_files: HashSet<String>,
}

impl<'a> ScopedAuditExecution<'a> {
    pub(super) fn new(file_filter: Option<&'a [String]>, git_ref: Option<&'a str>) -> Self {
        let changed_files = file_filter
            .unwrap_or_default()
            .iter()
            .cloned()
            .collect::<HashSet<_>>();

        Self {
            file_filter,
            git_ref,
            changed_files,
        }
    }

    pub(super) fn is_scoped(&self) -> bool {
        self.file_filter.is_some()
    }

    pub(super) fn changed_file_count(&self) -> usize {
        self.changed_files.len()
    }

    pub(super) fn impact_tracing_enabled(&self) -> bool {
        self.git_ref.is_some()
    }
}

pub(crate) fn time_audit_detector<T>(
    timing: &mut AuditTiming,
    id: &'static str,
    enabled: bool,
    run: impl FnOnce() -> T,
    skipped: impl FnOnce() -> T,
) -> T {
    if enabled {
        eprintln!("[audit] Running {id}...");
        emit_detector_progress(id, "running", 0.0);
        let started = std::time::Instant::now();
        let value = run_with_detector_heartbeat(id, started, run);
        let elapsed = started.elapsed();
        eprintln!(
            "[audit] Completed {id} in {:.0}ms",
            elapsed.as_secs_f64() * 1000.0
        );
        emit_detector_progress(id, "completed", elapsed.as_secs_f64() * 1000.0);
        timing.push_ok(id, elapsed);
        value
    } else {
        timing.push_skipped(id);
        skipped()
    }
}

/// Keep the active detector visible while its synchronous work runs. The audit
/// cannot safely cancel arbitrary detector code, so this heartbeat is an
/// operator-facing liveness signal rather than a timeout mechanism.
fn run_with_detector_heartbeat<T>(
    id: &str,
    started: std::time::Instant,
    run: impl FnOnce() -> T,
) -> T {
    let (completed, wait_for_completion) = mpsc::channel();
    std::thread::scope(|scope| {
        let heartbeat = scope.spawn(move || loop {
            match wait_for_completion.recv_timeout(Duration::from_secs(15)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => emit_detector_progress(
                    id,
                    "heartbeat",
                    started.elapsed().as_secs_f64() * 1000.0,
                ),
            }
        });
        let value = run();
        let _ = completed.send(());
        heartbeat.join().expect("detector heartbeat must not panic");
        value
    })
}

/// Emit detector lifecycle events even when stderr is not interactive, so CI
/// and a foreground review client can distinguish active work from a stall.
fn emit_detector_progress(id: &str, status: &str, elapsed_ms: f64) {
    eprintln!(
        "HOMEBOY_PROGRESS {}",
        detector_progress(id, status, elapsed_ms)
    );
}

fn detector_progress(id: &str, status: &str, elapsed_ms: f64) -> serde_json::Value {
    serde_json::json!({
        "phase": "audit_detectors",
        "current": id,
        "status": status,
        "elapsed_ms": elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::detector_progress;

    #[test]
    fn detector_progress_names_the_active_detector_and_state() {
        assert_eq!(
            detector_progress("detector.structural", "completed", 12.5),
            serde_json::json!({
                "phase": "audit_detectors",
                "current": "detector.structural",
                "status": "completed",
                "elapsed_ms": 12.5,
            })
        );
    }
}

/// [`time_audit_detector`] against a private timing report, returning the
/// detector's value together with the spans recorded for it.
///
/// This is what lets a detector run on a worker thread. `&mut AuditTiming` is a
/// single mutable borrow and cannot be shared across threads; a `Mutex` around it
/// would compile but would make span ORDER depend on completion order, which is
/// exactly the nondeterminism the parallel detector phase must not introduce.
/// Handing each detector its own report and concatenating the spans afterwards in
/// a fixed order keeps the timing report byte-identical to the serial one.
pub(crate) fn time_audit_detector_isolated<T>(
    id: &'static str,
    enabled: bool,
    run: impl FnOnce() -> T,
    skipped: impl FnOnce() -> T,
) -> (T, Vec<AuditTimingSpan>) {
    let mut timing = AuditTiming::default();
    let value = time_audit_detector(&mut timing, id, enabled, run, skipped);
    (value, timing.spans)
}

// ============================================================================
// Public API
// ============================================================================
