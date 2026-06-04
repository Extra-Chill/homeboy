use std::fs;
use std::path::Path;

use homeboy::core::extension::trace as extension_trace;

use super::bundle::{write_trace_experiment_bundle, TraceExperimentBundleRequest};
use super::output::{
    compare_trace_aggregates, compare_trace_aggregates_with_focus, parse_trace_aggregate_input,
    render_compare_markdown, run_compare, TraceAggregateInput, TraceAggregateSpanInput,
};
use super::*;

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
            span_input("boot_to_ready", 5, Some(100), Some(110.0), 0),
            span_input("large_improvement", 5, Some(300), Some(300.0), 0),
            span_input("large_regression", 5, Some(80), Some(80.0), 0),
            span_input("before_only", 5, Some(25), Some(25.0), 1),
        ],
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
            span_input("boot_to_ready", 5, Some(125), Some(121.0), 0),
            span_input("large_improvement", 5, Some(100), Some(100.0), 0),
            span_input("large_regression", 5, Some(200), Some(200.0), 0),
            span_input("after_only", 3, Some(75), Some(80.0), 0),
        ],
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

#[test]
fn trace_compare_focus_spans_report_independent_regression_status() {
    let before = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![
            span_input("focused", 6, Some(100), Some(100.0), 0),
            span_input("unfocused", 6, Some(100), Some(100.0), 0),
        ],
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
            span_input("focused", 6, Some(130), Some(130.0), 0),
            span_input("unfocused", 6, Some(250), Some(250.0), 0),
        ],
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
    };

    let compare = compare_trace_aggregates_with_focus(
        Path::new("before.json"),
        before,
        Path::new("after.json"),
        after,
        &["focused".to_string()],
        20.0,
        10,
    );

    assert_eq!(compare.span_count, 2);
    assert_eq!(compare.spans.len(), 2);
    assert_eq!(compare.focus_span_ids, vec!["focused"]);
    assert_eq!(compare.focus_spans.len(), 1);
    assert_eq!(compare.focus_spans[0].id, "focused");
    assert_eq!(compare.focus_regression_count, 1);
    assert_eq!(compare.focus_failure_count, 0);
    assert_eq!(compare.focus_status.as_deref(), Some("fail"));
}

#[test]
fn trace_compare_includes_classification_summary_output() {
    let metadata = extension_trace::TraceSpanMetadata {
        critical: true,
        blocking: true,
        cacheable: true,
        prewarmable: false,
        deferrable: false,
        blocks: Some("first_site_render".to_string()),
        category: None,
    };
    let before = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![TraceAggregateSpanInput {
            metadata: Some(metadata.clone()),
            ..span_input("boot_to_ready", 5, Some(100), Some(100.0), 0)
        }],
        guardrail_failure_count: 0,
        guardrails: Vec::new(),
    };
    let after = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![TraceAggregateSpanInput {
            metadata: Some(metadata),
            ..span_input("boot_to_ready", 5, Some(125), Some(125.0), 0)
        }],
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
    };

    let compare = compare_trace_aggregates(
        Path::new("before.json"),
        before,
        Path::new("after.json"),
        after,
    );

    assert!(compare.classification_summaries.iter().any(|summary| {
        summary.classification == "cacheable_critical"
            && summary.before_total_median_ms == Some(100)
            && summary.after_total_median_ms == Some(125)
            && summary.median_delta_ms == Some(25)
    }));
    let markdown = render_compare_markdown(&compare);
    assert!(markdown.contains("## Critical Path Classification"));
    assert!(markdown.contains("| `cacheable_critical` | 1 | 100ms | 125ms | **+25ms** |"));
}

