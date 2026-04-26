//! Bench command output — unified envelope for the `homeboy bench` command.

use std::collections::BTreeMap;

use serde::Serialize;

use super::baseline::BenchBaselineComparison;
use super::parsing::{BenchMetricPhase, BenchResults, BenchScenario};
use super::run::BenchRunWorkflowResult;
use crate::rig::RigStateSnapshot;

#[derive(Serialize)]
pub struct BenchCommandOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub iterations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<BenchResults>,
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
            results: result.results,
            baseline_comparison: result.baseline_comparison,
            hints: result.hints,
            rig_state,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct RigBenchEntry {
    pub rig_id: String,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<BenchResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
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

impl BenchPhaseGroups {
    /// Build a phase-grouping from a metric-policy table plus the set
    /// of metric names that actually appear in the diff. Metric names
    /// without a policy or without a `phase` tag fall into `untagged`.
    /// Within each phase bucket the metric names are kept in
    /// alphabetical order so the render is stable across runs.
    pub fn from_policies(
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
    pub fn is_phaseless(&self) -> bool {
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

    let mut hints = Vec::new();
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
            hints: Some(hints),
        },
        exit_code,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::bench::parsing::{BenchMetrics, BenchScenario};

    fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
        let mut values = BTreeMap::new();
        for (k, v) in metrics {
            values.insert((*k).to_string(), *v);
        }
        BenchScenario {
            id: id.to_string(),
            file: None,
            iterations: 10,
            metrics: BenchMetrics { values },
            memory: None,
        }
    }

    fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
        BenchResults {
            component_id: "studio".to_string(),
            iterations: 10,
            scenarios,
            metric_policies: BTreeMap::new(),
        }
    }

    fn entry(rig_id: &str, passed: bool, results: Option<BenchResults>) -> RigBenchEntry {
        RigBenchEntry {
            rig_id: rig_id.to_string(),
            passed,
            status: if passed { "passed" } else { "failed" }.to_string(),
            exit_code: if passed { 0 } else { 1 },
            results,
            rig_state: None,
        }
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
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/phase_tag_test.rs"]
mod phase_tag_test;
