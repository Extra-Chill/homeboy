//! Generic diagnostics emitted by bench workloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::parsing::BenchResults;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchDiagnostic {
    /// Workload-defined diagnostic class used for grouping related failures.
    #[serde(alias = "kind")]
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BenchDiagnosticSource>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BenchDiagnosticSource {
    Run,
    Scenario {
        scenario_id: String,
    },
    ScenarioRun {
        scenario_id: String,
        run_index: usize,
    },
}

pub fn collect_diagnostics(results: Option<&BenchResults>) -> Vec<BenchDiagnostic> {
    let Some(results) = results else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    diagnostics.extend(
        results
            .diagnostics
            .iter()
            .cloned()
            .map(|diagnostic| with_default_source(diagnostic, BenchDiagnosticSource::Run)),
    );

    for scenario in &results.scenarios {
        diagnostics.extend(scenario.diagnostics.iter().cloned().map(|diagnostic| {
            with_default_source(
                diagnostic,
                BenchDiagnosticSource::Scenario {
                    scenario_id: scenario.id.clone(),
                },
            )
        }));

        if let Some(runs) = &scenario.runs {
            for (run_index, run) in runs.iter().enumerate() {
                diagnostics.extend(run.diagnostics.iter().cloned().map(|diagnostic| {
                    with_default_source(
                        diagnostic,
                        BenchDiagnosticSource::ScenarioRun {
                            scenario_id: scenario.id.clone(),
                            run_index,
                        },
                    )
                }));
            }
        }
    }

    diagnostics
}

fn with_default_source(
    mut diagnostic: BenchDiagnostic,
    source: BenchDiagnosticSource,
) -> BenchDiagnostic {
    if diagnostic.source.is_none() {
        diagnostic.source = Some(source);
    }
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::bench::parsing::{BenchMetrics, BenchRunSnapshot, BenchScenario};

    #[test]
    fn test_collect_diagnostics() {
        let run_diagnostic = diagnostic("setup_failed");
        let scenario_diagnostic = diagnostic("fixture_missing");
        let snapshot_diagnostic = diagnostic("assertion_mismatch");

        let results = BenchResults {
            component_id: "demo".to_string(),
            iterations: 1,
            provenance: Default::default(),
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: vec![run_diagnostic],
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            responsiveness: None,
            budget_findings: Vec::new(),
            scenarios: vec![BenchScenario {
                id: "scenario-a".to_string(),
                file: None,
                source: None,
                default_iterations: None,
                tags: Vec::new(),
                iterations: 1,
                metrics: BenchMetrics::default(),
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: false,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: vec![scenario_diagnostic],
                runs: Some(vec![BenchRunSnapshot {
                    metrics: BenchMetrics::default(),
                    metric_groups: BTreeMap::new(),
                    timeline: Vec::new(),
                    span_definitions: Vec::new(),
                    span_results: Vec::new(),
                    memory: None,
                    artifacts: BTreeMap::new(),
                    diagnostics: vec![snapshot_diagnostic],
                }]),
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        };

        let diagnostics = collect_diagnostics(Some(&results));

        assert_eq!(diagnostics.len(), 3);
        assert_eq!(diagnostics[0].class, "setup_failed");
        assert_eq!(diagnostics[0].source, Some(BenchDiagnosticSource::Run));
        assert_eq!(diagnostics[1].class, "fixture_missing");
        assert_eq!(
            diagnostics[1].source,
            Some(BenchDiagnosticSource::Scenario {
                scenario_id: "scenario-a".to_string()
            })
        );
        assert_eq!(diagnostics[2].class, "assertion_mismatch");
        assert_eq!(
            diagnostics[2].source,
            Some(BenchDiagnosticSource::ScenarioRun {
                scenario_id: "scenario-a".to_string(),
                run_index: 0,
            })
        );
    }

    fn diagnostic(class: &str) -> BenchDiagnostic {
        BenchDiagnostic {
            class: class.to_string(),
            message: None,
            source: None,
            metadata: BTreeMap::new(),
        }
    }
}
