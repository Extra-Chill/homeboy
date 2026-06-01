use std::collections::BTreeMap;

use serde::Serialize;

use super::super::BenchArtifactRef;
use crate::core::agent_task::{AgentTaskMatrixAggregate, AgentTaskMatrixPlan};
use crate::core::extension::bench::diagnostic::BenchDiagnostic;
use crate::core::extension::bench::parsing::{BenchMetricPhase, BenchMetricPolicy, BenchResults};
use crate::core::extension::bench::run::BenchRunFailure;
use crate::core::extension::bench::side_by_side::BenchSideBySideReport;
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
    /// Generic agent-task fan-out plan for matrix-shaped rig comparisons.
    /// Present when compared rigs declare `bench.axes` metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_plan: Option<AgentTaskMatrixPlan>,
    /// Generic aggregate envelope keyed by the matrix plan's cell ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_aggregate: Option<AgentTaskMatrixAggregate>,
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
    pub(in crate::core::extension::bench::report) fn from_policies(
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
    pub(in crate::core::extension::bench::report) fn is_phaseless(&self) -> bool {
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
