//! Top-level lint result aggregate contract type.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_finding::HomeboyFinding;

use homeboy_engine_primitives::baseline::Comparison as BaselineComparison;
use homeboy_finding::FindingProducerSummary;
use homeboy_refactor_contract::AppliedRefactor;

use crate::ci_context::CiContext;
use crate::lint_result::{LintSummaryOutput, SelfCheckCaptureMetadata};
use crate::runner_contract::{PhaseFailure, PhaseReport};

/// Unified output envelope for the lint command.
///
/// This is the single serialization target. The workflow populates relevant
/// fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct LintCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub phase: PhaseReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<PhaseFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<AppliedRefactor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<BaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatting_findings: Option<crate::lint_result::FormattingFindings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<LintSummaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_check_capture: Option<SelfCheckCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_context: Option<CiContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<crate::ExtensionPhaseTiming>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<Value>,
}
