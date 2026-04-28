//! Bench command output — unified envelope for the `homeboy bench` command.

use std::collections::BTreeMap;

use serde::Serialize;

use super::artifact::BenchArtifact;
use super::baseline::BenchBaselineComparison;
use super::distribution::BenchRunDistribution;
use super::parsing::{BenchMetricPhase, BenchResults, BenchScenario};
use super::run::{BenchRunFailure, BenchRunWorkflowResult};
use crate::rig::RigStateSnapshot;

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
    let exit_code = result.exit_code;
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
            gate_failures: result.gate_failures,
            baseline_comparison: result.baseline_comparison,
            hints: result.hints,
            rig_state,
            failure: result.failure,
        },
        exit_code,
    )
}

/// Cross-rig comparison envelope.
///
/// Produced by `homeboy bench --rig <a>,<b>[,<c>...]` when more than one
/// rig is requested. Each rig is run in sequence (rig pre-flight + bench)
/// against the same component + workload + iteration count. Per-rig
/// outputs are collected verbatim alongside a `diff` table that expresses
/// each rig's metrics relative to the first rig in the list (the
/// "reference" rig).
///
/// Comparison runs are intentionally **baseline-free**: `--baseline` and
/// `--ratchet` are rejected at the CLI layer because writing one
/// baseline per rig from a comparison invocation would leak which rig is
/// "blessed" — that should be an explicit per-rig single-run
/// (`bench --rig <id> --baseline`).
///
/// The shape mirrors `BenchCommandOutput` enough that consumers reading
/// `passed` / `exit_code` / `component` get sensible values without
/// branching on `comparison`. `passed` is true iff every rig passed.
/// `exit_code` is the first non-zero rig exit code encountered, or `0`.
#[derive(Serialize)]
pub struct BenchComparisonOutput {
    /// Always `"cross_rig"` for this envelope; lets consumers branch on
    /// shape without sniffing field presence.
    pub comparison: &'static str,
    pub passed: bool,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    /// One per `--rig` argument, in input order. Index `0` is the
    /// reference rig that diffs are computed against.
    pub rigs: Vec<RigBenchEntry>,
    /// Per-(scenario, metric) deltas of every non-reference rig vs the
    /// reference rig. Empty when only one rig produced parseable
    /// results.
    pub diff: BenchComparisonDiff,
    /// Per-scenario run summary table. Promotes the variance-aware data
    /// already present under each scenario's `runs_summary` into a direct
    /// cross-rig comparison shape.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<BenchScenarioComparisonSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<BenchComparisonFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BenchComparisonFailure {
    pub rig_id: String,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub exit_code: i32,
    pub stderr_tail: String,
}

#[derive(Serialize)]
pub struct RigBenchEntry {
    pub rig_id: String,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<BenchArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<BenchRunFailure>,
}

/// A compact, grep-friendly pointer to an artifact emitted by a bench
/// scenario. `results` remains the full-fidelity source of truth; this
/// index surfaces the paths that users need immediately after a run.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BenchArtifactRef {
    pub scenario_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_index: Option<usize>,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
        path: artifact.path.clone(),
        kind: artifact.kind.clone(),
        label: artifact.label.clone(),
    }
}

/// Per-scenario, per-metric percent deltas of each non-reference rig vs
/// the reference rig at index 0.
///
/// Outer key: scenario id. Inner key: metric name (e.g. `"p95_ms"`).
/// Innermost: per-rig deltas keyed by rig id, value = `(current -
/// reference) / reference * 100`. The reference rig is omitted from the
/// inner map (its delta would always be zero). A scenario or metric
/// missing from a rig is silently skipped — no synthetic zeros.
///
/// `phase_groups` is the **render-order contract** for phase-aware
/// consumers: when at least one metric policy declares a `phase` tag,
/// this field lists metric names per phase in the canonical render
/// order (`Cold` first, then `Warm`, then `Amortized`, then untagged
/// metrics under `None`-keyed-as-`untagged`). Consumers that want
/// phase-grouped tables iterate `phase_groups` instead of the
/// `by_scenario` inner map (which stays alphabetical for stability).
/// When **no** policy declares a phase, `phase_groups` is `None` and
/// the JSON envelope is byte-identical to pre-phase output.
#[derive(Serialize, Default)]
pub struct BenchComparisonDiff {
    pub by_scenario: BTreeMap<String, BTreeMap<String, BTreeMap<String, MetricDelta>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_groups: Option<BenchPhaseGroups>,
}

