use std::collections::BTreeMap;

use serde_json::{json, Value};

mod types;

pub use types::{
    BenchAxisComparison, BenchAxisComparisonSummary, BenchComparisonDiff, BenchComparisonFailure,
    BenchComparisonOutput, BenchComparisonReports, BenchComparisonRigSummary,
    BenchComparisonSummaryOutput, BenchDefaultBaselineExpansion, BenchDiagnosticClassSummary,
    BenchPhaseGroups, BenchScenarioComparisonRow, BenchScenarioComparisonSummary, MetricDelta,
    RigBenchEntry,
};

use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskExecutor,
    AgentTaskLimits, AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskMatrixPlan,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
    AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::extension::bench::diagnostic::BenchDiagnostic;
use crate::core::extension::bench::distribution::BenchRunDistribution;
use crate::core::extension::bench::parsing::{BenchResults, BenchScenario};
use crate::core::extension::bench::side_by_side::build_side_by_side_report;

impl BenchComparisonDiff {
    /// Build the diff table from a reference rig's results plus zero or
    /// more comparison rigs' results.
    ///
    /// The "(rig_id, results)" pairs are taken in their original order
    /// so the JSON output's per-rig key insertion order matches the CLI
    /// invocation order. `reference` is the first rig.
    ///
    /// Missing scenarios or metrics are skipped, not zeroed: this is a
    /// comparison surface, not a baseline ratchet, so absent data should
    /// surface as absence rather than a misleading 0% delta.
    pub fn build(
        reference: (&str, &BenchResults),
        others: &[(&str, &BenchResults)],
    ) -> BenchComparisonDiff {
        let (_ref_id, ref_results) = reference;
        let mut by_scenario: BTreeMap<String, BTreeMap<String, BTreeMap<String, MetricDelta>>> =
            BTreeMap::new();

        for ref_scenario in &ref_results.scenarios {
            let mut metric_table: BTreeMap<String, BTreeMap<String, MetricDelta>> = BTreeMap::new();

            for (metric_name, ref_value) in comparison_metrics(ref_scenario) {
                let mut per_rig: BTreeMap<String, MetricDelta> = BTreeMap::new();
                for (other_id, other_results) in others {
                    let Some(other_scenario) = find_scenario(other_results, &ref_scenario.id)
                    else {
                        continue;
                    };
                    let Some(current) = comparison_metrics(other_scenario)
                        .get(&metric_name)
                        .copied()
                    else {
                        continue;
                    };
                    let delta_percent = if ref_value == 0.0 {
                        // Avoid divide-by-zero. Treat 0→nonzero as
                        // unbounded (None would be more honest, but the
                        // contract is f64; emit a deterministic +∞ /
                        // -∞ via signum so consumers can detect it).
                        if current == 0.0 {
                            0.0
                        } else if current > 0.0 {
                            f64::INFINITY
                        } else {
                            f64::NEG_INFINITY
                        }
                    } else {
                        (current - ref_value) / ref_value * 100.0
                    };
                    per_rig.insert(
                        (*other_id).to_string(),
                        MetricDelta {
                            reference: ref_value,
                            current,
                            delta_percent,
                        },
                    );
                }
                if !per_rig.is_empty() {
                    metric_table.insert(metric_name.clone(), per_rig);
                }
            }

            if !metric_table.is_empty() {
                by_scenario.insert(ref_scenario.id.clone(), metric_table);
            }
        }

        // Derive phase grouping from the reference rig's metric
        // policies. Phase tagging is opt-in: when no policy declares a
        // phase, `phase_groups` stays `None` and the JSON envelope is
        // byte-identical to pre-phase output. When at least one policy
        // declares a phase, emit the full grouping (including an
        // `untagged` bucket for metrics without a phase tag) so
        // consumers have a complete render-order contract.
        let metric_names: std::collections::BTreeSet<String> = by_scenario
            .values()
            .flat_map(|m| m.keys().cloned())
            .collect();
        let phase_groups = if metric_names.is_empty() {
            None
        } else {
            let groups =
                BenchPhaseGroups::from_policies(&ref_results.metric_policies, &metric_names);
            if groups.is_phaseless() {
                None
            } else {
                Some(groups)
            }
        };

        BenchComparisonDiff {
            by_scenario,
            phase_groups,
        }
    }
}

