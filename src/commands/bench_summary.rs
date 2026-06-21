//! Compact human-readable summaries for `homeboy bench`.
//!
//! `homeboy bench` serializes a large `BenchOutput` JSON envelope. Dumping
//! the full payload to a terminal buries the signal — pass/fail, the
//! persisted run ID, runner, component SHA, selected scenarios, key
//! metrics, and (critically) the artifact pointers — under hundreds of
//! metric lines (#3257).
//!
//! This module renders a compact summary from the serialized `BenchOutput`
//! value, mirroring the agent-task compact-summary pattern. The full JSON
//! remains available via `--json` (printed to stdout) and is always written
//! to the `--output` file unchanged, so no data is lost — only the default
//! human presentation becomes compact.
//!
//! Artifact pointers (shared-state files, runner bundle paths,
//! scenario-specific artifacts) and the `homeboy runs show` follow-up
//! command are surfaced near the top so they are easy to find (#3260).

use serde_json::Value;

use super::summary_json::{array_len, string_value, u64_value, usize_value, value_at};

mod coverage;
mod hotspots;
pub(crate) use self::coverage::bench_coverage_lines;
pub(crate) use self::hotspots::bench_hotspot_lines;

/// Render a compact summary for a serialized `BenchOutput` value. Returns
/// `None` for variants where the full JSON is the better default (lists,
/// embedded `runs` observation output, matrix fan-out), so those paths keep
/// their existing presentation.
pub(crate) fn render_bench_summary(payload: &Value) -> Option<String> {
    let variant = payload.get("variant").and_then(Value::as_str)?;
    let inner = payload.get("payload")?;
    match variant {
        "single" => Some(render_single_summary(inner)),
        "comparison" => Some(render_comparison_summary(inner)),
        "comparison_summary" => Some(render_comparison_summary(inner)),
        // List, observation, and matrix fan-out keep full JSON output.
        _ => None,
    }
}

