//! Top-level lint result aggregate contract type.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_finding::HomeboyFinding;

use homeboy_engine_primitives::baseline::Comparison as BaselineComparison;
use homeboy_finding::FindingProducerSummary;

use crate::ci_context::CiContext;
use crate::lint_result::{LintSummaryOutput, SelfCheckCaptureMetadata};
use crate::runner_contract::{PhaseFailure, PhaseReport};

/// Changes applied by `homeboy review lint --fix`.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedLintFix {
    pub files_modified: usize,
    pub rerun_recommended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
}

/// Aggregated summary of the fixes a lint fixer reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResultsSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub rules: Vec<RuleFixCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primitives: Vec<PrimitiveFixCount>,
}

/// Count of fixes applied for a single rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFixCount {
    pub rule: String,
    pub count: usize,
}

/// Count of fixes applied for a single fix primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimitiveFixCount {
    pub primitive: String,
    pub count: usize,
}

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
    pub autofix: Option<AppliedLintFix>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<BaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_provenance: Option<serde_json::Value>,
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
    pub cargo_target: Option<homeboy_engine_primitives::cargo_target::CargoTargetEvidence>,
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