impl BenchScenarioComparisonSummary {
    fn build(entries: &[RigBenchEntry]) -> Vec<BenchScenarioComparisonSummary> {
        let Some(reference_results) = entries.first().and_then(|e| e.results.as_ref()) else {
            return Vec::new();
        };

        let parseable_entries: Vec<&RigBenchEntry> = entries
            .iter()
            .filter(|entry| entry.results.is_some())
            .collect();
        if parseable_entries.len() < 2 {
            return Vec::new();
        }

        let mut summaries = Vec::new();
        for ref_scenario in &reference_results.scenarios {
            let scenario_rows: Vec<(&RigBenchEntry, &BenchScenario)> = parseable_entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .results
                        .as_ref()
                        .and_then(|results| find_scenario(results, &ref_scenario.id))
                        .map(|scenario| (*entry, scenario))
                })
                .collect();

            if scenario_rows.len() != parseable_entries.len() {
                continue;
            }

            let Some(metric) = select_summary_metric(
                scenario_rows
                    .iter()
                    .map(|(_, scenario)| *scenario)
                    .collect::<Vec<_>>()
                    .as_slice(),
            ) else {
                continue;
            };

            let reference_p50 = scenario_rows
                .first()
                .and_then(|(_, scenario)| summary_distribution(scenario, &metric))
                .map(|distribution| distribution.p50);

            let rows = scenario_rows
                .into_iter()
                .map(|(entry, scenario)| {
                    let distribution = summary_distribution(scenario, &metric);
                    let p50 = distribution.map(|d| d.p50);
                    let delta_p50_pct = match (reference_p50, p50) {
                        (Some(reference), Some(current)) => Some(percent_delta(reference, current)),
                        _ => None,
                    };

                    BenchScenarioComparisonRow {
                        rig_id: entry.rig_id.clone(),
                        n: distribution.map(|d| d.n),
                        p50_ms: p50,
                        p95_ms: distribution.map(|d| d.p95),
                        mean_ms: distribution.map(|d| d.mean),
                        cv_pct: distribution.map(|d| d.cv_pct),
                        delta_p50_pct,
                        semantic_metrics: semantic_metrics(scenario, &metric),
                    }
                })
                .collect();

            summaries.push(BenchScenarioComparisonSummary {
                scenario: ref_scenario.id.clone(),
                metric,
                rows,
            });
        }

        summaries
    }
}

fn select_summary_metric(scenarios: &[&BenchScenario]) -> Option<String> {
    let reference = scenarios.first()?;
    let summary = reference.runs_summary.as_ref()?;
    let candidates = ["elapsed_ms", "duration_ms", "p50_ms", "p95_ms", "mean_ms"];

    for candidate in candidates {
        if summary.contains_key(candidate)
            && scenarios
                .iter()
                .all(|scenario| summary_distribution(scenario, candidate).is_some())
        {
            return Some(candidate.to_string());
        }
    }

    summary.keys().find_map(|metric| {
        if metric.ends_with("_ms")
            && scenarios
                .iter()
                .all(|scenario| summary_distribution(scenario, metric).is_some())
        {
            Some(metric.clone())
        } else {
            None
        }
    })
}

fn summary_distribution<'a>(
    scenario: &'a BenchScenario,
    metric: &str,
) -> Option<&'a BenchRunDistribution> {
    scenario
        .runs_summary
        .as_ref()
        .and_then(|summary| summary.get(metric))
}

fn percent_delta(reference: f64, current: f64) -> f64 {
    if reference == 0.0 {
        if current == 0.0 {
            0.0
        } else if current > 0.0 {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }
    } else {
        (current - reference) / reference * 100.0
    }
}

