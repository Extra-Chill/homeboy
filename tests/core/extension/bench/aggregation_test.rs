use crate::extension::bench::test_support::{results_with_scenarios, scenario_with_iterations};

use super::aggregate_runs;

#[test]
fn test_aggregate_runs() {
    let aggregated = aggregate_runs(&[
        results_with_scenarios(
            "bench-noop",
            5,
            vec![scenario_with_iterations("s", &[("ms", 10.0)], 1)],
        ),
        results_with_scenarios(
            "bench-noop",
            5,
            vec![scenario_with_iterations("s", &[("ms", 30.0)], 1)],
        ),
    ])
    .unwrap();

    let scenario = aggregated.scenarios.first().unwrap();
    assert_eq!(scenario.metrics.get("ms"), Some(20.0));
    assert_eq!(scenario.metrics.distribution("ms"), Some(&[10.0, 30.0][..]));
    assert_eq!(scenario.runs.as_ref().unwrap().len(), 2);
    assert_eq!(scenario.runs_summary.as_ref().unwrap()["ms"].n, 2);
}

#[test]
fn aggregate_runs_rejects_mismatched_component_id() {
    let err = aggregate_runs(&[
        results_with_scenarios("one", 5, vec![]),
        results_with_scenarios("two", 5, vec![]),
    ])
    .expect_err("component mismatch must fail");

    assert!(format!("{}", err).contains("component_id"));
}

#[test]
fn aggregate_runs_rejects_mismatched_iterations() {
    let err = aggregate_runs(&[
        results_with_scenarios("bench-noop", 5, vec![]),
        results_with_scenarios("bench-noop", 7, vec![]),
    ])
    .expect_err("iteration mismatch must fail");

    assert!(format!("{}", err).contains("iterations"));
}
