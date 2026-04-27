//! Cross-spawn aggregation smokes for `homeboy bench --runs N`.
//!
//! The contract under test:
//! 1. The default `runs=1` envelope shape remains flat: no `runs` or
//!    `runs_summary` keys appear unless aggregation is requested.
//! 2. Multi-run aggregation keeps top-level metrics as cross-run p50 values
//!    while preserving per-run snapshots and stdev/cv_pct/n diagnostics.
//! 3. Distribution math uses population stdev and protects zero-mean CV.
//! 4. Scenarios missing from some runs aggregate from the runs that emitted
//!    them instead of failing the whole bench.

use std::collections::BTreeMap;

use crate::extension::bench::parsing::{BenchMetrics, BenchResults, BenchScenario};
use crate::extension::bench::run::aggregate_runs;

fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
    let mut values = BTreeMap::new();
    for (name, value) in metrics {
        values.insert((*name).to_string(), *value);
    }

    BenchScenario {
        id: id.to_string(),
        file: None,
        source: None,
        default_iterations: None,
        tags: Vec::new(),
        iterations: 1,
        metrics: BenchMetrics {
            values,
            distributions: BTreeMap::new(),
        },
        memory: None,
        runs: None,
        runs_summary: None,
    }
}

fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
    BenchResults {
        component_id: "bench-noop".to_string(),
        iterations: 5,
        scenarios,
        metric_policies: BTreeMap::new(),
    }
}

fn approx_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

mod cases {
    use super::*;

    #[test]
    fn runs_default_one_preserves_envelope_shape() {
        let single = results(vec![scenario("__bootstrap", &[("install_ms", 3107.66)])]);
        let raw = serde_json::to_string(&single).unwrap();

        assert_eq!(
            raw,
            r#"{"component_id":"bench-noop","iterations":5,"scenarios":[{"id":"__bootstrap","iterations":1,"metrics":{"install_ms":3107.66}}]}"#
        );
        assert!(!raw.contains("runs"));
        assert!(!raw.contains("runs_summary"));
    }

    #[test]
    fn runs_three_aggregates_correctly() {
        let aggregated = aggregate_runs(&[
            results(vec![scenario("__bootstrap", &[("install_ms", 100.0)])]),
            results(vec![scenario("__bootstrap", &[("install_ms", 200.0)])]),
            results(vec![scenario("__bootstrap", &[("install_ms", 300.0)])]),
        ])
        .unwrap();

        let scenario = aggregated.scenarios.first().unwrap();
        assert_eq!(scenario.metrics.get("install_ms"), Some(200.0));
        assert_eq!(scenario.runs.as_ref().unwrap().len(), 3);

        let summary = scenario
            .runs_summary
            .as_ref()
            .unwrap()
            .get("install_ms")
            .unwrap();
        approx_eq(summary.stdev, (20000.0_f64 / 3.0).sqrt());
        approx_eq(summary.cv_pct, summary.stdev / 200.0 * 100.0);
        assert_eq!(summary.n, 3);
    }

    #[test]
    fn runs_handles_zero_mean_for_cv() {
        let aggregated = aggregate_runs(&[
            results(vec![scenario("zero", &[("count", 0.0)])]),
            results(vec![scenario("zero", &[("count", 0.0)])]),
            results(vec![scenario("zero", &[("count", 0.0)])]),
        ])
        .unwrap();

        let summary = aggregated.scenarios[0]
            .runs_summary
            .as_ref()
            .unwrap()
            .get("count")
            .unwrap();
        assert_eq!(summary.cv_pct, 0.0);
        assert!(summary.cv_pct.is_finite());
    }

    #[test]
    fn runs_skip_serializes_when_none() {
        let raw = serde_json::to_string(&scenario("plain", &[("p95_ms", 12.0)])).unwrap();

        assert!(!raw.contains("runs"));
        assert!(!raw.contains("runs_summary"));
    }

    #[test]
    fn runs_distribution_math_population_stdev() {
        let aggregated = aggregate_runs(&[
            results(vec![scenario("known", &[("value", 1.0)])]),
            results(vec![scenario("known", &[("value", 2.0)])]),
            results(vec![scenario("known", &[("value", 3.0)])]),
        ])
        .unwrap();

        let summary = aggregated.scenarios[0]
            .runs_summary
            .as_ref()
            .unwrap()
            .get("value")
            .unwrap();
        approx_eq(summary.stdev, (2.0_f64 / 3.0).sqrt());
        assert_eq!(summary.n, 3);
    }

    #[test]
    fn runs_handles_missing_scenario_in_some_runs() {
        let aggregated = aggregate_runs(&[
            results(vec![scenario("x", &[("install_ms", 10.0)])]),
            results(vec![scenario("other", &[("install_ms", 999.0)])]),
            results(vec![scenario("x", &[("install_ms", 30.0)])]),
        ])
        .unwrap();

        let scenario = aggregated
            .scenarios
            .iter()
            .find(|scenario| scenario.id == "x")
            .unwrap();
        assert_eq!(scenario.metrics.get("install_ms"), Some(20.0));
        assert_eq!(scenario.runs.as_ref().unwrap().len(), 2);
        assert_eq!(
            scenario
                .runs_summary
                .as_ref()
                .unwrap()
                .get("install_ms")
                .unwrap()
                .n,
            2
        );
    }
}