fn semantic_metrics(scenario: &BenchScenario, primary_metric: &str) -> BTreeMap<String, f64> {
    scenario
        .metrics
        .values
        .iter()
        .filter_map(|(name, value)| {
            if name == primary_metric || name.ends_with("_ms") || name.ends_with("_pct") {
                return None;
            }
            Some((name.clone(), *value))
        })
        .collect()
}

fn find_scenario<'a>(results: &'a BenchResults, id: &str) -> Option<&'a BenchScenario> {
    results.scenarios.iter().find(|s| s.id == id)
}

pub(in crate::core::extension::bench) fn comparison_metrics(
    scenario: &BenchScenario,
) -> BTreeMap<String, f64> {
    let mut metrics = scenario.metrics.values.clone();
    for (group, values) in &scenario.metric_groups {
        for (name, value) in values {
            metrics.insert(format!("{}.{}", group, name), *value);
        }
    }
    metrics
}

/// Aggregate N per-rig single-run results into a comparison envelope.
///
/// Caller is responsible for the order: `entries[0]` is treated as the
/// reference for diff math. The aggregate `passed` flag is true iff all
/// rigs passed; `exit_code` is the first non-zero rig exit code, or 0.
pub fn aggregate_comparison(
    component: String,
    iterations: u64,
    entries: Vec<RigBenchEntry>,
) -> (BenchComparisonOutput, i32) {
    aggregate_comparison_with_axes(component, iterations, entries, &BTreeMap::new())
}

