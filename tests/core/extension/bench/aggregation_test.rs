use crate::extension::bench::test_support::{results_with_scenarios, scenario_with_iterations};
use crate::observation::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult, ObservationSpanStatus,
};

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

#[test]
fn aggregate_runs_preserves_and_summarizes_spans() {
    let mut first = scenario_with_iterations("s", &[("ms", 10.0)], 1);
    first.timeline = vec![event(0, "runner", "start"), event(20, "runner", "ready")];
    first.span_definitions = vec![span_definition()];
    first.span_results = vec![span_result(20)];

    let mut second = scenario_with_iterations("s", &[("ms", 30.0)], 1);
    second.timeline = vec![event(0, "runner", "start"), event(40, "runner", "ready")];
    second.span_definitions = vec![span_definition()];
    second.span_results = vec![span_result(40)];

    let aggregated = aggregate_runs(&[
        results_with_scenarios("bench-noop", 5, vec![first]),
        results_with_scenarios("bench-noop", 5, vec![second]),
    ])
    .unwrap();

    let scenario = aggregated.scenarios.first().unwrap();
    assert_eq!(scenario.span_results.len(), 1);
    assert_eq!(scenario.span_results[0].status, ObservationSpanStatus::Ok);
    assert_eq!(scenario.span_results[0].duration_ms, Some(30));

    let runs = scenario.runs.as_ref().unwrap();
    assert_eq!(runs[0].span_results[0].duration_ms, Some(20));
    assert_eq!(runs[1].span_results[0].duration_ms, Some(40));
    assert_eq!(runs[0].timeline.len(), 2);
}

fn event(t_ms: u64, source: &str, event: &str) -> ObservationEvent {
    ObservationEvent {
        t_ms,
        source: source.to_string(),
        event: event.to_string(),
        data: Default::default(),
    }
}

fn span_definition() -> ObservationSpanDefinition {
    ObservationSpanDefinition {
        id: "startup".to_string(),
        from: "runner.start".to_string(),
        to: "runner.ready".to_string(),
    }
}

fn span_result(duration_ms: u64) -> ObservationSpanResult {
    ObservationSpanResult {
        id: "startup".to_string(),
        from: "runner.start".to_string(),
        to: "runner.ready".to_string(),
        status: ObservationSpanStatus::Ok,
        duration_ms: Some(duration_ms),
        from_t_ms: Some(0),
        to_t_ms: Some(duration_ms),
        missing: Vec::new(),
        message: None,
    }
}
