use std::collections::BTreeMap;

use serde::Serialize;

use homeboy_core::extension::workload::case::CaseOutcome;
use homeboy_core::extension::workload::compare::WorkloadComparison;
use homeboy_core::extension::workload::list::WorkloadListing;
use homeboy_core::extension::workload::run::WorkloadRunOutcome;
use homeboy_core::extension::workload::stored::StoredWorkloadRun;
use homeboy_core::observation::FindingRecord;
use homeboy_extension_contract::{WorkloadCompareConfig, WorkloadKindConfig, WorkloadResult};

#[derive(Serialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum WorkloadOutput {
    List(WorkloadListOutput),
    Run(WorkloadRunOutput),
    Compare(WorkloadCompareOutput),
    Replay(WorkloadCaseOutput),
    Minimize(WorkloadCaseOutput),
    Inspect(WorkloadInspectOutput),
}

/// The cause of a nonzero exit, lifted into the command envelope's
/// diagnostics so `success: false` always names why.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadFailure {
    pub code: &'static str,
    pub message: String,
}

pub(super) fn run_failure(kind: &str, outcome: &WorkloadRunOutcome) -> Option<WorkloadFailure> {
    if outcome.exit_code == 0 {
        return None;
    }
    if let Some(error) = &outcome.result_error {
        return Some(WorkloadFailure {
            code: "workload.result_invalid",
            message: format!("`{kind}` run {} errored: {error}", outcome.run_id),
        });
    }
    if let Some(comparison) = outcome
        .baseline_comparison
        .as_ref()
        .filter(|comparison| comparison.regressed())
    {
        return Some(WorkloadFailure {
            code: "workload.baseline_regressed",
            message: format!(
                "`{kind}` run {} regressed against its baseline: {}",
                outcome.run_id,
                comparison.regressions.join("; ")
            ),
        });
    }
    let (failed_cases, findings) = outcome
        .result
        .as_ref()
        .map(|result| {
            (
                result
                    .cases
                    .iter()
                    .filter(|case| !case.status.is_pass())
                    .count(),
                result.findings.len(),
            )
        })
        .unwrap_or_default();
    Some(WorkloadFailure {
        code: "workload.failed",
        message: format!(
            "`{kind}` run {} reported {}: {failed_cases} failing case(s), {findings} finding(s)",
            outcome.run_id, outcome.status
        ),
    })
}

/// What a declared kind supports, read from its declaration.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadCapabilities {
    pub run: bool,
    pub replay: bool,
    pub minimize: bool,
    pub campaign: bool,
    pub compare_metrics: Vec<String>,
    pub safety_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<String>,
}

pub(super) fn capabilities(config: &WorkloadKindConfig) -> WorkloadCapabilities {
    WorkloadCapabilities {
        run: config.script.is_some(),
        replay: config.replay_command.is_some(),
        minimize: config.minimize_command.is_some(),
        campaign: config.campaign,
        compare_metrics: config
            .compare
            .metrics
            .iter()
            .map(|metric| metric.name.clone())
            .collect(),
        safety_class: config.safety_class.as_str().to_string(),
        result_schema: config.result_schema.clone(),
    }
}

#[derive(Serialize)]
pub struct WorkloadListOutput {
    pub command: String,
    pub kind: String,
    pub component: String,
    pub extension_id: String,
    pub count: usize,
    pub workloads: Vec<WorkloadListing>,
    pub capabilities: WorkloadCapabilities,
    pub diagnostics: Vec<String>,
}

#[derive(Serialize)]
pub struct WorkloadRunOutput {
    pub command: String,
    pub kind: String,
    pub component: String,
    pub extension_id: String,
    pub safety_class: String,
    #[serde(flatten)]
    pub outcome: WorkloadRunOutcome,
    pub hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<WorkloadFailure>,
}

pub(super) fn run_hints(kind: &str, outcome: &WorkloadRunOutcome) -> Vec<String> {
    let mut hints = vec![format!("homeboy workload inspect {}", outcome.run_id)];
    if let Some(case) = outcome
        .result
        .as_ref()
        .and_then(|result| result.cases.iter().find(|case| !case.status.is_pass()))
    {
        hints.push(format!(
            "homeboy workload replay --kind {kind} --run-id {} --case-id {}",
            outcome.run_id, case.case_id
        ));
    }
    hints.push(format!(
        "homeboy workload compare --kind {kind} --from-run <baseline-run> --to-run {}",
        outcome.run_id
    ));
    hints
}

#[derive(Serialize)]
pub struct WorkloadCompareOutput {
    pub command: String,
    pub kind: String,
    pub from_run: String,
    pub to_run: String,
    /// The candidate run's declared compare semantics the gate applied.
    pub compare: WorkloadCompareConfig,
    pub comparison: WorkloadComparison,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<WorkloadFailure>,
}

#[derive(Serialize)]
pub struct WorkloadCaseOutput {
    pub command: String,
    pub extension_id: String,
    #[serde(flatten)]
    pub outcome: CaseOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<WorkloadFailure>,
}

#[derive(Serialize)]
pub struct WorkloadInspectArtifact {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
}

#[derive(Serialize)]
pub struct WorkloadInspectOutput {
    pub command: String,
    pub run_id: String,
    pub kind: String,
    pub run_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    pub result_status: String,
    pub case_totals: BTreeMap<&'static str, usize>,
    pub cases: Vec<homeboy_extension_contract::WorkloadCase>,
    pub metrics: BTreeMap<String, f64>,
    pub finding_count: usize,
    pub findings: Vec<FindingRecord>,
    pub artifacts: Vec<WorkloadInspectArtifact>,
    pub result_file: String,
    /// The whole envelope, with `--full`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<WorkloadResult>,
}

pub(super) fn inspect_output(
    stored: StoredWorkloadRun,
    findings: Vec<FindingRecord>,
    full: bool,
) -> WorkloadInspectOutput {
    let result = &stored.result;
    WorkloadInspectOutput {
        command: "workload.inspect".to_string(),
        run_id: stored.run.id.clone(),
        kind: stored.run.kind.clone(),
        run_status: stored.run.status.clone(),
        component: stored.run.component_id.clone(),
        workload_id: result.workload_id.clone(),
        result_status: result.status.as_str().to_string(),
        case_totals: result.case_totals(),
        cases: result.cases.clone(),
        metrics: result.metrics.clone(),
        finding_count: findings.len(),
        findings,
        artifacts: stored
            .artifacts
            .iter()
            .map(|artifact| WorkloadInspectArtifact {
                id: artifact.id.clone(),
                kind: artifact.kind.clone(),
                name: artifact
                    .metadata_json
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                path: artifact.path.clone(),
            })
            .collect(),
        result_file: stored.result_file.to_string_lossy().to_string(),
        result: full.then(|| stored.result.clone()),
    }
}