pub fn aggregate_comparison_with_axes(
    component: String,
    iterations: u64,
    entries: Vec<RigBenchEntry>,
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> (BenchComparisonOutput, i32) {
    let passed = entries.iter().all(|e| e.passed);
    let exit_code = entries
        .iter()
        .find(|e| !e.passed)
        .map(|e| e.exit_code)
        .unwrap_or(0);

    let diff = match entries.first().and_then(|e| e.results.as_ref()) {
        None => BenchComparisonDiff::default(),
        Some(ref_results) => {
            let reference_id = entries[0].rig_id.as_str();
            let others: Vec<(&str, &BenchResults)> = entries
                .iter()
                .skip(1)
                .filter_map(|e| e.results.as_ref().map(|r| (e.rig_id.as_str(), r)))
                .collect();
            BenchComparisonDiff::build((reference_id, ref_results), &others)
        }
    };
    let axis_diffs = build_axis_diffs(&entries, axes_by_rig);
    let agent_task_matrix =
        build_agent_task_matrix_summary(&component, iterations, &entries, axes_by_rig);
    let summary = BenchScenarioComparisonSummary::build(&entries);
    let side_by_side = build_side_by_side_report(&component, iterations, &entries);

    let failures: Vec<BenchComparisonFailure> = entries
        .iter()
        .filter(|entry| entry.results.is_none())
        .filter_map(|entry| {
            entry
                .failure
                .as_ref()
                .map(|failure| BenchComparisonFailure {
                    rig_id: entry.rig_id.clone(),
                    implicit_default_baseline: false,
                    component_id: failure.component_id.clone(),
                    component_path: failure.component_path.clone(),
                    scenario_id: failure.scenario_id.clone(),
                    exit_code: failure.exit_code,
                    stderr_tail: failure.stderr_tail.clone(),
                    diagnostics: failure.diagnostics.clone(),
                })
        })
        .collect();
    let diagnostic_classes = summarize_diagnostic_classes(&entries);

    let mut hints = Vec::new();
    for summary in &diagnostic_classes {
        if summary.rigs.len() > 1 {
            hints.push(format!(
                "Diagnostic `{}` occurred in multiple rigs: {}",
                summary.class,
                summary.rigs.join(", ")
            ));
        }
    }
    for failure in &failures {
        hints.push(format_failure_hint(failure));
    }
    if entries.iter().any(|e| e.results.is_none()) {
        hints.push(
            "One or more rigs produced no parseable results; their columns are absent from `diff`."
                .to_string(),
        );
    }
    hints.push(
        "Cross-rig runs are comparison-only. Use `homeboy bench --rig <id> --baseline` to ratchet a single rig.".to_string(),
    );
    hints.push("Full options: homeboy docs commands/bench".to_string());

    (
        BenchComparisonOutput {
            comparison: "cross_rig",
            passed,
            component,
            exit_code,
            iterations,
            rigs: entries,
            diff,
            axis_diffs,
            agent_task_plan: agent_task_matrix.as_ref().map(|(plan, _)| plan.clone()),
            agent_task_aggregate: agent_task_matrix.map(|(_, aggregate)| aggregate),
            summary,
            failures,
            diagnostic_classes,
            hints: Some(hints),
            reports: BenchComparisonReports { side_by_side },
            default_baseline_expansion: None,
        },
        exit_code,
    )
}

fn build_agent_task_matrix_summary(
    component: &str,
    iterations: u64,
    entries: &[RigBenchEntry],
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<(AgentTaskMatrixPlan, AgentTaskMatrixAggregate)> {
    if axes_by_rig.is_empty() {
        return None;
    }

    let axes = agent_task_axes(axes_by_rig);
    if axes.is_empty() {
        return None;
    }

    let plan = expand_agent_task_matrix(
        format!("bench/{component}"),
        axes,
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "template".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "homeboy.bench".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: vec!["bench".to_string()],
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Run the selected bench matrix cell.".to_string(),
            inputs: json!({
                "component": component,
                "iterations": iterations,
            }),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["bench-results".to_string()],
            artifact_declarations: Vec::new(),
            metadata: json!({ "source": "bench.comparison" }),
        },
    )
    .ok()?;

    let outcomes = entries
        .iter()
        .filter_map(|entry| agent_task_outcome_for_entry(entry, axes_by_rig, &plan))
        .collect::<Vec<_>>();
    let aggregate = AgentTaskMatrixAggregate::from_outcomes(&plan, &outcomes);

    Some((plan, aggregate))
}

fn agent_task_axes(
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Vec<AgentTaskMatrixAxis> {
    let mut values_by_axis: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for axes in axes_by_rig.values() {
        for (axis, value) in axes {
            let values = values_by_axis.entry(axis.clone()).or_default();
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
    }

    values_by_axis
        .into_iter()
        .map(|(name, values)| AgentTaskMatrixAxis { name, values })
        .collect()
}

fn agent_task_outcome_for_entry(
    entry: &RigBenchEntry,
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
    plan: &AgentTaskMatrixPlan,
) -> Option<AgentTaskOutcome> {
    let axes = axes_by_rig.get(&entry.rig_id)?;
    let task_id = plan
        .cells
        .iter()
        .find(|cell| &cell.axes == axes)
        .map(|cell| cell.task.task_id.clone())?;

    Some(AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: if entry.passed {
            AgentTaskOutcomeStatus::Succeeded
        } else {
            AgentTaskOutcomeStatus::Failed
        },
        summary: Some(entry.status.clone()),
        failure_classification: None,
        outputs: serde_json::Value::Null,
        artifacts: entry
            .artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| AgentTaskArtifact {
                id: format!("{}:{}", entry.rig_id, index),
                kind: artifact
                    .kind
                    .clone()
                    .or_else(|| artifact.artifact_type.clone())
                    .unwrap_or_else(|| "bench_artifact".to_string()),
                name: Some(artifact.name.clone()),
                path: artifact.path.clone(),
                url: artifact.url.clone(),
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: json!({
                    "scenario_id": artifact.scenario_id,
                    "run_index": artifact.run_index,
                    "label": artifact.label,
                }),
                schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            })
            .collect(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: entry
            .diagnostics
            .iter()
            .map(agent_task_diagnostic)
            .collect(),
        workflow: None,
        follow_up: None,
        metadata: json!({
            "rig_id": entry.rig_id,
            "status": entry.status,
            "exit_code": entry.exit_code,
        }),
    })
}

fn agent_task_diagnostic(diagnostic: &BenchDiagnostic) -> AgentTaskDiagnostic {
    AgentTaskDiagnostic {
        class: diagnostic.class.clone(),
        message: diagnostic.message.clone().unwrap_or_default(),
        data: json!({
            "source": diagnostic.source,
            "metadata": diagnostic.metadata,
        }),
    }
}

fn summarize_diagnostic_classes(entries: &[RigBenchEntry]) -> Vec<BenchDiagnosticClassSummary> {
    let mut by_class: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        for diagnostic in &entry.diagnostics {
            let rigs = by_class.entry(diagnostic.class.clone()).or_default();
            if !rigs.contains(&entry.rig_id) {
                rigs.push(entry.rig_id.clone());
            }
        }
    }

    by_class
        .into_iter()
        .map(|(class, rigs)| BenchDiagnosticClassSummary { class, rigs })
        .collect()
}

fn build_axis_diffs(
    entries: &[RigBenchEntry],
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Vec<BenchAxisComparison> {
    if axes_by_rig.len() < 2 {
        return Vec::new();
    }

    let mut axes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for values in axes_by_rig.values() {
        axes.extend(values.keys().cloned());
    }

    let mut comparisons = Vec::new();
    for axis in axes {
        let mut groups: BTreeMap<Vec<(String, String)>, Vec<&RigBenchEntry>> = BTreeMap::new();
        for entry in entries.iter().filter(|entry| entry.results.is_some()) {
            let Some(values) = axes_by_rig.get(&entry.rig_id) else {
                continue;
            };
            if !values.contains_key(&axis) {
                continue;
            }
            let fixed: Vec<(String, String)> = values
                .iter()
                .filter(|(key, _)| *key != &axis)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            groups.entry(fixed).or_default().push(entry);
        }

        for (fixed_pairs, group_entries) in groups {
            let mut by_axis_value: BTreeMap<&str, &RigBenchEntry> = BTreeMap::new();
            let mut ordered_values = Vec::new();
            for entry in group_entries {
                let value = axes_by_rig
                    .get(&entry.rig_id)
                    .and_then(|values| values.get(&axis))
                    .map(String::as_str)
                    .expect("axis value was checked above");
                if !by_axis_value.contains_key(value) {
                    ordered_values.push(value);
                }
                by_axis_value.insert(value, entry);
            }

            if ordered_values.len() != 2 || by_axis_value.len() != 2 {
                continue;
            }

            let reference_value = ordered_values[0];
            let current_value = ordered_values[1];
            let reference = by_axis_value[reference_value];
            let current = by_axis_value[current_value];
            let (Some(reference_results), Some(current_results)) =
                (reference.results.as_ref(), current.results.as_ref())
            else {
                continue;
            };

            comparisons.push(BenchAxisComparison {
                axis: axis.clone(),
                fixed: fixed_pairs.into_iter().collect(),
                reference_rig: reference.rig_id.clone(),
                reference_value: reference_value.to_string(),
                current_rig: current.rig_id.clone(),
                current_value: current_value.to_string(),
                diff: BenchComparisonDiff::build(
                    (&reference.rig_id, reference_results),
                    &[(&current.rig_id, current_results)],
                ),
            });
        }
    }

    comparisons
}

fn format_failure_hint(failure: &BenchComparisonFailure) -> String {
    let component = match &failure.component_path {
        Some(path) => format!("{} ({})", failure.component_id, path),
        None => failure.component_id.clone(),
    };
    let scenario = failure
        .scenario_id
        .as_deref()
        .map(|id| format!("\n- scenario: {}", id))
        .unwrap_or_default();

    format!(
        "Rig failed before producing parseable bench results:\n- rig: {}\n- component: {}{}\n- exit: {}{}\n- stderr: {}",
        failure.rig_id,
        component,
        scenario,
        failure.exit_code,
        format_diagnostic_hint_suffix(&failure.diagnostics),
        failure.stderr_tail
    )
}

fn format_diagnostic_hint_suffix(diagnostics: &[BenchDiagnostic]) -> String {
    diagnostics
        .first()
        .map(|diagnostic| format!("\n- diagnostic: {}", diagnostic.class))
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg(test)]
#[path = "../../../../../tests/core/extension/bench/report_comparison_test.rs"]
mod tests;
