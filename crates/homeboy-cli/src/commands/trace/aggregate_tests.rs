use homeboy::core::extension::trace as extension_trace;

use super::aggregate::aggregate_span;
use super::aggregate_test_support::aggregate_samples;
use super::render_aggregate_markdown;

#[test]
fn aggregate_span_reports_percentiles_when_sample_size_is_sufficient() {
    let span = aggregate_span(
        "boot_to_ready".to_string(),
        aggregate_samples(&[
            200, 10, 190, 20, 180, 30, 170, 40, 160, 50, 150, 60, 140, 70, 130, 80, 120, 90, 110,
            100,
        ]),
        2,
    );

    assert_eq!(span.n, 20);
    assert_eq!(span.min_ms, Some(10));
    assert_eq!(span.median_ms, Some(105));
    assert_eq!(span.avg_ms, Some(105.0));
    assert!(span
        .stddev_ms
        .is_some_and(|value| (value - 57.66281297335398).abs() < 0.000001));
    assert_eq!(span.p75_ms, Some(150));
    assert_eq!(span.p90_ms, Some(180));
    assert_eq!(span.p95_ms, Some(190));
    assert_eq!(span.max_ms, Some(200));
    assert_eq!(span.max_run_index, Some(1));
    assert_eq!(
        span.max_artifact_path.as_deref(),
        Some("/tmp/trace-run-1.json")
    );
    assert_eq!(span.failures, 2);
    assert_eq!(span.samples.len(), 20);
    assert_eq!(span.samples[0].run_index, 1);
    assert_eq!(span.samples[0].duration_ms, 200);
    assert_eq!(span.samples[0].artifact_path, "/tmp/trace-run-1.json");
}

#[test]
fn aggregate_span_reports_run_and_artifact_for_max_sample() {
    let span = aggregate_span(
        "submit_to_running".to_string(),
        aggregate_samples(&[340, 11_757, 410]),
        0,
    );

    assert_eq!(span.max_ms, Some(11_757));
    assert_eq!(span.max_run_index, Some(2));
    assert_eq!(
        span.max_artifact_path.as_deref(),
        Some("/tmp/trace-run-2.json")
    );
}

#[test]
fn aggregate_span_omits_percentiles_for_small_sample_sizes() {
    let single = aggregate_span("single".to_string(), aggregate_samples(&[42]), 0);
    assert_eq!(single.min_ms, Some(42));
    assert_eq!(single.median_ms, Some(42));
    assert_eq!(single.avg_ms, Some(42.0));
    assert_eq!(single.stddev_ms, Some(0.0));
    assert_eq!(single.p75_ms, None);
    assert_eq!(single.p90_ms, None);
    assert_eq!(single.p95_ms, None);
    assert_eq!(single.max_ms, Some(42));

    let four_samples = aggregate_span("four".to_string(), aggregate_samples(&[10, 20, 30, 40]), 0);
    assert_eq!(four_samples.p75_ms, Some(30));
    assert_eq!(four_samples.p90_ms, None);
    assert_eq!(four_samples.p95_ms, None);
}

#[test]
fn aggregate_markdown_includes_percentile_columns() {
    let aggregate = extension_trace::TraceAggregateOutput {
        command: "trace.aggregate.spans",
        passed: true,
        status: "pass".to_string(),
        component: "studio".to_string(),
        scenario_id: "create-site".to_string(),
        phase_preset: None,
        repeat: 20,
        run_count: 20,
        failure_count: 0,
        exit_code: 0,
        rig_state: None,
        schedule: None,
        run_order: Vec::new(),
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![aggregate_span(
            "boot_to_ready".to_string(),
            aggregate_samples(&((1..=20).map(|value| value * 10).collect::<Vec<_>>())),
            0,
        )],
        metrics: Vec::new(),
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
        focus_span_ids: Vec::new(),
        focus_spans: Vec::new(),
        classification_summaries: Vec::new(),
        unmatched_span_metadata_ids: Vec::new(),
        profile: None,
    };

    let markdown = render_aggregate_markdown(&aggregate);

    assert!(markdown
        .contains("| Span | n | min | median | avg | stddev | p75 | p90 | p95 | max | failures |"));
    assert!(markdown.contains(
        "| `boot_to_ready` | 20 | 10ms | 105ms | 105.0ms | 57.7ms | 150ms | 180ms | 190ms | 200ms | 0 |"
    ));
    assert!(markdown.contains("| Span | max | max run | max artifact |"));
    assert!(markdown.contains("| `boot_to_ready` | 200ms | 20 | `/tmp/trace-run-20.json` |"));
}

#[test]
fn aggregate_json_serializes_available_percentiles() {
    let span = aggregate_span(
        "boot_to_ready".to_string(),
        aggregate_samples(&((1..=20).map(|value| value * 10).collect::<Vec<_>>())),
        0,
    );

    let value = serde_json::to_value(&span).expect("span serializes");

    assert_eq!(value["p75_ms"], 150);
    assert_eq!(value["p90_ms"], 180);
    assert_eq!(value["p95_ms"], 190);
    assert!(value["stddev_ms"]
        .as_f64()
        .is_some_and(|stddev| stddev > 57.6));
    assert_eq!(value["samples"].as_array().expect("samples").len(), 20);
    assert_eq!(value["samples"][0]["run_index"], 1);
    assert_eq!(value["samples"][0]["duration_ms"], 10);
}

#[test]
fn aggregate_json_omits_unavailable_percentiles() {
    let span = aggregate_span("boot_to_ready".to_string(), aggregate_samples(&[10, 20]), 0);

    let value = serde_json::to_value(&span).expect("span serializes");

    assert!(value.get("p75_ms").is_none());
    assert!(value.get("p90_ms").is_none());
    assert!(value.get("p95_ms").is_none());
}
