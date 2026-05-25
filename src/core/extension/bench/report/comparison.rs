use std::collections::BTreeMap;

use serde::Serialize;

use super::BenchArtifactRef;
use crate::core::extension::bench::diagnostic::BenchDiagnostic;
use crate::core::extension::bench::distribution::BenchRunDistribution;
use crate::core::extension::bench::parsing::{
    BenchMetricPhase, BenchMetricPolicy, BenchResults, BenchScenario,
};
use crate::core::extension::bench::run::BenchRunFailure;
use crate::core::extension::bench::side_by_side::{
    build_side_by_side_report, BenchSideBySideReport,
};
use crate::core::rig::RigStateSnapshot;

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
    /// Supplemental pairwise diffs for rig matrices that declare
    /// `bench.axes`. The primary `diff` remains first-reference vs all
    /// other rigs; these entries compare rigs that differ by exactly one
    /// declared axis while all other axis values match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub axis_diffs: Vec<BenchAxisComparison>,
    /// Per-scenario run summary table. Promotes the variance-aware data
    /// already present under each scenario's `runs_summary` into a direct
    /// cross-rig comparison shape.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<BenchScenarioComparisonSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<BenchComparisonFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostic_classes: Vec<BenchDiagnosticClassSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    pub reports: BenchComparisonReports,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_baseline_expansion: Option<BenchDefaultBaselineExpansion>,
}

#[derive(Serialize)]
pub struct BenchComparisonReports {
    pub side_by_side: BenchSideBySideReport,
}

/// Compact cross-rig comparison envelope for operator-facing summary reads.
///
/// This intentionally omits the heavy per-rig `results`, `artifacts`, and
/// `diff` payloads. The full `BenchComparisonOutput` remains the default
/// machine-readable shape for artifact consumers.
#[derive(Serialize)]
pub struct BenchComparisonSummaryOutput {
    pub comparison: &'static str,
    pub summary_only: bool,
    pub passed: bool,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    pub rigs: Vec<BenchComparisonRigSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<BenchScenarioComparisonSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub axis_diffs: Vec<BenchAxisComparisonSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<BenchComparisonFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostic_classes: Vec<BenchDiagnosticClassSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_baseline_expansion: Option<BenchDefaultBaselineExpansion>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BenchDefaultBaselineExpansion {
    pub baseline_rig: String,
    pub candidate_rig: String,
    pub execution_order: Vec<String>,
    pub opt_out_flag: &'static str,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchComparisonRigSummary {
    pub rig_id: String,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BenchDiagnosticClassSummary {
    pub class: String,
    pub rigs: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BenchComparisonFailure {
    pub rig_id: String,
    #[serde(skip_serializing_if = "is_false")]
    pub implicit_default_baseline: bool,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub exit_code: i32,
    pub stderr_tail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

impl From<BenchComparisonOutput> for BenchComparisonSummaryOutput {
    fn from(output: BenchComparisonOutput) -> Self {
        BenchComparisonSummaryOutput {
            comparison: output.comparison,
            summary_only: true,
            passed: output.passed,
            component: output.component,
            exit_code: output.exit_code,
            iterations: output.iterations,
            rigs: output
                .rigs
                .into_iter()
                .map(|rig| BenchComparisonRigSummary {
                    rig_id: rig.rig_id,
                    passed: rig.passed,
                    status: rig.status,
                    exit_code: rig.exit_code,
                    diagnostics: rig.diagnostics,
                })
                .collect(),
            summary: output.summary,
            axis_diffs: output
                .axis_diffs
                .into_iter()
                .map(BenchAxisComparisonSummary::from)
                .collect(),
            failures: output.failures,
            diagnostic_classes: output.diagnostic_classes,
            hints: output.hints,
            default_baseline_expansion: output.default_baseline_expansion,
        }
    }
}

#[derive(Serialize, Debug, PartialEq)]
pub struct BenchAxisComparisonSummary {
    pub axis: String,
    pub fixed: BTreeMap<String, String>,
    pub reference_rig: String,
    pub reference_value: String,
    pub current_rig: String,
    pub current_value: String,
}

impl From<BenchAxisComparison> for BenchAxisComparisonSummary {
    fn from(comparison: BenchAxisComparison) -> Self {
        BenchAxisComparisonSummary {
            axis: comparison.axis,
            fixed: comparison.fixed,
            reference_rig: comparison.reference_rig,
            reference_value: comparison.reference_value,
            current_rig: comparison.current_rig,
            current_value: comparison.current_value,
        }
    }
}

/// A compact, grep-friendly pointer to an artifact emitted by a bench
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

#[derive(Serialize)]
pub struct BenchAxisComparison {
    pub axis: String,
    pub fixed: BTreeMap<String, String>,
    pub reference_rig: String,
    pub reference_value: String,
    pub current_rig: String,
    pub current_value: String,
    pub diff: BenchComparisonDiff,
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
    pub(super) fn from_policies(
        policies: &BTreeMap<String, BenchMetricPolicy>,
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
    pub(super) fn is_phaseless(&self) -> bool {
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
