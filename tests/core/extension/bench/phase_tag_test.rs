//! Render grouping for bench measurement-phase metadata
//! (`BenchMetricPolicy.phase` → `BenchComparisonDiff.phase_groups`).
//!
//! Phase is metadata only: it never participates in regression math. When a
//! policy declares a phase, `phase_groups` lists cold, warm, amortized, then
//! untagged metrics (alphabetical within a bucket). Without any declared
//! phase the field stays absent from the JSON envelope.

use homeboy_engine_primitives::metric_comparison::Direction;
use std::collections::BTreeMap;

use crate::extension::bench::parsing::{
    BenchMetricPhase, BenchMetricPolicy, BenchResults, BenchScenario,
};
use crate::extension::bench::report::BenchComparisonDiff;
use crate::extension::bench::test_support::scenario_with_iterations;

fn policy(direction: Direction, phase: Option<BenchMetricPhase>) -> BenchMetricPolicy {
    BenchMetricPolicy {
        direction,
        regression_threshold_percent: None,
        regression_threshold_absolute: None,
        variance_aware: false,
        min_iterations_for_variance: None,
        regression_test: None,
        phase,
    }
}

fn scenario(id: &str, metrics: &[(&str, f64)]) -> BenchScenario {
    scenario_with_iterations(id, metrics, 10)
}

fn results_with(
    scenarios: Vec<BenchScenario>,
    policies: BTreeMap<String, BenchMetricPolicy>,
) -> BenchResults {
    BenchResults {
        component_id: "demo".to_string(),
        iterations: 10,
        provenance: Default::default(),
        run_metadata: None,
        metadata: BTreeMap::new(),
        metric_groups: BTreeMap::new(),
        timeline: Vec::new(),
        span_definitions: BTreeMap::new(),
        diagnostics: Vec::new(),
        child_command_failures: Vec::new(),
        phase_events: Vec::new(),
        phase_summaries: Vec::new(),
        failure_classification: None,
        responsiveness: None,
        budget_findings: Vec::new(),
        scenarios,
        metric_policies: policies,
        metric_policy_presets: BTreeMap::new(),
    }
}

mod cases {
    use super::*;

    #[test]
    fn phase_groups_orders_cold_before_warm_before_amortized_before_untagged() {
        let mut policies = BTreeMap::new();
        // Two cold metrics, declared in non-alphabetical order to prove the
        // bucket sorts independently.
        policies.insert(
            "load_deps_ms".to_string(),
            policy(Direction::LowerIsBetter, Some(BenchMetricPhase::Cold)),
        );
        policies.insert(
            "boot_ms".to_string(),
            policy(Direction::LowerIsBetter, Some(BenchMetricPhase::Cold)),
        );
        policies.insert(
            "p95_ms".to_string(),
            policy(Direction::LowerIsBetter, Some(BenchMetricPhase::Warm)),
        );
        policies.insert(
            "first_paint_ms".to_string(),
            policy(Direction::LowerIsBetter, Some(BenchMetricPhase::Amortized)),
        );
        // `error_rate` intentionally has no policy → falls into untagged.

        let ref_r = results_with(
            vec![scenario(
                "page_load",
                &[
                    ("boot_ms", 3500.0),
                    ("load_deps_ms", 200.0),
                    ("p95_ms", 12.0),
                    ("first_paint_ms", 600.0),
                    ("error_rate", 0.0),
                ],
            )],
            policies,
        );
        // Comparison rig with the same shape so all metrics appear in the
        // diff (otherwise unmatched metrics drop out per build()'s contract).
        let other = results_with(
            vec![scenario(
                "page_load",
                &[
                    ("boot_ms", 3000.0),
                    ("load_deps_ms", 190.0),
                    ("p95_ms", 13.0),
                    ("first_paint_ms", 580.0),
                    ("error_rate", 0.0),
                ],
            )],
            BTreeMap::new(),
        );

        let diff = BenchComparisonDiff::build(("trunk", &ref_r), &[("combined-fixes", &other)]);
        let groups = diff
            .phase_groups
            .as_ref()
            .expect("phase_groups must be Some when any policy declares a phase");

        assert_eq!(
            groups.cold,
            vec!["boot_ms".to_string(), "load_deps_ms".to_string()]
        );
        assert_eq!(groups.warm, vec!["p95_ms".to_string()]);
        assert_eq!(groups.amortized, vec!["first_paint_ms".to_string()]);
        assert_eq!(groups.untagged, vec!["error_rate".to_string()]);
    }

    // Extensions that never opt into phase tagging (no policies, or policies
    // without a phase) keep the pre-phase JSON envelope.
    #[test]
    fn phaseless_diffs_omit_phase_groups() {
        let mut untagged = BTreeMap::new();
        untagged.insert("p95_ms".to_string(), policy(Direction::LowerIsBetter, None));

        for policies in [untagged, BTreeMap::new()] {
            let ref_r = results_with(vec![scenario("s", &[("p95_ms", 100.0)])], policies);
            let other = results_with(vec![scenario("s", &[("p95_ms", 105.0)])], BTreeMap::new());

            let diff = BenchComparisonDiff::build(("ref", &ref_r), &[("other", &other)]);
            assert!(diff.phase_groups.is_none());
            let raw = serde_json::to_string(&diff).unwrap();
            assert!(!raw.contains("phase_groups"), "got: {raw}");
        }
    }
}