#[test]
fn trace_compare_reports_guardrail_failures() {
    let before = TraceAggregateInput {
        component: Some("studio".to_string()),
        scenario_id: Some("create-site".to_string()),
        phase_preset: None,
        repeat: None,
        rig_state: None,
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![span_input("boot", 1, Some(100), Some(100.0), 0)],
        guardrails: vec![extension_trace::TraceGuardrailOutput {
            label: "baseline smoke".to_string(),
            source: "rig:baseline".to_string(),
            passed: true,
            status: "pass".to_string(),
            failure: None,
        }],
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
        spans: vec![span_input("boot", 1, Some(90), Some(90.0), 0)],
        guardrails: vec![extension_trace::TraceGuardrailOutput {
            label: "behavior smoke".to_string(),
            source: "rig:variant".to_string(),
            passed: false,
            status: "fail".to_string(),
            failure: Some("assertion changed".to_string()),
        }],
        guardrail_failure_count: 1,
    };

    let compare = compare_trace_aggregates(
        Path::new("before.json"),
        before,
        Path::new("after.json"),
        after,
    );

    assert_eq!(compare.guardrail_status.as_deref(), Some("fail"));
    assert_eq!(compare.guardrail_failure_count, 1);
    assert_eq!(compare.before_guardrails.len(), 1);
    assert_eq!(compare.after_guardrails[0].label, "behavior smoke");
    let markdown = render_compare_markdown(&compare);
    assert!(markdown.contains("## After Guardrails"));
    assert!(markdown.contains("assertion changed"));
}

#[test]
fn trace_compare_exits_nonzero_for_guardrail_failures() {
    let dir = tempfile::TempDir::new().expect("compare dir");
    let before_path = dir.path().join("before.json");
    let after_path = dir.path().join("after.json");
    fs::write(
        &before_path,
        serde_json::json!({
            "component": "studio",
            "scenario_id": "create-site",
            "spans": [{ "id": "boot", "n": 1, "median_ms": 100, "avg_ms": 100.0, "failures": 0 }],
            "guardrails": [{ "label": "baseline smoke", "source": "rig:baseline", "passed": true, "status": "pass" }]
        })
        .to_string(),
    )
    .expect("write before");
    fs::write(
        &after_path,
        serde_json::json!({
            "component": "studio",
            "scenario_id": "create-site",
            "spans": [{ "id": "boot", "n": 1, "median_ms": 80, "avg_ms": 80.0, "failures": 0 }],
            "guardrails": [{ "label": "variant smoke", "source": "rig:variant", "passed": false, "status": "fail", "failure": "behavior changed" }],
            "guardrail_failure_count": 1
        })
        .to_string(),
    )
    .expect("write after");

    let (_output, exit_code) = run_compare(TraceArgs {
        comp: PositionalComponentArgs {
            component: Some("compare".to_string()),
            path: None,
        },
        component_arg: None,
        scenario: Some(before_path.to_string_lossy().to_string()),
        scenario_arg: None,
        compare_after: Some(after_path),
        rig: None,
        profile: None,
        profiles: false,
        setting_args: SettingArgs::default(),
        json_summary: false,
        report: None,
        experiment: None,
        repeat: 1,
        aggregate: None,
        schedule: TraceSchedule::Grouped,
        focus_spans: Vec::new(),
        spans: Vec::new(),
        phases: Vec::new(),
        attachments: Vec::new(),
        phase_preset: None,
        baseline_args: BaselineArgs::default(),
        regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        overlays: Vec::new(),
        variants: Vec::new(),
        matrix: TraceVariantMatrixMode::None,
        axes: Vec::new(),
        matrix_env: Vec::new(),
        output_dir: None,
        keep_overlay: false,
        stale: false,
        force: false,
    })
    .expect("compare should run");

    assert_eq!(exit_code, 1);
}

