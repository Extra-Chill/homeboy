//! Bench baseline — ratchet for scenario latency (p95) regressions.
//!
//! Stored under `homeboy.json` → `baselines.bench` via the generic
//! `engine::baseline` primitive, alongside `baselines.test` and
//! `baselines.audit`. Each scenario appears as a `Fingerprintable` item
//! with fingerprint = `scenario_id`, so adding or removing scenarios
//! tracks through the generic `new_items` / `resolved_fingerprints`
//! lanes automatically.
//!
//! On top of that, bench adds a **threshold-based regression check**:
//! a scenario regresses when its current `p95_ms` exceeds its baseline
//! `p95_ms` by more than the configured percentage. Improvements (p95
//! got faster) are celebrated and only written back to the baseline
//! when `--ratchet` is set.
//!
//! Default threshold is 5%. p95 was chosen as the signal over mean
//! because p95 is less sensitive than mean to one-off GC pauses but
//! more sensitive than p99 to genuine regressions. Callers can pass
//! any threshold per run via the command flag.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::engine::baseline::{self as generic, BaselineConfig};
use crate::error::Result;

use super::parsing::{BenchResults, BenchScenario};

const BASELINE_KEY: &str = "bench";

/// Resolve the baseline storage key for a given rig context.
///
/// - `None` → `"bench"` (the historic, unpinned baseline). Same shape and
///   path as before this change so existing baselines load unchanged.
/// - `Some("studio-playground-dev")` → `"bench.rig.studio-playground-dev"`
///   so rig-pinned runs don't collide with bare ones, and different rigs
///   keep their own histories side by side under the same `homeboy.json`.
fn baseline_key_for(rig_id: Option<&str>) -> String {
    match rig_id {
        None => BASELINE_KEY.to_string(),
        Some(id) => format!("{}.rig.{}", BASELINE_KEY, id),
    }
}

/// Default regression threshold: 5% p95_ms slowdown flags a regression.
pub const DEFAULT_REGRESSION_THRESHOLD_PERCENT: f64 = 5.0;

/// Per-scenario snapshot persisted in the baseline metadata.
///
/// Only the metrics that participate in comparisons + the ones useful
/// for human diffs are stored. The runner can emit more per-scenario
/// data in each run (that's in `BenchResults`); the baseline stores a
/// canonical compact form.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchScenarioSnapshot {
    pub id: String,
    pub p95_ms: f64,
    pub p50_ms: f64,
    pub mean_ms: f64,
}

impl BenchScenarioSnapshot {
    pub(crate) fn from_scenario(scenario: &BenchScenario) -> Self {
        Self {
            id: scenario.id.clone(),
            p95_ms: scenario.metrics.p95_ms,
            p50_ms: scenario.metrics.p50_ms,
            mean_ms: scenario.metrics.mean_ms,
        }
    }
}

impl generic::Fingerprintable for BenchScenarioSnapshot {
    fn fingerprint(&self) -> String {
        self.id.clone()
    }
    fn description(&self) -> String {
        format!(
            "p95 {:.2}ms (p50 {:.2}ms, mean {:.2}ms)",
            self.p95_ms, self.p50_ms, self.mean_ms
        )
    }
    fn context_label(&self) -> String {
        self.id.clone()
    }
}

/// Metadata stored alongside the fingerprint list in the baseline file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchBaselineMetadata {
    pub scenarios: Vec<BenchScenarioSnapshot>,
    /// Total iterations used when the baseline was captured. Stored for
    /// human context; comparisons don't require matching iteration
    /// counts.
    pub iterations: u64,
}

pub type BenchBaseline = generic::Baseline<BenchBaselineMetadata>;

/// Per-scenario delta vs baseline.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScenarioDelta {
    pub id: String,
    pub baseline_p95_ms: f64,
    pub current_p95_ms: f64,
    /// Current minus baseline in ms. Negative = faster.
    pub p95_delta_ms: f64,
    /// (current - baseline) / baseline * 100. Negative = faster.
    pub p95_delta_pct: f64,
    pub regression: bool,
    pub improvement: bool,
}

/// Summary of comparing a current run against a stored baseline.
#[derive(Debug, Clone, Serialize)]
pub struct BenchBaselineComparison {
    pub threshold_percent: f64,
    pub scenarios: Vec<ScenarioDelta>,
    /// Scenarios present in the current run but not the baseline.
    pub new_scenario_ids: Vec<String>,
    /// Scenarios present in the baseline but not the current run.
    pub removed_scenario_ids: Vec<String>,
    pub regression: bool,
    pub has_improvements: bool,
    /// Short human-readable reasons, one per regressed scenario.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    results: &BenchResults,
    rig_id: Option<&str>,
) -> Result<std::path::PathBuf> {
    let snapshots: Vec<BenchScenarioSnapshot> = results
        .scenarios
        .iter()
        .map(BenchScenarioSnapshot::from_scenario)
        .collect();
    let metadata = BenchBaselineMetadata {
        scenarios: snapshots.clone(),
        iterations: results.iterations,
    };
    let key = baseline_key_for(rig_id);
    let config = BaselineConfig::new(source_path, key);
    generic::save(&config, component_id, &snapshots, metadata)
}

