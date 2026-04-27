use super::{results_with_scenarios, scenario_with_iterations};

#[test]
fn scenario_with_iterations_builds_minimal_bench_scenario() {
    let scenario = scenario_with_iterations("cold", &[("p95_ms", 12.0)], 7);

    assert_eq!(scenario.id, "cold");
    assert_eq!(scenario.iterations, 7);
    assert_eq!(scenario.metrics.get("p95_ms"), Some(12.0));
    assert!(scenario.metrics.distributions.is_empty());
    assert!(scenario.artifacts.is_empty());
    assert!(scenario.runs.is_none());
    assert!(scenario.runs_summary.is_none());
}

#[test]
fn results_with_scenarios_builds_minimal_bench_results() {
    let scenario = scenario_with_iterations("warm", &[("mean_ms", 3.0)], 2);
    let results = results_with_scenarios("bench-noop", 5, vec![scenario]);

    assert_eq!(results.component_id, "bench-noop");
    assert_eq!(results.iterations, 5);
    assert_eq!(results.scenarios.len(), 1);
    assert!(results.metric_policies.is_empty());
}
