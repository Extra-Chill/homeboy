//! Shared workflow types — args, results, and summary structures.

use crate::lint::baseline as lint_baseline;
use crate::self_check::SelfCheckCaptureMetadata;
use crate::ExtensionPhaseTiming;
use homeboy_core::finding::{FindingProducerSummary, HomeboyFinding};
use homeboy_engine_primitives::baseline::BaselineFlags;
pub use homeboy_extension_contract::FormattingFindings;
pub use homeboy_extension_contract::LintSummaryOutput;
use homeboy_refactor_contract::AppliedRefactor;
use serde::Serialize;
use std::collections::BTreeMap;

/// Sniff-selection filters shared by every lint entry point.
///
/// The CLI args (`LintArgs`), the workflow args (`LintRunWorkflowArgs`), and
/// the refactor-source options (`LintSourceOptions`) all carry the same
/// `errors_only` / `sniffs` / `exclude_sniffs` triplet. Extracting it into one
/// composed struct keeps that contract defined in a single place instead of
/// being re-declared field-by-field across layers.
#[derive(Debug, Clone, Default)]
pub struct LintSniffFilters {
    /// Show only errors, suppress warnings.
    pub errors_only: bool,
    /// Only check specific sniffs (comma-separated codes).
    pub sniffs: Option<String>,
    /// Exclude sniffs from checking (comma-separated codes).
    pub exclude_sniffs: Option<String>,
}

/// Arguments for the main lint workflow — populated by the command layer from CLI flags.
#[derive(Debug, Clone)]
pub struct LintRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, serde_json::Value)>,
    pub summary: bool,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub changed_only: bool,
    pub changed_since: Option<String>,
    pub precomputed_changed_files: Option<Vec<String>>,
    pub sniff_filters: LintSniffFilters,
    pub category: Option<String>,
    pub ci_env: Vec<(String, String)>,
    pub baseline_flags: BaselineFlags,
    pub json_summary: bool,
}

/// Result of the main lint workflow — ready for report assembly.
#[derive(Debug, Clone, Serialize)]
pub struct LintRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    /// True when the lint harness/wrapper itself failed (non-zero exit) while
    /// the underlying linter produced no findings — e.g. the missing
    /// `runner-steps.sh` environmental issue. Distinct from a real lint failure
    /// where findings exist. Callers (e.g. release preflight) treat this as a
    /// non-blocking warning rather than a hard failure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub harness_error: bool,
    /// True when the extension runner failed before yielding source findings.
    /// This is an internal normalization signal used to render the existing
    /// phase infrastructure status without adding a second public envelope.
    #[serde(skip)]
    pub infrastructure_failure: bool,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub baseline_comparison: Option<lint_baseline::BaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatting_findings: Option<FormattingFindings>,
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    pub summary: Option<LintSummaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_check_capture: Option<SelfCheckCaptureMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedLintRun {
    pub(crate) glob: String,
    pub(crate) step: Option<String>,
    pub(crate) changed_files: Vec<String>,
}

/// The resolved changed-file lint scope, plus the population it was resolved
/// from.
///
/// The population is carried alongside the runs because `runs.is_empty()` on
/// its own is ambiguous, and the workflow used to render an unconditional
/// `passed` for both readings of it (#10685):
///
///   * nothing changed at all — a genuinely empty population, and an honest
///     green; and
///   * files changed but no declared lint route claimed any of them — which is
///     *usually* also honest (a documentation-only diff), and is *sometimes* a
///     route glob that stopped matching.
///
/// `measurement_ok` deliberately does not adjudicate between those two: the
/// route matcher is simultaneously the instrument and the only thing that
/// knows the population, so a broken matcher and an empty population are
/// indistinguishable from inside. What the predicate *does* demand is that the
/// two states stop rendering identically, so `changed_files_considered` is
/// recorded and reported rather than discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedLintPlan {
    pub(crate) runs: Vec<ScopedLintRun>,
    /// Changed files considered before route matching. Zero means the diff
    /// itself was empty.
    pub(crate) changed_files_considered: usize,
}