/// Render-order contract for phase-aware bench-output consumers.
///
/// Each field lists the metric names whose policy declared the given
/// phase, in the canonical render order: `cold` first (one-time setup
/// costs), `warm` second (steady-state per-iteration costs),
/// `amortized` third (synthetic blends), `untagged` last (metrics
/// whose policy didn't declare a phase, or whose name has no policy
/// at all).
///
/// Empty buckets are omitted from the JSON envelope.
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct BenchPhaseGroups {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cold: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warm: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub amortized: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub untagged: Vec<String>,
}

/// Table-shaped cross-rig summary for one shared scenario.
#[derive(Serialize, Debug, PartialEq)]
pub struct BenchScenarioComparisonSummary {
    pub scenario: String,
    /// Metric used for p50/p95/mean/CV. Timing metrics are preferred so
    /// users see latency variance first, while semantic metrics stay as
    /// row columns.
    pub metric: String,
    pub rows: Vec<BenchScenarioComparisonRow>,
}

/// One row in a scenario's cross-rig summary table.
#[derive(Serialize, Debug, PartialEq)]
pub struct BenchScenarioComparisonRow {
    pub rig_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cv_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_p50_pct: Option<f64>,
    #[serde(flatten)]
    pub semantic_metrics: BTreeMap<String, f64>,
}

impl BenchPhaseGroups {
    /// Build a phase-grouping from a metric-policy table plus the set
    /// of metric names that actually appear in the diff. Metric names
    /// without a policy or without a `phase` tag fall into `untagged`.
    /// Within each phase bucket the metric names are kept in
    /// alphabetical order so the render is stable across runs.
    fn from_policies(
        policies: &BTreeMap<String, super::parsing::BenchMetricPolicy>,
        metric_names: &std::collections::BTreeSet<String>,
    ) -> Self {
        let mut groups = BenchPhaseGroups::default();
        for name in metric_names {
            let phase = policies.get(name).and_then(|p| p.phase);
            match phase {
                Some(BenchMetricPhase::Cold) => groups.cold.push(name.clone()),
                Some(BenchMetricPhase::Warm) => groups.warm.push(name.clone()),
                Some(BenchMetricPhase::Amortized) => groups.amortized.push(name.clone()),
                None => groups.untagged.push(name.clone()),
            }
        }
        groups
    }

    /// True when no policy declared any phase tag — i.e. every metric
    /// name is in the `untagged` bucket. Used to suppress the
    /// `phase_groups` field entirely so back-compat consumers see no
    /// change in the JSON envelope.
    fn is_phaseless(&self) -> bool {
        self.cold.is_empty() && self.warm.is_empty() && self.amortized.is_empty()
    }
}

