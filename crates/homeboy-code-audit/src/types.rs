//! Core audit result types and timing primitives.
//!
//! Mechanically split out of `mod.rs`; the public API is preserved by the
//! re-export in the module root.

use std::collections::HashSet;
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
        let started = std::time::Instant::now();
        let value = run();
        let elapsed = started.elapsed();
        eprintln!(
            "[audit] Completed {id} in {:.0}ms",
            elapsed.as_secs_f64() * 1000.0
        );
        timing.push_ok(id, elapsed);
        value
    } else {
        timing.push_skipped(id);
        skipped()
    }
}

// ============================================================================
// Public API
// ============================================================================
