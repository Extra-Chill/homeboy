//! Top-level test result aggregate contract types.

use serde::Serialize;
use serde_json::Value;

use homeboy_finding::HomeboyFinding;

use homeboy_refactor_contract::AppliedRefactor;

use crate::ci_context::CiContext;
use crate::runner_contract::{PhaseFailure, PhaseReport};
use crate::test_analysis::{TestAnalysis, TestAnalysisInput};
use crate::test_duration::TestDurations;
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
    /// Positive evidence emitted by an inventory-only test run. This replaces
    /// execution counts only for that explicit mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_inventory: Option<TestInventoryOutput>,
    /// Why an inventory-only run rejected child evidence. This is bounded
    /// diagnostic metadata; it never relaxes the evidence contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_inventory_rejection: Option<TestInventoryRejection>,
    /// Duration facts for this phase. Deliberately separate from `findings`:
    /// those drive failure classification, and a slow test is not a failing
    /// test. `None` when nothing could be measured — never a zeroed block.
    /// (#10655)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_durations: Option<TestDurations>,
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
    pub cargo_target: Option<homeboy_engine_primitives::cargo_target::CargoTargetEvidence>,
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
    /// The unmodified test-runner exit used when artifacts finalize the result.
    #[serde(skip)]
    pub runner_exit_code: Option<i32>,
    pub test_counts: Option<TestCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_inventory: Option<TestInventoryOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_inventory_rejection: Option<TestInventoryRejection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_durations: Option<TestDurations>,
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
    pub cargo_target: Option<homeboy_engine_primitives::cargo_target::CargoTargetEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

/// Validated inventory-only test evidence from a supervised extension child.
#[derive(Debug, Clone, Serialize)]
pub struct TestInventoryOutput {
    pub schema: String,
    pub runner: String,
    pub runner_fingerprint: String,
    pub workspace_fingerprint: String,
    pub test_count: usize,
    pub inventory_fingerprint: String,
    /// Why a changed-scope selection deliberately widened to this inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Stable, non-sensitive reason descriptor-bound inventory evidence was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestInventoryRejection {
    BindingUnavailable,
    RequestedPathRejected,
    PreparationFailed,
    ChildFileMissing,
    ChildFileUnsafe,
    ChildFileOversized,
    ChildFileUnreadable,
    ChildFileCleanupFailed,
    InvalidJson,
    InvalidSchema,
    RunnerFingerprintMismatch,
    WorkspaceFingerprintMismatch,
    InventoryFingerprintMismatch,
    InvalidTests,
    InvalidPayload,
    RevalidationFailed,
    PublicationFailed,
}

impl TestInventoryRejection {
    pub fn message(self) -> &'static str {
        match self {
            Self::BindingUnavailable => "test inventory binding could not be established",
            Self::RequestedPathRejected => {
                "test inventory requested an output path outside the fixed workspace output"
            }
            Self::PreparationFailed => "test inventory temporary evidence could not be prepared",
            Self::ChildFileMissing => {
                "test inventory producer did not write its descriptor-bound evidence file"
            }
            Self::ChildFileUnsafe => "test inventory evidence file was not a regular safe file",
            Self::ChildFileOversized => {
                "test inventory evidence exceeded the maximum permitted size"
            }
            Self::ChildFileUnreadable => "test inventory evidence could not be read completely",
            Self::ChildFileCleanupFailed => "test inventory evidence could not be consumed safely",
            Self::InvalidJson => "test inventory evidence was not valid JSON",
            Self::InvalidSchema => "test inventory evidence did not use the supported schema",
            Self::RunnerFingerprintMismatch => {
                "test inventory runner provenance did not match the bound runner"
            }
            Self::WorkspaceFingerprintMismatch => {
                "test inventory workspace provenance did not match the bound workspace"
            }
            Self::InventoryFingerprintMismatch => {
                "test inventory fingerprint did not match its canonical payload"
            }
            Self::InvalidTests => "test inventory contained invalid or duplicate test entries",
            Self::InvalidPayload => "test inventory payload violated the evidence contract",
            Self::RevalidationFailed => "test inventory binding changed while the producer ran",
            Self::PublicationFailed => "validated test inventory could not be published safely",
        }
    }
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