/// One rig's delta for one metric in one scenario.
#[derive(Serialize, Clone, Copy)]
pub struct MetricDelta {
    pub reference: f64,
    pub current: f64,
    pub delta_percent: f64,
}

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

            for (metric_name, ref_value) in &ref_scenario.metrics.values {
                let mut per_rig: BTreeMap<String, MetricDelta> = BTreeMap::new();
                for (other_id, other_results) in others {
                    let Some(other_scenario) = find_scenario(other_results, &ref_scenario.id)
                    else {
                        continue;
                    };
                    let Some(&current) = other_scenario.metrics.values.get(metric_name) else {
                        continue;
                    };
                    let delta_percent = if *ref_value == 0.0 {
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
                            reference: *ref_value,
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
    let summary = BenchScenarioComparisonSummary::build(&entries);

    let failures: Vec<BenchComparisonFailure> = entries
        .iter()
        .filter(|entry| entry.results.is_none())
        .filter_map(|entry| {
            entry
                .failure
                .as_ref()
                .map(|failure| BenchComparisonFailure {
                    rig_id: entry.rig_id.clone(),
                    component_id: failure.component_id.clone(),
                    component_path: failure.component_path.clone(),
                    scenario_id: failure.scenario_id.clone(),
                    exit_code: failure.exit_code,
                    stderr_tail: failure.stderr_tail.clone(),
                })
        })
        .collect();

    let mut hints = Vec::new();
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
            summary,
            failures,
            hints: Some(hints),
        },
        exit_code,
    )
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
        "Rig failed before producing parseable bench results:\n- rig: {}\n- component: {}{}\n- exit: {}\n- stderr: {}",
        failure.rig_id, component, scenario, failure.exit_code, failure.stderr_tail
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::bench::artifact::BenchArtifact;
    use crate::extension::bench::parsing::{
        BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy, BenchMetrics, BenchRunSnapshot,
        BenchScenario,
    };

    fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
        let mut values = BTreeMap::new();
        for (k, v) in metrics {
            values.insert((*k).to_string(), *v);
        }
        BenchScenario {
            id: id.to_string(),
            file: None,
            source: None,
            default_iterations: None,
            tags: Vec::new(),
            iterations: 10,
            metrics: BenchMetrics {
                values,
                distributions: BTreeMap::new(),
            },
            gates: Vec::new(),
            gate_results: Vec::new(),
            passed: true,
            memory: None,
            artifacts: BTreeMap::new(),
            runs: None,
            runs_summary: None,
        }
    }

    fn scenario_with_runs_summary(
        id: &str,
        metrics: &[(&str, f64)],
        summary_metric: &str,
        distribution: BenchRunDistribution,
    ) -> BenchScenario {
        let mut scenario = scenario(id, metrics);
        let mut runs_summary = BTreeMap::new();
        runs_summary.insert(summary_metric.to_string(), distribution);
        scenario.runs_summary = Some(runs_summary);
        scenario
    }

    fn run_distribution(
        n: u64,
        p50: f64,
        p95: f64,
        mean: f64,
        cv_pct: f64,
    ) -> BenchRunDistribution {
        BenchRunDistribution {
            n,
            min: p50,
            max: p95,
            mean,
            stdev: mean * cv_pct / 100.0,
            cv_pct,
            p50,
            p95,
        }
    }

    fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
        BenchResults {
            component_id: "studio".to_string(),
            iterations: 10,
            run_metadata: None,
            scenarios,
            metric_policies: BTreeMap::new(),
        }
    }

    fn artifact(path: &str, kind: Option<&str>, label: Option<&str>) -> BenchArtifact {
        BenchArtifact {
            path: path.to_string(),
            kind: kind.map(str::to_string),
            label: label.map(str::to_string),
        }
    }

    fn entry(rig_id: &str, passed: bool, results: Option<BenchResults>) -> RigBenchEntry {
        RigBenchEntry {
            rig_id: rig_id.to_string(),
            passed,
            status: if passed { "passed" } else { "failed" }.to_string(),
            exit_code: if passed { 0 } else { 1 },
            artifacts: results.as_ref().map(collect_artifacts).unwrap_or_default(),
            results,
            rig_state: None,
            failure: None,
        }
    }

    fn failed_entry_with_stderr(rig_id: &str) -> RigBenchEntry {
        RigBenchEntry {
            rig_id: rig_id.to_string(),
            passed: false,
            status: "failed".to_string(),
            exit_code: 2,
            artifacts: Vec::new(),
            results: None,
            rig_state: None,
            failure: Some(BenchRunFailure {
                component_id: "studio".to_string(),
                component_path: Some("/Users/chubes/Developer/studio@candidate".to_string()),
                scenario_id: None,
                exit_code: 2,
                stderr_tail: "ERROR: Homeboy bench helper not found at /Users/chubes/.homeboy/runtime/bench-helper.sh".to_string(),
            }),
        }
    }

    #[test]
    fn test_from_main_workflow() {
        let (out, exit) = from_main_workflow(BenchRunWorkflowResult {
            status: "passed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 0,
            iterations: 3,
            results: None,
            gate_failures: Vec::new(),
            baseline_comparison: None,
            hints: None,
            failure: None,
        });

        assert!(out.passed);
        assert_eq!(out.component, "homeboy");
        assert_eq!(out.iterations, 3);
        assert_eq!(exit, 0);
    }

    #[test]
    fn test_from_main_workflow_with_rig() {
        let (out, exit) = from_main_workflow_with_rig(
            BenchRunWorkflowResult {
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 1,
                iterations: 1,
                results: None,
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: Some(vec!["check output".to_string()]),
                failure: None,
            },
            None,
        );

        assert!(!out.passed);
        assert_eq!(out.exit_code, 1);
        assert_eq!(out.hints.as_ref().unwrap()[0], "check output");
        assert_eq!(exit, 1);
    }

    #[test]
    fn test_from_policies() {
        let mut policies = BTreeMap::new();
        policies.insert(
            "boot_ms".to_string(),
            BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: None,
                regression_threshold_absolute: None,
                variance_aware: false,
                min_iterations_for_variance: None,
                regression_test: None,
                phase: Some(BenchMetricPhase::Cold),
            },
        );

        let metric_names = ["boot_ms".to_string(), "p95_ms".to_string()].into();
        let groups = BenchPhaseGroups::from_policies(&policies, &metric_names);

        assert_eq!(groups.cold, vec!["boot_ms".to_string()]);
        assert_eq!(groups.untagged, vec!["p95_ms".to_string()]);
    }

    #[test]
    fn test_is_phaseless() {
        assert!(BenchPhaseGroups {
            cold: Vec::new(),
            warm: Vec::new(),
            amortized: Vec::new(),
            untagged: vec!["p95_ms".to_string()],
        }
        .is_phaseless());

        assert!(!BenchPhaseGroups {
            cold: vec!["boot_ms".to_string()],
            warm: Vec::new(),
            amortized: Vec::new(),
            untagged: Vec::new(),
        }
        .is_phaseless());
    }

    #[test]
    fn test_aggregate_comparison() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![entry("a", true, Some(r.clone())), entry("b", true, Some(r))];
        let (out, exit) = aggregate_comparison("studio".into(), 10, entries);

        assert!(out.passed);
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.iterations, 10);
        assert_eq!(exit, 0);
    }

    #[test]
    fn test_collect_artifacts() {
        let mut scenario = scenario("agent-runtime", &[("p95_ms", 100.0)]);
        scenario.artifacts.insert(
            "summary".to_string(),
            artifact("artifacts/summary.json", Some("json"), Some("Summary")),
        );
        scenario.runs = Some(vec![
            BenchRunSnapshot {
                metrics: scenario.metrics.clone(),
                memory: None,
                artifacts: [(
                    "raw_result".to_string(),
                    artifact("artifacts/run-0/raw.json", Some("json"), Some("Raw result")),
                )]
                .into(),
            },
            BenchRunSnapshot {
                metrics: scenario.metrics.clone(),
                memory: None,
                artifacts: [(
                    "raw_result".to_string(),
                    artifact("artifacts/run-1/raw.json", None, None),
                )]
                .into(),
            },
        ]);

        let indexed = collect_artifacts(&results(vec![scenario]));

        assert_eq!(indexed.len(), 3);
        assert_eq!(indexed[0].scenario_id, "agent-runtime");
        assert_eq!(indexed[0].run_index, None);
        assert_eq!(indexed[0].name, "summary");
        assert_eq!(indexed[0].path, "artifacts/summary.json");
        assert_eq!(indexed[0].kind.as_deref(), Some("json"));
        assert_eq!(indexed[0].label.as_deref(), Some("Summary"));
        assert_eq!(indexed[1].run_index, Some(0));
        assert_eq!(indexed[1].name, "raw_result");
        assert_eq!(indexed[1].path, "artifacts/run-0/raw.json");
        assert_eq!(indexed[2].run_index, Some(1));
        assert_eq!(indexed[2].path, "artifacts/run-1/raw.json");
    }

    #[test]
    fn cross_rig_output_serializes_artifact_index() {
        let mut ref_scenario = scenario("agent-runtime", &[("p95_ms", 100.0)]);
        ref_scenario.runs = Some(vec![BenchRunSnapshot {
            metrics: ref_scenario.metrics.clone(),
            memory: None,
            artifacts: [(
                "raw_result".to_string(),
                artifact("baseline/run-0/raw.json", None, None),
            )]
            .into(),
        }]);
        let mut candidate_scenario = scenario("agent-runtime", &[("p95_ms", 80.0)]);
        candidate_scenario.runs = Some(vec![BenchRunSnapshot {
            metrics: candidate_scenario.metrics.clone(),
            memory: None,
            artifacts: [(
                "raw_result".to_string(),
                artifact("candidate/run-0/raw.json", None, None),
            )]
            .into(),
        }]);

        let entries = vec![
            entry("baseline", true, Some(results(vec![ref_scenario]))),
            entry("candidate", true, Some(results(vec![candidate_scenario]))),
        ];
        let (out, _) = aggregate_comparison("studio".into(), 10, entries);
        let value = serde_json::to_value(out).expect("serialize comparison");

        assert_eq!(
            value["rigs"][0]["artifacts"][0]["path"],
            "baseline/run-0/raw.json"
        );
        assert_eq!(value["rigs"][0]["artifacts"][0]["run_index"], 0);
        assert_eq!(
            value["rigs"][1]["artifacts"][0]["path"],
            "candidate/run-0/raw.json"
        );
    }

    #[test]
    fn diff_computes_percent_delta_lower_is_better() {
        let ref_r = results(vec![scenario("boot", &[("p95_ms", 30000.0)])]);
        let other = results(vec![scenario("boot", &[("p95_ms", 18000.0)])]);
        let diff = BenchComparisonDiff::build(("trunk", &ref_r), &[("combined-fixes", &other)]);
        let d = diff
            .by_scenario
            .get("boot")
            .and_then(|m| m.get("p95_ms"))
            .and_then(|m| m.get("combined-fixes"))
            .unwrap();
        assert_eq!(d.reference, 30000.0);
        assert_eq!(d.current, 18000.0);
        assert!((d.delta_percent - -40.0).abs() < 1e-9);
    }

    #[test]
    fn diff_skips_missing_scenarios_silently() {
        let ref_r = results(vec![
            scenario("a", &[("p95_ms", 100.0)]),
            scenario("b", &[("p95_ms", 200.0)]),
        ]);
        let other = results(vec![scenario("a", &[("p95_ms", 110.0)])]);
        let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
        assert!(diff.by_scenario.contains_key("a"));
        // "b" is in reference but absent from other; reference scenarios
        // are kept only when at least one comparison rig has the metric.
        assert!(!diff.by_scenario.contains_key("b"));
    }

    #[test]
    fn diff_handles_zero_reference_with_signed_infinity() {
        let ref_r = results(vec![scenario("a", &[("errors", 0.0)])]);
        let other_pos = results(vec![scenario("a", &[("errors", 5.0)])]);
        let other_neg = results(vec![scenario("a", &[("errors", -5.0)])]);
        let other_zero = results(vec![scenario("a", &[("errors", 0.0)])]);

        let diff_pos = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_pos)]);
        let pos = diff_pos
            .by_scenario
            .get("a")
            .unwrap()
            .get("errors")
            .unwrap()
            .get("other")
            .unwrap();
        assert!(pos.delta_percent.is_infinite() && pos.delta_percent.is_sign_positive());

        let diff_neg = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_neg)]);
        let neg = diff_neg
            .by_scenario
            .get("a")
            .unwrap()
            .get("errors")
            .unwrap()
            .get("other")
            .unwrap();
        assert!(neg.delta_percent.is_infinite() && neg.delta_percent.is_sign_negative());

        let diff_zero = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other_zero)]);
        let zero = diff_zero
            .by_scenario
            .get("a")
            .unwrap()
            .get("errors")
            .unwrap()
            .get("other")
            .unwrap();
        assert_eq!(zero.delta_percent, 0.0);
    }

    #[test]
    fn aggregate_passed_only_when_all_rigs_pass() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![
            entry("a", true, Some(r.clone())),
            entry("b", false, Some(r.clone())),
        ];
        let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
        assert!(!out.passed);
        assert_eq!(exit, 1);
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn aggregate_exit_zero_when_all_rigs_pass() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![
            entry("a", true, Some(r.clone())),
            entry("b", true, Some(r.clone())),
        ];
        let (out, exit) = aggregate_comparison("studio".into(), 10, entries);
        assert!(out.passed);
        assert_eq!(exit, 0);
        assert_eq!(out.rigs.len(), 2);
    }

    #[test]
    fn aggregate_handles_more_than_two_rigs() {
        let ref_r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let r2 = results(vec![scenario("boot", &[("p95_ms", 80.0)])]);
        let r3 = results(vec![scenario("boot", &[("p95_ms", 120.0)])]);
        let entries = vec![
            entry("a", true, Some(ref_r)),
            entry("b", true, Some(r2)),
            entry("c", true, Some(r3)),
        ];
        let (out, _) = aggregate_comparison("studio".into(), 10, entries);
        let metric = out
            .diff
            .by_scenario
            .get("boot")
            .and_then(|m| m.get("p95_ms"))
            .unwrap();
        assert!(!metric.contains_key("a")); // reference excluded
        assert_eq!(metric.len(), 2);
        assert!((metric.get("b").unwrap().delta_percent - -20.0).abs() < 1e-9);
        assert!((metric.get("c").unwrap().delta_percent - 20.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_emits_hint_when_a_rig_has_no_results() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![entry("a", true, Some(r)), entry("b", false, None)];
        let (out, _) = aggregate_comparison("studio".into(), 10, entries);
        let hints = out.hints.as_ref().unwrap();
        assert!(hints.iter().any(|h| h.contains("no parseable results")));
    }

    #[test]
    fn aggregate_promotes_cross_rig_run_summary() {
        let reference = results(vec![scenario_with_runs_summary(
            "studio-agent-runtime",
            &[("elapsed_ms", 7552.0), ("success_rate", 1.0)],
            "elapsed_ms",
            run_distribution(3, 7552.0, 8324.0, 7827.0, 5.27),
        )]);
        let candidate = results(vec![scenario_with_runs_summary(
            "studio-agent-runtime",
            &[("elapsed_ms", 3311.0), ("success_rate", 1.0)],
            "elapsed_ms",
            run_distribution(3, 3311.0, 3377.0, 3232.0, 5.15),
        )]);

        let entries = vec![
            entry("studio-agent-sdk", true, Some(reference)),
            entry("studio-agent-pi", true, Some(candidate)),
        ];
        let (out, _) = aggregate_comparison("studio".into(), 10, entries);

        assert_eq!(out.summary.len(), 1);
        let summary = &out.summary[0];
        assert_eq!(summary.scenario, "studio-agent-runtime");
        assert_eq!(summary.metric, "elapsed_ms");
        assert_eq!(summary.rows.len(), 2);

        let reference_row = &summary.rows[0];
        assert_eq!(reference_row.rig_id, "studio-agent-sdk");
        assert_eq!(reference_row.n, Some(3));
        assert_eq!(reference_row.p50_ms, Some(7552.0));
        assert_eq!(reference_row.p95_ms, Some(8324.0));
        assert_eq!(reference_row.mean_ms, Some(7827.0));
        assert_eq!(reference_row.cv_pct, Some(5.27));
        assert_eq!(reference_row.delta_p50_pct, Some(0.0));
        assert_eq!(
            reference_row.semantic_metrics.get("success_rate"),
            Some(&1.0)
        );

        let candidate_row = &summary.rows[1];
        assert_eq!(candidate_row.rig_id, "studio-agent-pi");
        assert_eq!(candidate_row.n, Some(3));
        assert_eq!(candidate_row.p50_ms, Some(3311.0));
        assert_eq!(candidate_row.p95_ms, Some(3377.0));
        assert_eq!(candidate_row.mean_ms, Some(3232.0));
        assert_eq!(candidate_row.cv_pct, Some(5.15));
        assert!(
            (candidate_row.delta_p50_pct.unwrap() - -56.157309322033896).abs() < 1e-9,
            "expected p50 delta against reference, got {:?}",
            candidate_row.delta_p50_pct
        );
        assert_eq!(
            candidate_row.semantic_metrics.get("success_rate"),
            Some(&1.0)
        );
    }

    #[test]
    fn comparison_summary_serializes_as_direct_table_shape() {
        let reference = results(vec![scenario_with_runs_summary(
            "chat",
            &[("elapsed_ms", 100.0), ("assistant_message_count", 2.0)],
            "elapsed_ms",
            run_distribution(2, 100.0, 110.0, 105.0, 4.76),
        )]);
        let candidate = results(vec![scenario_with_runs_summary(
            "chat",
            &[("elapsed_ms", 80.0), ("assistant_message_count", 2.0)],
            "elapsed_ms",
            run_distribution(2, 80.0, 90.0, 85.0, 5.88),
        )]);

        let entries = vec![
            entry("ref", true, Some(reference)),
            entry("next", true, Some(candidate)),
        ];
        let (out, _) = aggregate_comparison("agent".into(), 10, entries);
        let value = serde_json::to_value(out).unwrap();
        let rows = value["summary"][0]["rows"].as_array().unwrap();

        assert_eq!(value["summary"][0]["scenario"], "chat");
        assert_eq!(rows[0]["rig_id"], "ref");
        assert_eq!(rows[0]["n"], 2);
        assert_eq!(rows[0]["p50_ms"], 100.0);
        assert_eq!(rows[0]["p95_ms"], 110.0);
        assert_eq!(rows[0]["mean_ms"], 105.0);
        assert_eq!(rows[0]["cv_pct"], 4.76);
        assert_eq!(rows[0]["assistant_message_count"], 2.0);
        assert_eq!(rows[1]["rig_id"], "next");
        assert_eq!(rows[1]["delta_p50_pct"], -20.0);
    }

    #[test]
    fn aggregate_surfaces_no_parseable_failure_metadata() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![
            entry("baseline", true, Some(r)),
            failed_entry_with_stderr("candidate"),
        ];
        let (out, exit) = aggregate_comparison("studio".into(), 10, entries);

        assert_eq!(exit, 2);
        assert_eq!(out.failures.len(), 1);
        let failure = &out.failures[0];
        assert_eq!(failure.rig_id, "candidate");
        assert_eq!(failure.component_id, "studio");
        assert_eq!(failure.exit_code, 2);
        assert!(failure
            .stderr_tail
            .contains("Homeboy bench helper not found"));

        let value = serde_json::to_value(&out).unwrap();
        let json_failure = &value["failures"][0];
        assert_eq!(json_failure["rig_id"], "candidate");
        assert_eq!(json_failure["component_id"], "studio");
        assert!(json_failure["stderr_tail"]
            .as_str()
            .unwrap()
            .contains("bench-helper.sh"));
    }

    #[test]
    fn aggregate_puts_actionable_failure_block_before_generic_hint() {
        let r = results(vec![scenario("boot", &[("p95_ms", 100.0)])]);
        let entries = vec![
            entry("baseline", true, Some(r)),
            failed_entry_with_stderr("candidate"),
        ];
        let (out, _) = aggregate_comparison("studio".into(), 10, entries);
        let hints = out.hints.as_ref().unwrap();

        assert!(hints[0].starts_with("Rig failed before producing parseable bench results:"));
        assert!(hints[0].contains("- rig: candidate"));
        assert!(hints[0].contains("- component: studio (/Users/chubes/Developer/studio@candidate)"));
        assert!(hints[0].contains("- exit: 2"));
        assert!(hints[0].contains("Homeboy bench helper not found"));
        assert!(hints[1].contains("no parseable results"));
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/phase_tag_test.rs"]
mod phase_tag_test;
