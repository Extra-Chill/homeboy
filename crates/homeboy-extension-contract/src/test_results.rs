//! Top-level test result aggregate contract types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_finding::HomeboyFinding;

use homeboy_refactor_contract::AppliedRefactor;

use crate::ci_context::CiContext;
use crate::runner_contract::{PhaseFailure, PhaseReport};
use crate::test_analysis::{TestAnalysis, TestAnalysisInput};
use crate::test_parsing::{CoverageOutput, TestSummaryOutput};
use crate::test_result::{TestCounts, TestScopeOutput};
use crate::test_workflow::{
    AutoFixDriftOutput, DriftReport, RawTestOutput, TestBaselineComparison,
};
use crate::ExtensionPhaseTiming;

/// Unified output envelope for all test command modes.
///
/// This is the single serialization target for the test command. Each sub-workflow
/// populates its relevant fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct TestCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<PhaseFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_counts: Option<TestCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TestBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<TestAnalysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<AppliedRefactor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<DriftReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fix_drift: Option<AutoFixDriftOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_scope: Option<TestScopeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<TestSummaryOutput>,
    /// Tail of runner stdout/stderr when tests fail — lets CI wrappers and
    /// users see the actual PHPUnit/cargo output. (#1143)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<RawTestOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_context: Option<CiContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub test_counts: Option<TestCounts>,
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip)]
    pub failure_analysis_input: Option<TestAnalysisInput>,
    pub coverage: Option<CoverageOutput>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<TestSummaryOutput>,
    /// Tail of the runner's stdout/stderr, surfaced when tests fail so users
    /// can see runner output (bootstrap errors, stack traces) without
    /// having to re-run with a different flag. (#1143)
    pub raw_output: Option<RawTestOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriftWorkflowResult {
    pub component: String,
    pub report: DriftReport,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixDriftWorkflowResult {
    pub component: String,
    pub output: AutoFixDriftOutput,
    pub hints: Vec<String>,
    pub report: Option<DriftReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MainTestWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub test_counts: Option<TestCounts>,
    pub coverage: Option<serde_json::Value>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<serde_json::Value>,
}