fn render_single_summary(output: &Value) -> String {
    let passed = output
        .get("passed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = string_value(output, &["status"]).unwrap_or("unknown");
    let component = string_value(output, &["component"]).unwrap_or("<unknown>");
    let iterations = u64_value(output, &["iterations"]).unwrap_or(0);

    let mut lines = vec![
        "Bench run".to_string(),
        format!("Result: {}", pass_fail(passed)),
        format!("Status: {status}"),
        format!("Component: {component}"),
        format!("Iterations: {iterations}"),
    ];

    if let Some(run_id) = persisted_run_id(output) {
        lines.push(format!("Run: {run_id}"));
    }
    if let Some(runner) = runner_id(output) {
        lines.push(format!("Runner: {runner}"));
    }
    if let Some(sha) = component_sha(output) {
        lines.push(format!("Component SHA: {sha}"));
    }

    let scenarios = scenario_ids(output);
    if !scenarios.is_empty() {
        lines.push(format!(
            "Scenarios ({}): {}",
            scenarios.len(),
            scenarios.join(", ")
        ));
    }

    lines.extend(key_metric_lines(output));
    lines.extend(bench_hotspot_lines(output));
    lines.extend(bench_coverage_lines(output));
    lines.extend(bench_regression_threshold_lines(output));
    lines.extend(gate_failure_lines(output));
    lines.extend(key_artifact_lines(
        output,
        persisted_run_id(output).as_deref(),
    ));
    lines.extend(artifact_lines(output));
    lines.extend(failure_lines(output));
    lines.extend(hint_lines(output));
    lines.extend(next_command_lines(output, persisted_run_id(output)));

    finish(lines)
}

fn render_comparison_summary(output: &Value) -> String {
    let passed = output
        .get("passed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let component = string_value(output, &["component"]).unwrap_or("<unknown>");
    let iterations = u64_value(output, &["iterations"]).unwrap_or(0);

    let mut lines = vec![
        "Bench comparison".to_string(),
        format!("Result: {}", pass_fail(passed)),
        format!("Component: {component}"),
        format!("Iterations: {iterations}"),
    ];

    // Per-rig pass/fail roll-up. Comparison output carries a `rigs` array
    // (full) or `summary.rigs` (json_summary). Render whichever is present.
    let rig_lines = comparison_rig_lines(output);
    if !rig_lines.is_empty() {
        lines.extend(rig_lines);
    }

    let regressions = usize_value(output, &["regressions"])
        .or_else(|| array_len(output, &["failures"]))
        .unwrap_or(0);
    if regressions > 0 {
        lines.push(format!("Regressions/failures: {regressions}"));
    }

    lines.extend(bench_regression_threshold_lines(output));
    lines.extend(comparison_artifact_lines(output));
    lines.extend(hint_lines(output));

    finish(lines)
}

// --- field extraction --------------------------------------------------

/// The persisted observation run id, sourced from the structured
/// `persisted_run` pointer the bench command attaches once results are
/// stored.
fn persisted_run_id(output: &Value) -> Option<String> {
    string_value(output, &["persisted_run", "run_id"]).map(str::to_string)
}

fn runner_id(output: &Value) -> Option<String> {
    string_value(output, &["results", "run_metadata", "runner", "extension"])
        .or_else(|| string_value(output, &["results", "run_metadata", "runner", "path"]))
        .map(str::to_string)
}

fn component_sha(output: &Value) -> Option<String> {
    // Rig-pinned runs capture component states under rig_state.
    string_value(output, &["rig_state", "git_sha"])
        .or_else(|| string_value(output, &["rig_state", "head_sha"]))
        .or_else(|| {
            value_at(output, &["rig_state", "components"])
                .and_then(Value::as_array)
                .and_then(|components| components.first())
                .and_then(|component| {
                    string_value(component, &["git_sha"])
                        .or_else(|| string_value(component, &["head_sha"]))
                })
        })
        .map(str::to_string)
}

fn scenario_ids(output: &Value) -> Vec<String> {
    value_at(output, &["results", "scenarios"])
        .and_then(Value::as_array)
        .map(|scenarios| {
            scenarios
                .iter()
                .filter_map(|scenario| string_value(scenario, &["id"]).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A small, fixed set of headline metrics surfaced per scenario. Bench
/// scenarios emit dozens of metrics; the compact summary shows the few that
/// matter at a glance and points the operator at the full JSON for the rest.
const KEY_METRICS: &[&str] = &["p50_ms", "p95_ms", "mean_ms", "ops_per_sec"];

fn key_metric_lines(output: &Value) -> Vec<String> {
    let Some(scenarios) = value_at(output, &["results", "scenarios"]).and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    for scenario in scenarios {
        let Some(id) = string_value(scenario, &["id"]) else {
            continue;
        };
        let Some(metrics) = value_at(scenario, &["metrics"]).and_then(Value::as_object) else {
            continue;
        };
        let mut parts = Vec::new();
        for key in KEY_METRICS {
            if let Some(value) = metrics.get(*key).and_then(Value::as_f64) {
                parts.push(format!("{key}={}", format_metric(value)));
            }
        }
        if !parts.is_empty() {
            lines.push(format!("  {id}: {}", parts.join(" ")));
        }
    }
    if !lines.is_empty() {
        lines.insert(0, "Key metrics:".to_string());
    }
    lines
}

fn gate_failure_lines(output: &Value) -> Vec<String> {
    let failures = value_at(output, &["gate_failures"])
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if failures.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("Gate failures ({}):", failures.len())];
    for failure in failures {
        lines.push(format!("  {failure}"));
    }
    lines
}

/// Surface generic baseline/regression threshold checks when a bench payload
/// includes them. The bench producers are intentionally extensible, so this
/// reader accepts a few plain metadata shapes and ignores anything incomplete.
pub(crate) fn bench_regression_threshold_lines(output: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for check in regression_threshold_checks(output) {
        let Some(metric) = first_string(check, &["metric", "metric_name", "name"]) else {
            continue;
        };
        let scenario = first_string(check, &["scenario", "scenario_id", "id"]);
        let current = first_display_value(check, &["current", "current_value", "actual", "value"]);
        let baseline = first_display_value(
            check,
            &[
                "baseline",
                "baseline_value",
                "expected",
                "reference",
                "previous",
            ],
        );
        let threshold = first_display_value(
            check,
            &[
                "threshold",
                "threshold_value",
                "tolerance",
                "regression_threshold",
            ],
        );
        if current.is_none() && baseline.is_none() && threshold.is_none() {
            continue;
        }

        let mut parts = Vec::new();
        if let Some(scenario) = scenario {
            parts.push(scenario.to_string());
        }
        parts.push(metric.to_string());
        if let Some(current) = current {
            parts.push(format!("current={current}"));
        }
        if let Some(baseline) = baseline {
            parts.push(format!("baseline={baseline}"));
        }
        if let Some(threshold) = threshold {
            parts.push(format!("threshold={threshold}"));
        }
        if let Some(status) = threshold_status(check) {
            parts.push(status.to_string());
        }
        lines.push(format!("  {}", parts.join(" ")));
    }
    if !lines.is_empty() {
        lines.insert(0, "Regression thresholds:".to_string());
    }
    lines
}

fn regression_threshold_checks(output: &Value) -> Vec<&Value> {
    let mut checks = Vec::new();
    for base_path in [
        &[][..],
        &["metadata"][..],
        &["results", "metadata"][..],
        &["results", "run_metadata"][..],
    ] {
        let Some(base) = value_at(output, base_path) else {
            continue;
        };
        collect_threshold_checks(base, &mut checks);
    }
    checks
}

fn collect_threshold_checks<'a>(value: &'a Value, checks: &mut Vec<&'a Value>) {
    for key in [
        "regression_thresholds",
        "regression_threshold_checks",
        "thresholds",
        "threshold_checks",
        "baseline_thresholds",
        "baseline_regressions",
        "regression_checks",
    ] {
        let Some(candidate) = value.get(key) else {
            continue;
        };
        match candidate {
            Value::Array(items) => checks.extend(items.iter().filter(|item| item.is_object())),
            Value::Object(_) => {
                if let Some(items) = candidate.get("checks").and_then(Value::as_array) {
                    checks.extend(items.iter().filter(|item| item.is_object()));
                } else {
                    checks.push(candidate);
                }
            }
            _ => {}
        }
    }
}

fn first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| string_value(value, &[*key]))
}

fn first_display_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| display_value(value.get(*key)?))
}

