use serde::Serialize;

use super::parsing::BenchResults;
use super::report::{comparison_metrics, BenchArtifactRef, RigBenchEntry};

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchSideBySideReport {
    pub report: &'static str,
    pub component: String,
    pub iterations: u64,
    pub rigs: Vec<BenchSideBySideRigReport>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchSideBySideRigReport {
    pub rig_id: String,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub key_metrics: Vec<BenchSideBySideMetric>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<BenchSideBySideArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchSideBySideMetric {
    pub scenario_id: String,
    pub name: String,
    pub value: f64,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchSideBySideArtifact {
    pub scenario_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_index: Option<usize>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub(super) fn build_side_by_side_report(
    component: &str,
    iterations: u64,
    entries: &[RigBenchEntry],
) -> BenchSideBySideReport {
    BenchSideBySideReport {
        report: "side_by_side",
        component: component.to_string(),
        iterations,
        rigs: entries.iter().map(side_by_side_rig_report).collect(),
    }
}

fn side_by_side_rig_report(entry: &RigBenchEntry) -> BenchSideBySideRigReport {
    let key_metrics = entry
        .results
        .as_ref()
        .map(side_by_side_key_metrics)
        .unwrap_or_default();

    BenchSideBySideRigReport {
        rig_id: entry.rig_id.clone(),
        passed: entry.passed,
        status: entry.status.clone(),
        exit_code: entry.exit_code,
        elapsed_ms: entry.results.as_ref().and_then(total_elapsed_ms),
        key_metrics,
        artifacts: entry.artifacts.iter().map(side_by_side_artifact).collect(),
        failure_reason: failure_reason(entry),
    }
}

fn side_by_side_key_metrics(results: &BenchResults) -> Vec<BenchSideBySideMetric> {
    let mut metrics = Vec::new();
    for scenario in &results.scenarios {
        for (name, value) in comparison_metrics(scenario) {
            metrics.push(BenchSideBySideMetric {
                scenario_id: scenario.id.clone(),
                name,
                value,
            });
        }
    }
    metrics
}

fn total_elapsed_ms(results: &BenchResults) -> Option<f64> {
    let mut total = 0.0;
    let mut found = false;
    for scenario in &results.scenarios {
        let elapsed = scenario
            .metrics
            .get("elapsed_ms")
            .or_else(|| scenario.metrics.get("duration_ms"));
        if let Some(value) = elapsed {
            total += value;
            found = true;
        }
    }
    found.then_some(total)
}

fn side_by_side_artifact(artifact: &BenchArtifactRef) -> BenchSideBySideArtifact {
    BenchSideBySideArtifact {
        scenario_id: artifact.scenario_id.clone(),
        run_index: artifact.run_index,
        name: artifact.name.clone(),
        path: artifact.path.clone(),
        url: artifact
            .url
            .clone()
            .or_else(|| artifact.path.as_deref().and_then(url_from_artifact_path)),
        kind: artifact.kind.clone(),
        label: artifact.label.clone(),
    }
}

fn url_from_artifact_path(path: &str) -> Option<String> {
    (path.starts_with("http://") || path.starts_with("https://")).then(|| path.to_string())
}

fn failure_reason(entry: &RigBenchEntry) -> Option<String> {
    if let Some(failure) = &entry.failure {
        return Some(failure.stderr_tail.clone());
    }

    entry.results.as_ref().and_then(|results| {
        results.scenarios.iter().find_map(|scenario| {
            scenario
                .gate_results
                .iter()
                .find_map(|result| (!result.passed).then(|| result.reason.clone()).flatten())
                .or_else(|| {
                    (!scenario.passed).then(|| format!("scenario `{}` failed", scenario.id))
                })
        })
    })
}
