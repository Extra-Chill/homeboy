use std::path::Path;

use super::output::{
    compare_trace_aggregates, TraceAggregateIdentity, TraceAggregateInput, TraceAggregateSpanInput,
};

#[test]
fn trace_compare_reports_median_and_average_deltas() {
    let before = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "boot_to_ready".to_string(),
                    n: 5,
                },
                median_ms: Some(100),
                avg_ms: Some(110.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "large_improvement".to_string(),
                    n: 5,
                },
                median_ms: Some(300),
                avg_ms: Some(300.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "large_regression".to_string(),
                    n: 5,
                },
                median_ms: Some(80),
                avg_ms: Some(80.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "before_only".to_string(),
                    n: 5,
                },
                median_ms: Some(25),
                avg_ms: Some(25.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 1,
                metadata: None,
            },
        ],
        metrics: Vec::new(),
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
    };
    let after = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "boot_to_ready".to_string(),
                    n: 5,
                },
                median_ms: Some(125),
                avg_ms: Some(121.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "large_improvement".to_string(),
                    n: 5,
                },
                median_ms: Some(100),
                avg_ms: Some(100.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "large_regression".to_string(),
                    n: 5,
                },
                median_ms: Some(200),
                avg_ms: Some(200.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
            TraceAggregateSpanInput {
                identity: TraceAggregateIdentity {
                    id: "after_only".to_string(),
                    n: 3,
                },
                median_ms: Some(75),
                avg_ms: Some(80.0),
                max_ms: None,
                max_run_index: None,
                max_artifact_path: None,
                failures: 0,
                metadata: None,
            },
        ],
        metrics: Vec::new(),
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
    };

    let compare = compare_trace_aggregates(
        Path::new("before.json"),
        before,
        Path::new("after.json"),
        after,
    );

    assert_eq!(compare.command, "trace.compare.spans");
    assert_eq!(compare.span_count, 5);
    assert_eq!(compare.spans[0].id, "large_improvement");
    assert_eq!(compare.spans[1].id, "large_regression");
    assert_eq!(compare.spans[2].id, "boot_to_ready");
    let changed = compare
        .spans
        .iter()
        .find(|span| span.id == "boot_to_ready")
        .expect("changed span");
    assert_eq!(changed.before_median_ms, Some(100));
    assert_eq!(changed.after_median_ms, Some(125));
    assert_eq!(changed.median_delta_ms, Some(25));
    assert_eq!(changed.median_delta_percent, Some(25.0));
    assert_eq!(changed.avg_delta_ms, Some(11.0));
    assert_eq!(changed.avg_delta_percent, Some(10.0));

    let before_only = compare
        .spans
        .iter()
        .find(|span| span.id == "before_only")
        .expect("before-only span");
    assert_eq!(before_only.after_n, None);
    assert_eq!(before_only.median_delta_ms, None);
}
