//! Test command output builders — owns the unified test output envelope.
//!
//! All test sub-workflows (main run, drift detection, auto-fix drift, scaffold)
//! produce domain-specific result types. This module provides the unified output
//! envelope and builder functions that assemble results into command-ready output.

use crate::extension::test::{
    CoverageOutput, DriftReport, TestAnalysis, TestBaselineComparison, TestCounts,
    TestScopeOutput, TestSummaryOutput,
};
use crate::refactor::AppliedRefactor;
use serde::Serialize;

use super::scaffold::ScaffoldOutput;
use super::workflow::{AutoFixDriftOutput, AutoFixDriftWorkflowResult, DriftWorkflowResult};
use super::run::TestRunWorkflowResult;

/// Unified output envelope for all test command modes.
///
/// This is the single serialization target for the test command. Each sub-workflow
/// populates its relevant fields; unused fields are `None` and skipped in serialization.
#[derive(Serialize)]
pub struct TestCommandOutput {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_counts: Option<TestCounts>,
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
    pub scaffold: Option<ScaffoldOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fix_drift: Option<AutoFixDriftOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_scope: Option<TestScopeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<TestSummaryOutput>,
}

/// Build output from a main test workflow result.
pub fn from_main_workflow(result: TestRunWorkflowResult) -> (TestCommandOutput, i32) {
    let exit_code = result.exit_code;
    (
        TestCommandOutput {
            status: result.status,
            component: result.component,
            exit_code: result.exit_code,
            test_counts: result.test_counts,
            coverage: result.coverage,
            baseline_comparison: result.baseline_comparison,
            analysis: result.analysis,
            autofix: result.autofix,
            hints: result.hints,
            drift: None,
            scaffold: None,
            auto_fix_drift: None,
            test_scope: result.test_scope,
            summary: result.summary,
        },
        exit_code,
    )
}

/// Build output from a drift detection workflow result.
pub fn from_drift_workflow(result: DriftWorkflowResult) -> (TestCommandOutput, i32) {
    let exit_code = result.exit_code;
    (
        TestCommandOutput {
            status: "drift".to_string(),
            component: result.component,
            exit_code: result.exit_code,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: Some(result.report),
            scaffold: None,
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
        },
        exit_code,
    )
}

/// Build output from an auto-fix drift workflow result.
pub fn from_auto_fix_drift_workflow(result: AutoFixDriftWorkflowResult) -> (TestCommandOutput, i32) {
    let status = if result.output.replacements > 0 || !result.hints.is_empty() {
        if result.output.written { "fixed" } else { "planned" }.to_string()
    } else {
        "passed".to_string()
    };

    (
        TestCommandOutput {
            status,
            component: result.component,
            exit_code: 0,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: Some(result.hints),
            drift: result.report,
            scaffold: None,
            auto_fix_drift: Some(result.output),
            test_scope: None,
            summary: None,
        },
        0,
    )
}

/// Build output from a scaffold workflow result.
pub fn from_scaffold_workflow(
    component: String,
    output: ScaffoldOutput,
) -> (TestCommandOutput, i32) {
    (
        TestCommandOutput {
            status: "scaffold".to_string(),
            component,
            exit_code: 0,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: None,
            scaffold: Some(output),
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
        },
        0,
    )
}