fn display_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.to_string()),
        Value::Number(value) => value.as_f64().map(format_metric),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn threshold_status(check: &Value) -> Option<&'static str> {
    if let Some(passed) = check.get("passed").and_then(Value::as_bool) {
        return Some(pass_fail(passed));
    }
    match first_string(check, &["status", "result", "outcome"])? {
        "pass" | "passed" | "ok" | "success" => Some("PASS"),
        "fail" | "failed" | "regression" | "error" => Some("FAIL"),
        _ => None,
    }
}

/// Surface artifact pointers (#3260): shared-state files, runner bundle
/// paths, and scenario-specific artifacts. Each ref carries a path or a URL;
/// we print whichever locates it, prefixed by scenario + name so it is
/// grep-friendly.
fn artifact_lines(output: &Value) -> Vec<String> {
    let Some(artifacts) = value_at(output, &["artifacts"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    if artifacts.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![format!("Artifacts ({}):", artifacts.len())];
    for artifact in artifacts {
        let scenario = string_value(artifact, &["scenario_id"]).unwrap_or("");
        let name = string_value(artifact, &["name"]).unwrap_or("artifact");
        let locator = artifact_locator(artifact);
        let label = if scenario.is_empty() {
            name.to_string()
        } else {
            format!("{scenario}/{name}")
        };
        match locator {
            Some(locator) => lines.push(format!("  {label}: {locator}")),
            None => lines.push(format!("  {label}")),
        }
    }
    lines
}

fn key_artifact_lines(output: &Value, run_id: Option<&str>) -> Vec<String> {
    value_at(output, &["artifacts"])
        .and_then(Value::as_array)
        .map(|artifacts| super::key_artifacts::key_artifact_lines(artifacts, run_id, false))
        .unwrap_or_default()
}

/// Best on-disk / network locator for an artifact ref, in preference order.
/// Paths come first because shared-state and bundle artifacts are local
/// files the operator wants to open directly.
fn artifact_locator(artifact: &Value) -> Option<String> {
    super::key_artifacts::artifact_locator(artifact).map(str::to_string)
}

fn failure_lines(output: &Value) -> Vec<String> {
    let Some(failure) = value_at(output, &["failure"]) else {
        return Vec::new();
    };
    let message = string_value(failure, &["message"])
        .or_else(|| string_value(failure, &["summary"]))
        .or_else(|| failure.as_str());
    match message {
        Some(message) => vec![format!("Failure: {message}")],
        None => Vec::new(),
    }
}

fn hint_lines(output: &Value) -> Vec<String> {
    value_at(output, &["hints"])
        .and_then(Value::as_array)
        .map(|hints| {
            hints
                .iter()
                .filter_map(Value::as_str)
                .map(|hint| format!("Hint: {hint}"))
                .collect()
        })
        .unwrap_or_default()
}

/// Always point the operator at the full evidence. When a persisted run id
/// is available, `homeboy runs show <id>` is the canonical "see more"
/// command; otherwise suggest re-running with `--json` for the full payload.
fn next_command_lines(output: &Value, run_id: Option<String>) -> Vec<String> {
    if let Some(run_id) = run_id {
        return vec![
            format!("Inspect: homeboy runs show {run_id}"),
            format!("Artifacts: homeboy runs artifacts {run_id}"),
        ];
    }
    // No persisted run id (e.g. failed before persistence): still surface
    // artifacts inline above; tell the user how to get the full JSON.
    let _ = output;
    vec!["Full output: re-run with --json".to_string()]
}

fn comparison_rig_lines(output: &Value) -> Vec<String> {
    let rigs = value_at(output, &["rigs"])
        .and_then(Value::as_array)
        .or_else(|| value_at(output, &["summary", "rigs"]).and_then(Value::as_array));
    let Some(rigs) = rigs else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for rig in rigs {
        let rig_id = string_value(rig, &["rig_id"]).unwrap_or("<rig>");
        let passed = rig.get("passed").and_then(Value::as_bool).unwrap_or(false);
        lines.push(format!("  {rig_id}: {}", pass_fail(passed)));
    }
    if !lines.is_empty() {
        lines.insert(0, "Rigs:".to_string());
    }
    lines
}

fn comparison_artifact_lines(output: &Value) -> Vec<String> {
    // Comparison output nests artifacts under each rig entry.
    let Some(rigs) = value_at(output, &["rigs"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for rig in rigs {
        let rig_id = string_value(rig, &["rig_id"]).unwrap_or("<rig>");
        let Some(artifacts) = value_at(rig, &["artifacts"]).and_then(Value::as_array) else {
            continue;
        };
        for artifact in artifacts {
            let name = string_value(artifact, &["name"]).unwrap_or("artifact");
            if let Some(locator) = artifact_locator(artifact) {
                lines.push(format!("  {rig_id}/{name}: {locator}"));
            }
        }
    }
    if !lines.is_empty() {
        lines.insert(0, "Artifacts:".to_string());
    }
    lines
}

// --- formatting helpers ------------------------------------------------

fn pass_fail(passed: bool) -> &'static str {
    if passed {
        "PASS"
    } else {
        "FAIL"
    }
}

/// Format a metric value without noisy trailing zeros, keeping up to three
/// decimal places for sub-integer values.
fn format_metric(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        let rendered = format!("{value:.3}");
        rendered
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn finish(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn single_payload(inner: Value) -> Value {
        json!({ "variant": "single", "payload": inner })
    }

    #[test]
    fn non_summarized_variant_returns_none() {
        let payload = json!({ "variant": "list", "payload": { "count": 0 } });
        assert!(render_bench_summary(&payload).is_none());

        let observation = json!({ "variant": "observation", "payload": {} });
        assert!(render_bench_summary(&observation).is_none());
    }

    #[test]
    fn single_summary_leads_with_result_and_surfaces_run_and_artifacts() {
        let payload = single_payload(json!({
            "passed": true,
            "status": "passed",
            "component": "homeboy",
            "exit_code": 0,
            "iterations": 10,
            "artifacts": [
                {
                    "scenario_id": "rtc-smoke",
                    "name": "response-rows",
                    "path": "/tmp/shared/response-rows.json"
                },
                {
                    "scenario_id": "rtc-smoke",
                    "name": "bundle",
                    "url": "https://runner.test/bundle.zip"
                }
            ],
            "persisted_run": {
                "run_id": "bench-run-42",
                "component_id": "homeboy",
                "show_command": "homeboy runs show bench-run-42",
                "artifacts_command": "homeboy runs artifacts bench-run-42"
            },
            "results": {
                "component_id": "homeboy",
                "iterations": 10,
                "run_metadata": {
                    "started_at": "2026-06-19T00:00:00Z",
                    "iterations": 10,
                    "runs": 1,
                    "concurrency": 1,
                    "runner": { "extension": "sample-runner", "path": "/runner" }
                },
                "scenarios": [
                    {
                        "id": "rtc-smoke",
                        "iterations": 10,
                        "metrics": { "p50_ms": 12.5, "p95_ms": 30.0, "noise": 999.0 }
                    }
                ]
            }
        }));

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.starts_with("Bench run\nResult: PASS\n"));
        assert!(summary.contains("Component: homeboy\n"));
        assert!(summary.contains("Iterations: 10\n"));
        assert!(summary.contains("Run: bench-run-42\n"));
        assert!(summary.contains("Runner: sample-runner\n"));
        assert!(summary.contains("Scenarios (1): rtc-smoke\n"));
        assert!(summary.contains("Key metrics:\n"));
        assert!(summary.contains("p50_ms=12.5"));
        assert!(summary.contains("p95_ms=30"));
        assert!(summary.contains("Hotspots:\n"));
        assert!(summary.contains("  Slowest timing metrics:\n"));
        assert!(summary.contains("    rtc-smoke p95_ms=30\n"));
        // Surface artifact pointers (#3260).
        assert!(summary.contains("Artifacts (2):\n"));
        assert!(summary.contains("rtc-smoke/response-rows: /tmp/shared/response-rows.json\n"));
        assert!(summary.contains("rtc-smoke/bundle: https://runner.test/bundle.zip\n"));
        // Point at the persisted run for full evidence.
        assert!(summary.contains("Inspect: homeboy runs show bench-run-42\n"));
        assert!(summary.contains("Artifacts: homeboy runs artifacts bench-run-42\n"));
        // Compact: never dumps raw JSON braces.
        assert!(!summary.contains("{\n"));
    }

    #[test]
    fn single_summary_reports_failure_and_gate_failures() {
        let payload = single_payload(json!({
            "passed": false,
            "status": "failed",
            "component": "homeboy",
            "exit_code": 1,
            "iterations": 5,
            "gate_failures": ["p95_ms regressed by 40%"],
            "failure": { "message": "scenario rtc-smoke exceeded budget" }
        }));

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.contains("Result: FAIL\n"));
        assert!(summary.contains("Status: failed\n"));
        assert!(summary.contains("Gate failures (1):\n"));
        assert!(summary.contains("  p95_ms regressed by 40%\n"));
        assert!(summary.contains("Failure: scenario rtc-smoke exceeded budget\n"));
        // No persisted run id: suggest --json for full output.
        assert!(summary.contains("Full output: re-run with --json\n"));
    }

    #[test]
    fn single_summary_surfaces_key_artifacts_before_full_artifact_list() {
        let payload = single_payload(json!({
            "passed": true,
            "status": "passed",
            "component": "homeboy",
            "iterations": 1,
            "artifacts": [
                {
                    "scenario_id": "scenario-a",
                    "name": "route_inventory",
                    "observation_artifact_id": "artifact-route-inventory",
                    "path": "/tmp/route-inventory.json"
                },
                {
                    "scenario_id": "scenario-a",
                    "name": "transcript",
                    "path": "/tmp/transcript.txt"
                }
            ],
            "persisted_run": {
                "run_id": "bench-run-42"
            }
        }));

        let summary = render_bench_summary(&payload).expect("summary");
        let key_index = summary.find("Key artifacts:\n").expect("key artifacts");
        let artifact_index = summary.find("Artifacts (2):\n").expect("artifacts");

        assert!(key_index < artifact_index);
        assert!(summary.contains("  scenario-a/route_inventory: /tmp/route-inventory.json\n"));
        assert!(summary.contains(
            "    get: homeboy runs artifact get bench-run-42 artifact-route-inventory -o <path>\n"
        ));
        assert!(!summary.contains("Key artifacts:\n  scenario-a/transcript"));
    }

    #[test]
    fn hotspot_lines_rank_timing_metrics_and_metric_families() {
        let payload = json!({
            "scenario_metrics": [
                {
                    "scenario_id": "fast-path",
                    "metrics": {
                        "create_ms_per_item": 125.0,
                        "create_queries_per_item": 9.0,
                        "rows_count": 3.0
                    },
                    "metric_groups": {
                        "query_families": {
                            "select_count": 14.0,
                            "insert_count": 2.0
                        }
                    }
                },
                {
                    "scenario_id": "slow-path",
                    "metrics": {
                        "create_ms_per_item": 980.0,
                        "create_queries_per_item": 27.0,
                        "validation_ms": 40.0
                    },
                    "metric_groups": {
                        "query_families": {
                            "select_count": 44.0,
                            "insert_count": 7.0
                        }
                    }
                }
            ]
        });

        let lines = bench_hotspot_lines(&payload).join("\n");

        assert!(lines.starts_with("Hotspots:\n"));
        assert!(lines.contains("  Slowest timing metrics:\n"));
        assert!(lines.contains("    slow-path create_ms_per_item=980\n"));
        assert!(lines.contains("    fast-path create_ms_per_item=125\n"));
        assert!(lines.contains("  Hottest metric families:\n"));
        assert!(lines.contains("    query_families total=67 metrics=4"));
        assert!(lines.contains("    create total=36 metrics=2"));
    }

    #[test]
    fn hotspot_lines_annotate_failed_http_coverage_metrics() {
        let payload = json!({
            "scenario_metrics": [
                {
                    "scenario_id": "admin-page-coverage",
                    "metrics": {
                        "duration_ms": 32210.0,
                        "success_rate": 0.0,
                        "http_error_count": 62.0,
                        "request_error_count": 3.0,
                        "status_counts": {
                            "500": 47,
                            "403": 15,
                            "200": 9
                        }
                    }
                },
                {
                    "scenario_id": "normal-slow-path",
                    "metrics": {
                        "duration_ms": 1500.0,
                        "success_rate": 1.0
                    }
                }
            ],
            "artifacts": [
                {
                    "scenario_id": "admin-page-coverage",
                    "name": "fatal-log",
                    "fatal_signature": "PHP Fatal error: sample"
                }
            ]
        });

        let lines = bench_hotspot_lines(&payload).join("\n");

        assert!(lines.contains(
            "admin-page-coverage duration_ms=32210 [failed: success_rate=0 http_errors=62 request_errors=3 statuses=403:15,500:47 fatal=PHP Fatal error: sample]"
        ));
        assert!(lines.contains("normal-slow-path duration_ms=1500\n"));
        assert!(lines.contains("  Failure context:\n"));
        assert!(lines.contains(
            "    admin-page-coverage: success_rate=0 http_errors=62 request_errors=3 statuses=403:15,500:47 fatal=PHP Fatal error: sample"
        ));
    }

    #[test]
    fn single_summary_surfaces_schema_blind_coverage_metadata() {
        let payload = single_payload(json!({
            "passed": true,
            "status": "passed",
            "component": "homeboy",
            "iterations": 1,
            "results": {
                "coverage_summary": {
                    "surface_count": 120,
                    "exercised_count": 82,
                    "skipped_count": 14,
                    "failed_count": 3,
                    "coverage_gaps": [
                        { "group": "runner" },
                        { "group": "runner" },
                        { "group": "report" }
                    ]
                }
            }
        }));

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.contains("Coverage:\n"));
        assert!(summary
            .contains("  Surfaces: discovered=120 exercised=82 skipped_unsafe=14 failed=3\n"));
        assert!(summary.contains("  Coverage gaps: 3\n"));
        assert!(summary.contains("  Top uncovered groups:\n"));
        assert!(summary.contains("    runner: 2\n"));
        assert!(summary.contains("    report: 1\n"));
    }

    #[test]
    fn single_summary_surfaces_generic_regression_threshold_metadata() {
        let payload = single_payload(json!({
            "passed": false,
            "status": "failed",
            "component": "sample-component",
            "iterations": 3,
            "metadata": {
                "regression_thresholds": [
                    {
                        "scenario_id": "scenario-alpha",
                        "metric": "duration_ms",
                        "current": 130.25,
                        "baseline": 100.0,
                        "threshold": "20%",
                        "passed": false
                    },
                    {
                        "scenario": "scenario-beta",
                        "metric_name": "throughput",
                        "actual": 45.0,
                        "reference": 40.0,
                        "tolerance": 10.0,
                        "status": "passed"
                    }
                ]
            }
        }));

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.contains("Regression thresholds:\n"));
        assert!(summary.contains(
            "  scenario-alpha duration_ms current=130.25 baseline=100 threshold=20% FAIL\n"
        ));
        assert!(summary
            .contains("  scenario-beta throughput current=45 baseline=40 threshold=10 PASS\n"));
    }

    #[test]
    fn comparison_summary_surfaces_generic_nested_threshold_checks() {
        let payload = json!({
            "variant": "comparison",
            "payload": {
                "passed": false,
                "component": "sample-component",
                "iterations": 3,
                "results": {
                    "run_metadata": {
                        "threshold_checks": {
                            "checks": [
                                {
                                    "id": "scenario-gamma",
                                    "name": "latency_ms",
                                    "value": 88.0,
                                    "baseline_value": 70.0,
                                    "regression_threshold": 15.0,
                                    "outcome": "regression"
                                }
                            ]
                        }
                    }
                }
            }
        });

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.contains("Regression thresholds:\n"));
        assert!(summary
            .contains("  scenario-gamma latency_ms current=88 baseline=70 threshold=15 FAIL\n"));
    }

    #[test]
    fn coverage_lines_use_artifact_metadata_and_degrade_gracefully() {
        let payload = json!({
            "artifacts": [
                {
                    "name": "coverage",
                    "coverage_summary": {
                        "surface_count": 9,
                        "exercised_count": 5,
                        "top_uncovered_groups": [
                            { "name": "scheduler", "uncovered_count": 3 },
                            "runner"
                        ]
                    }
                }
            ]
        });

        let lines = bench_coverage_lines(&payload).join("\n");

        assert!(lines.starts_with("Coverage:\n"));
        assert!(lines.contains("  Surfaces: discovered=9 exercised=5"));
        assert!(lines.contains("    scheduler: 3"));
        assert!(lines.contains("    runner"));
        assert!(bench_coverage_lines(&json!({ "coverage_summary": {} })).is_empty());
    }

    #[test]
    fn comparison_summary_rolls_up_rigs_and_artifacts() {
        let payload = json!({
            "variant": "comparison",
            "payload": {
                "passed": false,
                "component": "homeboy",
                "iterations": 10,
                "regressions": 2,
                "rigs": [
                    {
                        "rig_id": "baseline",
                        "passed": true,
                        "artifacts": [
                            { "name": "rows", "path": "/tmp/baseline-rows.json" }
                        ]
                    },
                    {
                        "rig_id": "candidate",
                        "passed": false,
                        "artifacts": [
                            { "name": "rows", "path": "/tmp/candidate-rows.json" }
                        ]
                    }
                ]
            }
        });

        let summary = render_bench_summary(&payload).expect("summary");

        assert!(summary.starts_with("Bench comparison\nResult: FAIL\n"));
        assert!(summary.contains("Rigs:\n"));
        assert!(summary.contains("  baseline: PASS\n"));
        assert!(summary.contains("  candidate: FAIL\n"));
        assert!(summary.contains("Regressions/failures: 2\n"));
        assert!(summary.contains("Artifacts:\n"));
        assert!(summary.contains("  baseline/rows: /tmp/baseline-rows.json\n"));
        assert!(summary.contains("  candidate/rows: /tmp/candidate-rows.json\n"));
    }

    #[test]
    fn format_metric_trims_trailing_zeros() {
        assert_eq!(format_metric(30.0), "30");
        assert_eq!(format_metric(12.5), "12.5");
        assert_eq!(format_metric(12.500), "12.5");
        assert_eq!(format_metric(0.125), "0.125");
    }
}