#[test]
fn trace_compare_accepts_json_summary_envelope_outputs() {
    let input = parse_trace_aggregate_input(
        r#"{
                "success": true,
                "data": {
                    "command": "trace.aggregate.spans",
                    "component": "studio",
                    "scenario_id": "create-site",
                    "spans": [
                        {
                            "id": "submit_to_running",
                            "n": 5,
                            "median_ms": 6059,
                            "avg_ms": 6019.8,
                            "failures": 0
                        }
                    ]
                }
            }"#,
    )
    .expect("json summary envelope should parse");

    assert_eq!(input.component.as_deref(), Some("studio"));
    assert_eq!(input.scenario_id.as_deref(), Some("create-site"));
    assert_eq!(input.spans.len(), 1);
    assert_eq!(input.spans[0].id, "submit_to_running");
    assert_eq!(input.spans[0].median_ms, Some(6059));
}

#[test]
fn trace_compare_markdown_and_experiment_bundle_render_artifacts() {
    let compare = extension_trace::TraceCompareOutput {
        command: "trace.compare.spans",
        before_path: "before.json".to_string(),
        after_path: "after.json".to_string(),
        before_component: Some("studio".to_string()),
        after_component: Some("studio".to_string()),
        before_scenario_id: Some("create-site".to_string()),
        after_scenario_id: Some("create-site".to_string()),
        span_count: 1,
        spans: vec![extension_trace::TraceCompareSpanOutput {
            id: "boot_to_ready".to_string(),
            before_n: Some(5),
            after_n: Some(5),
            before_median_ms: Some(100),
            after_median_ms: Some(125),
            median_delta_ms: Some(25),
            median_delta_percent: Some(25.0),
            before_avg_ms: Some(110.0),
            after_avg_ms: Some(121.0),
            avg_delta_ms: Some(11.0),
            avg_delta_percent: Some(10.0),
            before_failures: Some(0),
            after_failures: Some(0),
        }],
        focus_span_ids: Vec::new(),
        focus_spans: Vec::new(),
        focus_regression_count: 0,
        focus_failure_count: 0,
        focus_status: None,
        before_guardrails: Vec::new(),
        after_guardrails: Vec::new(),
        guardrail_failure_count: 0,
        guardrail_status: None,
        classification_summaries: Vec::new(),
    };

    let markdown = render_compare_markdown(&compare);

    assert!(markdown.contains("# Trace Compare"));
    assert!(markdown.contains("| Span | before median | after median | median delta | median % | before avg | after avg | avg delta | avg % |"));
    assert!(markdown.contains(
        "| `boot_to_ready` | 100ms | 125ms | **+25ms** | +25.0% | 110.0ms | 121.0ms | **+11.0ms** | +10.0% |"
    ));

    let dir = tempfile::TempDir::new().expect("bundle dir");
    let before_path = dir.path().join("baseline-source.json");
    let after_path = dir.path().join("variant-source.json");
    let overlay_path = dir.path().join("fast-install.patch");
    fs::write(&overlay_path, "diff --git a/install.ts b/install.ts\n").expect("write overlay");

    let before_json = serde_json::json!({
        "command": "trace.aggregate.spans",
        "component": "studio",
        "scenario_id": "studio-fast-install",
        "phase_preset": "startup",
        "repeat": 3,
        "rig_state": {
            "rig_id": "studio-rig",
            "captured_at": "2026-05-02T00:00:00Z",
            "components": {
                "studio": { "path": "/repo/studio", "branch": "main", "sha": "abc123" }
            }
        },
        "runs": [
            { "index": 1, "passed": true, "status": "pass", "exit_code": 0, "artifact_path": "/tmp/baseline-1.json" }
        ],
        "spans": [
            { "id": "install", "n": 3, "median_ms": 120, "avg_ms": 130.0, "max_ms": 160, "max_run_index": 1, "max_artifact_path": "/tmp/baseline-1.json", "failures": 0 }
        ]
    })
    .to_string();
    let after_json = serde_json::json!({
        "command": "trace.aggregate.spans",
        "component": "studio",
        "scenario_id": "studio-fast-install",
        "phase_preset": "startup",
        "repeat": 3,
        "rig_state": {
            "rig_id": "studio-rig",
            "captured_at": "2026-05-02T00:00:00Z",
            "components": {
                "studio": { "path": "/repo/studio", "branch": "trace-experiment-bundles", "sha": "def456" }
            }
        },
        "overlays": [
            { "path": overlay_path, "component_path": "/repo/studio", "touched_files": ["install.ts"], "kept": false }
        ],
        "runs": [
            { "index": 1, "passed": false, "status": "fail", "exit_code": 1, "artifact_path": "/tmp/variant-1.json", "failure": "assertion failed" }
        ],
        "spans": [
            { "id": "install", "n": 2, "median_ms": 80, "avg_ms": 90.0, "max_ms": 140, "max_run_index": 1, "max_artifact_path": "/tmp/variant-1.json", "failures": 1 }
        ]
    })
    .to_string();
    fs::write(&before_path, &before_json).expect("write before");
    fs::write(&after_path, &after_json).expect("write after");

    let before_for_compare = parse_trace_aggregate_input(&before_json).expect("before compare");
    let after_for_compare = parse_trace_aggregate_input(&after_json).expect("after compare");
    let compare = compare_trace_aggregates(
        &before_path,
        before_for_compare,
        &after_path,
        after_for_compare,
    );
    let before = parse_trace_aggregate_input(&before_json).expect("before bundle");
    let after = parse_trace_aggregate_input(&after_json).expect("after bundle");

    let bundle_dir = write_trace_experiment_bundle(TraceExperimentBundleRequest {
        name: "studio-fast-install",
        bundle_root: Some(dir.path()),
        command: "homeboy trace compare baseline-source.json variant-source.json --experiment studio-fast-install".to_string(),
        before_path: &before_path,
        before_json: &before_json,
        before: &before,
        after_path: &after_path,
        after_json: &after_json,
        after: &after,
        compare: &compare,
    })
    .expect("write bundle");

    assert!(bundle_dir.join("baseline.json").is_file());
    assert!(bundle_dir
        .join("variant-studio-fast-install.json")
        .is_file());
    assert!(bundle_dir
        .join("compare-studio-fast-install.json")
        .is_file());
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(bundle_dir.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    assert!(manifest["command"]
        .as_str()
        .unwrap()
        .contains("trace compare"));
    assert_eq!(manifest["variants"][0]["role"], "baseline");
    assert_eq!(manifest["variants"][0]["phase_preset"], "startup");
    assert_eq!(manifest["variants"][0]["repeat"], 3);
    assert_eq!(manifest["variants"][0]["rig_id"], "studio-rig");
    assert_eq!(manifest["variants"][0]["components"][0]["sha"], "abc123");
    assert_eq!(
        manifest["variants"][1]["artifact_paths"][0],
        "/tmp/variant-1.json"
    );
    assert_eq!(manifest["overlays"][0]["touched_files"][0], "install.ts");
    assert_eq!(
        manifest["overlays"][0]["sha256"].as_str().unwrap().len(),
        64
    );
    assert!(Path::new(manifest["overlays"][0]["bundle_path"].as_str().unwrap()).is_file());

    let report = fs::read_to_string(bundle_dir.join("report.md")).expect("read report");
    assert!(report.contains("## Top Median Improvements"));
    assert!(report.contains("## Top Average Improvements"));
    assert!(report.contains("## Variant Failures and Outliers"));
    assert!(report.contains("/tmp/variant-1.json"));
}

fn span_input(
    id: &str,
    n: usize,
    median_ms: Option<u64>,
    avg_ms: Option<f64>,
    failures: usize,
) -> TraceAggregateSpanInput {
    TraceAggregateSpanInput {
        id: id.to_string(),
        n,
        median_ms,
        avg_ms,
        max_ms: None,
        max_run_index: None,
        max_artifact_path: None,
        failures,
        metadata: None,
    }
}