pub fn load_baseline(source_path: &Path, rig_id: Option<&str>) -> Option<BenchBaseline> {
    let key = baseline_key_for(rig_id);
    let config = BaselineConfig::new(source_path, key);
    generic::load::<BenchBaselineMetadata>(&config)
        .ok()
        .flatten()
}

/// Compare a current run against a loaded baseline at the given p95
/// regression threshold (as a percentage, e.g. `5.0` = 5%).
pub fn compare(
    current: &BenchResults,
    baseline: &BenchBaseline,
    threshold_percent: f64,
) -> BenchBaselineComparison {
    let baseline_by_id: HashMap<&str, &BenchScenarioSnapshot> = baseline
        .metadata
        .scenarios
        .iter()
        .map(|snap| (snap.id.as_str(), snap))
        .collect();

    let current_ids: std::collections::HashSet<&str> =
        current.scenarios.iter().map(|s| s.id.as_str()).collect();

    let mut scenario_deltas = Vec::new();
    let mut new_scenario_ids = Vec::new();
    let mut reasons = Vec::new();
    let mut has_improvements = false;
    let mut any_regression = false;

    for scenario in &current.scenarios {
        let Some(prior) = baseline_by_id.get(scenario.id.as_str()) else {
            new_scenario_ids.push(scenario.id.clone());
            continue;
        };

        let baseline_p95 = prior.p95_ms;
        let current_p95 = scenario.metrics.p95_ms;
        let delta_ms = current_p95 - baseline_p95;

        // Guard against zero-valued baselines — anything-over-zero is
        // infinite % regression, which is noise for a micro-benchmark
        // that legitimately measured 0ms. Treat them as no-delta.
        let delta_pct = if baseline_p95 > 0.0 {
            (delta_ms / baseline_p95) * 100.0
        } else {
            0.0
        };

        let threshold_ratio = 1.0 + (threshold_percent / 100.0);
        let regression = baseline_p95 > 0.0 && current_p95 > baseline_p95 * threshold_ratio;
        let improvement = delta_ms < 0.0;

        if regression {
            any_regression = true;
            reasons.push(format!(
                "{}: p95 {:.2}ms → {:.2}ms ({:+.1}%)",
                scenario.id, baseline_p95, current_p95, delta_pct
            ));
        }
        if improvement {
            has_improvements = true;
        }

        scenario_deltas.push(ScenarioDelta {
            id: scenario.id.clone(),
            baseline_p95_ms: baseline_p95,
            current_p95_ms: current_p95,
            p95_delta_ms: delta_ms,
            p95_delta_pct: delta_pct,
            regression,
            improvement,
        });
    }

    let removed_scenario_ids: Vec<String> = baseline
        .metadata
        .scenarios
        .iter()
        .filter(|s| !current_ids.contains(s.id.as_str()))
        .map(|s| s.id.clone())
        .collect();

    BenchBaselineComparison {
        threshold_percent,
        scenarios: scenario_deltas,
        new_scenario_ids,
        removed_scenario_ids,
        regression: any_regression,
        has_improvements,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::super::parsing::{BenchMetrics, BenchResults, BenchScenario};
    use super::*;

    fn scenario(id: &str, p95_ms: f64) -> BenchScenario {
        BenchScenario {
            id: id.to_string(),
            file: None,
            iterations: 10,
            metrics: BenchMetrics {
                mean_ms: p95_ms * 0.9,
                p50_ms: p95_ms * 0.85,
                p95_ms,
                p99_ms: p95_ms * 1.05,
                min_ms: p95_ms * 0.7,
                max_ms: p95_ms * 1.1,
            },
            memory: None,
        }
    }

    fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
        BenchResults {
            component_id: "demo".to_string(),
            iterations: 10,
            scenarios,
        }
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let run = results(vec![scenario("a", 100.0), scenario("b", 200.0)]);
        save_baseline(dir.path(), "demo", &run, None).unwrap();

        let loaded = load_baseline(dir.path(), None).unwrap();
        assert_eq!(loaded.context_id, "demo");
        assert_eq!(loaded.metadata.iterations, 10);
        assert_eq!(loaded.metadata.scenarios.len(), 2);
        assert_eq!(loaded.metadata.scenarios[0].id, "a");
        assert_eq!(loaded.metadata.scenarios[0].p95_ms, 100.0);
    }

    #[test]
    fn no_regression_when_flat() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_run = results(vec![scenario("a", 100.0)]);
        save_baseline(dir.path(), "demo", &baseline_run, None).unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 100.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(!comparison.regression);
        assert!(!comparison.has_improvements);
        assert_eq!(comparison.scenarios.len(), 1);
        assert_eq!(comparison.scenarios[0].p95_delta_ms, 0.0);
    }

    #[test]
    fn four_percent_slower_does_not_regress_at_five_percent_threshold() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 104.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(!comparison.regression);
        assert!(!comparison.scenarios[0].regression);
        assert_eq!(comparison.reasons, Vec::<String>::new());
    }

    #[test]
    fn six_percent_slower_regresses_at_five_percent_threshold() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 106.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(comparison.regression);
        assert!(comparison.scenarios[0].regression);
        assert_eq!(comparison.reasons.len(), 1);
        assert!(comparison.reasons[0].contains("a:"));
        assert!(comparison.reasons[0].contains("100.00ms"));
        assert!(comparison.reasons[0].contains("106.00ms"));
    }

    #[test]
    fn improvement_is_flagged_not_regression() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 80.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(!comparison.regression);
        assert!(comparison.has_improvements);
        assert!(comparison.scenarios[0].improvement);
        assert_eq!(comparison.scenarios[0].p95_delta_ms, -20.0);
    }

    #[test]
    fn new_scenario_is_tracked() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 100.0), scenario("b", 50.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(!comparison.regression);
        assert_eq!(comparison.new_scenario_ids, vec!["b".to_string()]);
        assert_eq!(comparison.scenarios.len(), 1); // only "a" has a baseline delta
    }

    #[test]
    fn removed_scenario_is_tracked() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0), scenario("b", 50.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        let current = results(vec![scenario("a", 100.0)]);
        let comparison = compare(&current, &baseline, 5.0);

        assert!(!comparison.regression);
        assert_eq!(comparison.removed_scenario_ids, vec!["b".to_string()]);
    }

    #[test]
    fn threshold_percent_is_configurable() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &results(vec![scenario("a", 100.0)]),
            None,
        )
        .unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        // 8% slower: passes at 10% threshold, fails at 5%.
        let current = results(vec![scenario("a", 108.0)]);
        assert!(!compare(&current, &baseline, 10.0).regression);
        assert!(compare(&current, &baseline, 5.0).regression);
    }

    #[test]
    fn zero_baseline_p95_does_not_panic_or_always_regress() {
        let dir = tempfile::tempdir().unwrap();
        save_baseline(dir.path(), "demo", &results(vec![scenario("a", 0.0)]), None).unwrap();
        let baseline = load_baseline(dir.path(), None).unwrap();

        // Even a non-trivial current p95 should not be flagged as a
        // regression when the baseline was effectively zero — that
        // almost certainly means the baseline was miscaptured.
        let current = results(vec![scenario("a", 5.0)]);
        let comparison = compare(&current, &baseline, 5.0);
        assert!(!comparison.regression);
        assert_eq!(comparison.scenarios[0].p95_delta_pct, 0.0);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path(), None).is_none());
    }

    #[test]
    fn rig_pinned_baseline_isolated_from_unpinned() {
        // The point of `rig_id`: same component, two baselines side by
        // side under different `homeboy.json` keys. Saving rig-pinned
        // must not overwrite the unpinned baseline, and loading from one
        // namespace must not surface entries from the other.
        let dir = tempfile::tempdir().unwrap();

        let unpinned_run = results(vec![scenario("workload", 100.0)]);
        let pinned_run = results(vec![scenario("workload", 200.0)]);

        save_baseline(dir.path(), "demo", &unpinned_run, None).unwrap();
        save_baseline(
            dir.path(),
            "demo",
            &pinned_run,
            Some("studio-playground-dev"),
        )
        .unwrap();

        let unpinned = load_baseline(dir.path(), None).expect("unpinned baseline present");
        assert_eq!(unpinned.metadata.scenarios[0].p95_ms, 100.0);

        let pinned = load_baseline(dir.path(), Some("studio-playground-dev"))
            .expect("rig-pinned baseline present");
        assert_eq!(pinned.metadata.scenarios[0].p95_ms, 200.0);

        // Different rig identifier returns None — no cross-rig leakage.
        assert!(load_baseline(dir.path(), Some("other-rig")).is_none());
    }
}
