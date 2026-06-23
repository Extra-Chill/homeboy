//! Shared workflow types — args, results, and summary structures.

use crate::core::engine::baseline::BaselineFlags;
use crate::core::extension::lint::baseline as lint_baseline;
use crate::core::extension::self_check::SelfCheckCaptureMetadata;
use crate::core::extension::ExtensionPhaseTiming;
use crate::core::finding::{FindingProducerSummary, HomeboyFinding};
use crate::core::refactor::AppliedRefactor;
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

/// Compact lint summary for automation consumers.
#[derive(Debug, Clone, Serialize)]
pub struct LintSummaryOutput {
    pub total_findings: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub categories: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_findings: Vec<HomeboyFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FormattingFindings {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub suggested_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedLintRun {
    pub(crate) glob: String,
    pub(crate) step: Option<String>,
    pub(crate) changed_files: Vec<String>,
}
