//! Bench command output — unified envelope for the `homeboy bench` command.

use serde::Serialize;

use super::artifact::BenchArtifact;
use super::baseline::BenchBaselineComparison;
use super::diagnostic::BenchDiagnostic;
use super::parsing::BenchResults;
use super::run::{BenchRunFailure, BenchRunWorkflowResult};
use crate::core::ci_profile::CiContext;
use crate::core::finding::HomeboyFinding;
use crate::core::rig::RigStateSnapshot;
use crate::core::runner::reportable_artifact_evidence_path;

pub use super::side_by_side::{
    BenchSideBySideArtifact, BenchSideBySideMetric, BenchSideBySidePreviewLink,
    BenchSideBySideReport, BenchSideBySideRigReport,
};

#[derive(Serialize)]
pub struct BenchCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<BenchArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub budget_findings: Vec<HomeboyFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gate_failures: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<BenchBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    /// Rig state captured at the start of the run when bench was invoked
    /// with `--rig <id>`. Skipped when bench ran without a rig so the
    /// existing output shape is unchanged for the bare `homeboy bench`
    /// path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<BenchRunFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_context: Option<CiContext>,
}

pub fn from_main_workflow(result: BenchRunWorkflowResult) -> (BenchCommandOutput, i32) {
    from_main_workflow_with_rig(result, None)
}

/// Same as `from_main_workflow` but also embeds an optional rig-state
/// snapshot — populated by `homeboy bench --rig <id>` so consumers can
/// see exactly which component commits the numbers were measured
/// against.
pub fn from_main_workflow_with_rig(
    result: BenchRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
) -> (BenchCommandOutput, i32) {
    from_main_workflow_with_rig_and_ci_context(result, rig_state, None)
}

pub fn from_main_workflow_with_rig_and_ci_context(
    result: BenchRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
    ci_context: Option<CiContext>,
) -> (BenchCommandOutput, i32) {
    let exit_code = result.exit_code;
    let budget_findings = result
        .results
        .as_ref()
        .map(|results| results.budget_findings.clone())
        .unwrap_or_default();
    (
        BenchCommandOutput {
            passed: exit_code == 0,
            status: result.status,
            component: result.component,
            exit_code,
            iterations: result.iterations,
            artifacts: result
                .results
                .as_ref()
                .map(collect_artifacts)
                .unwrap_or_default(),
            results: result.results,
            budget_findings,
            gate_failures: result.gate_failures,
            baseline_comparison: result.baseline_comparison,
            hints: result.hints,
            rig_state,
            failure: result.failure,
            diagnostics: result.diagnostics,
            ci_context,
        },
        exit_code,
    )
}

mod comparison;

pub(super) use comparison::comparison_metrics;
pub use comparison::{
    aggregate_comparison, aggregate_comparison_with_axes, BenchAxisComparison,
    BenchAxisComparisonSummary, BenchComparisonDiff, BenchComparisonFailure, BenchComparisonOutput,
    BenchComparisonReports, BenchComparisonRigSummary, BenchComparisonSummaryOutput,
    BenchDefaultBaselineExpansion, BenchDiagnosticClassSummary, BenchPhaseGroups,
    BenchScenarioComparisonRow, BenchScenarioComparisonSummary, MetricDelta, RigBenchEntry,
};

/// A compact, grep-friendly pointer to an artifact emitted by a bench
/// scenario. `results` remains the full-fidelity source of truth; this
/// index surfaces the paths that users need immediately after a run.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BenchArtifactRef {
    pub scenario_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_index: Option<usize>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_lifecycle: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_origin_evidence: Option<serde_json::Value>,
}

pub(crate) fn collect_artifacts(results: &BenchResults) -> Vec<BenchArtifactRef> {
    let mut artifacts = Vec::new();
    for scenario in &results.scenarios {
        artifacts.extend(
            scenario
                .artifacts
                .iter()
                .map(|(name, artifact)| artifact_ref(&scenario.id, None, name, artifact)),
        );
        if let Some(runs) = &scenario.runs {
            for (index, run) in runs.iter().enumerate() {
                artifacts.extend(run.artifacts.iter().map(|(name, artifact)| {
                    artifact_ref(&scenario.id, Some(index), name, artifact)
                }));
            }
        }
    }
    artifacts
}

fn artifact_ref(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &BenchArtifact,
) -> BenchArtifactRef {
    BenchArtifactRef {
        scenario_id: scenario_id.to_string(),
        run_index,
        name: name.to_string(),
        path: reportable_artifact_evidence_path(artifact.path.as_ref()),
        url: artifact.url.clone(),
        artifact_type: artifact.artifact_type.clone(),
        kind: artifact.kind.clone(),
        label: artifact.label.clone(),
        observation_artifact_id: artifact.observation_artifact_id.clone(),
        role: artifact.role.clone(),
        preview_url: artifact.preview_url.clone(),
        public_url: artifact.public_url.clone(),
        local_url: artifact.local_url.clone(),
        status: artifact.status.clone(),
        expires_at: artifact.expires_at.clone(),
        cleanup_status: artifact.cleanup_status.clone(),
        service_lifecycle: artifact.service_lifecycle.clone(),
        browser_origin_evidence: artifact.browser_origin_evidence.clone(),
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/phase_tag_test.rs"]
mod phase_tag_test;
