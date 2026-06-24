//! Compact human-readable summary for `homeboy runs show`.
//!
//! `runs show` returns a `RunDetail` that embeds full run metadata and the
//! complete artifact list. For bench runs in particular, the useful evidence
//! — shared-state files, runner artifact bundles, scenario-specific
//! artifacts — is buried in a large JSON payload (#3260).
//!
//! This module renders a compact summary from the serialized `RunsOutput`
//! value, surfacing run identity, status, and (prominently) each artifact's
//! locator plus a concise `homeboy runs artifact get ...` command to inspect
//! it. The full JSON remains available via `runs show <id> --json` and is
//! always written to `--output <file>` unchanged.

use serde_json::Value;

use super::summary_json::{string_value, value_at};

/// Render a compact summary for a serialized `RunsOutput` value. Returns
/// `None` for any variant other than `show`, leaving other `runs`
/// subcommands with their existing full-JSON presentation.
pub(crate) fn render_runs_show_summary(payload: &Value) -> Option<String> {
    if payload.get("variant").and_then(Value::as_str)? != "show" {
        return None;
    }
    let run = value_at(payload, &["payload", "run"])?;
    Some(render_run_detail(run))
}

fn render_run_detail(run: &Value) -> String {
    let run_id = string_value(run, &["id"]).unwrap_or("<unknown>");
    let kind = string_value(run, &["kind"]).unwrap_or("run");
    let status = string_value(run, &["status"]).unwrap_or("unknown");

    let mut lines = vec![
        format!("Run {run_id} ({kind})"),
        format!("Status: {status}"),
    ];

    if let Some(component) = string_value(run, &["component_id"]) {
        lines.push(format!("Component: {component}"));
    }
    if let Some(rig) = string_value(run, &["rig_id"]) {
        lines.push(format!("Rig: {rig}"));
    }
    if let Some(sha) = string_value(run, &["git_sha"]) {
        lines.push(format!("Component SHA: {sha}"));
    }
    if let Some(started) = string_value(run, &["started_at"]) {
        lines.push(format!("Started: {started}"));
    }
    if let Some(finished) = string_value(run, &["finished_at"]) {
        lines.push(format!("Finished: {finished}"));
    }

    if kind == "bench" {
        lines.extend(super::bench_summary::bench_hotspot_lines(run));
        lines.extend(super::bench_summary::bench_regression_threshold_lines(run));
    } else if kind == "fuzz" {
        lines.extend(super::runs::fuzz_hotspot_lines(run));
    }
    lines.extend(super::bench_summary::bench_coverage_lines(run));
    lines.extend(key_artifact_lines(run, run_id));
    lines.extend(artifact_lines(run, run_id));
    lines.extend(report_followup_lines(run, run_id, kind));
    lines.push(format!("Full output: homeboy runs show {run_id} --json"));

    finish(lines)
}

fn report_followup_lines(run: &Value, run_id: &str, kind: &str) -> Vec<String> {
    if kind != "bench" {
        return Vec::new();
    }

    let Some(component) = string_value(run, &["component_id"]) else {
        return Vec::new();
    };

    let mut filter = format!("--kind bench --component {component}");
    if let Some(rig) = string_value(run, &["rig_id"]) {
        filter.push_str(&format!(" --rig {rig}"));
    }
    if let Some(scenario) = first_bench_scenario(run) {
        filter.push_str(&format!(" --scenario {scenario}"));
    }

    vec![
        "Reports:".to_string(),
        format!("  history: homeboy runs list {filter}"),
        format!("  distribution: homeboy runs distribution {filter} --field <metadata.path>"),
        format!(
            "  compare: homeboy runs bench-compare --from-run <other-run-id> --to-run {run_id}"
        ),
    ]
}

fn first_bench_scenario(run: &Value) -> Option<&str> {
    value_at(run, &["metadata", "scenario_metrics"])
        .and_then(Value::as_array)
        .and_then(|scenarios| scenarios.first())
        .and_then(|scenario| string_value(scenario, &["scenario_id"]))
}

/// Surface every recorded artifact with its best on-disk / network locator
/// and a concise command to fetch it (#3260). Local file paths are shown
/// directly; otherwise the public/viewer URL is shown.
fn artifact_lines(run: &Value, run_id: &str) -> Vec<String> {
    let Some(artifacts) = value_at(run, &["artifacts"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    if artifacts.is_empty() {
        return vec!["Artifacts: none recorded".to_string()];
    }

    let mut lines = vec![format!("Artifacts ({}):", artifacts.len())];
    for artifact in artifacts {
        let id = string_value(artifact, &["id"]).unwrap_or("artifact");
        let kind = string_value(artifact, &["kind"]).unwrap_or("");
        let label = if kind.is_empty() {
            id.to_string()
        } else {
            format!("{id} [{kind}]")
        };
        match artifact_locator(artifact) {
            Some(locator) => lines.push(format!("  {label}: {locator}")),
            None => lines.push(format!("  {label}")),
        }
        // Only file artifacts are fetchable via `runs artifact get`.
        if string_value(artifact, &["type"]) == Some("file") {
            lines.push(format!(
                "    get: homeboy runs artifact get {run_id} {id} -o <path>"
            ));
        }
    }
    lines
}

fn key_artifact_lines(run: &Value, run_id: &str) -> Vec<String> {
    value_at(run, &["artifacts"])
        .and_then(Value::as_array)
        .map(|artifacts| super::key_artifacts::key_artifact_lines(artifacts, Some(run_id), true))
        .unwrap_or_default()
}

fn artifact_locator(artifact: &Value) -> Option<String> {
    super::key_artifacts::artifact_locator(artifact).map(str::to_string)
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

    #[test]
    fn non_show_variant_returns_none() {
        let payload = json!({ "variant": "list", "payload": { "runs": [] } });
        assert!(render_runs_show_summary(&payload).is_none());
    }

    #[test]
    fn show_summary_surfaces_identity_and_artifact_pointers() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "started_at": "2026-06-19T00:00:00Z",
                    "finished_at": "2026-06-19T00:01:00Z",
                    "component_id": "homeboy",
                    "rig_id": "rtc",
                    "git_sha": "abcdef1234",
                    "homeboy_version": "0.232.0",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "bench_artifact",
                            "run_id": "bench-run-42",
                            "kind": "bench_artifact",
                            "type": "file",
                            "path": "/var/lib/homeboy/runs/bench-run-42/response-rows.json",
                            "created_at": "2026-06-19T00:01:00Z"
                        },
                        {
                            "id": "admin_url",
                            "run_id": "bench-run-42",
                            "kind": "admin_url",
                            "type": "url",
                            "path": "",
                            "url": "https://example.test/wp-admin/",
                            "created_at": "2026-06-19T00:01:00Z"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.starts_with("Run bench-run-42 (bench)\nStatus: pass\n"));
        assert!(summary.contains("Component: homeboy\n"));
        assert!(summary.contains("Rig: rtc\n"));
        assert!(summary.contains("Component SHA: abcdef1234\n"));
        assert!(summary.contains("Artifacts (2):\n"));
        assert!(summary.contains(
            "  bench_artifact [bench_artifact]: /var/lib/homeboy/runs/bench-run-42/response-rows.json\n"
        ));
        assert!(summary.contains(
            "    get: homeboy runs artifact get bench-run-42 bench_artifact -o <path>\n"
        ));
        assert!(summary.contains("  admin_url [admin_url]: https://example.test/wp-admin/\n"));
        assert!(summary.contains("Reports:\n"));
        assert!(summary
            .contains("  history: homeboy runs list --kind bench --component homeboy --rig rtc\n"));
        assert!(summary.contains(
            "  distribution: homeboy runs distribution --kind bench --component homeboy --rig rtc --field <metadata.path>\n"
        ));
        assert!(summary.contains(
            "  compare: homeboy runs bench-compare --from-run <other-run-id> --to-run bench-run-42\n"
        ));
        assert!(summary.contains("Full output: homeboy runs show bench-run-42 --json\n"));
        // URL artifacts are not fetchable via `runs artifact get`.
        assert!(!summary.contains("get: homeboy runs artifact get bench-run-42 admin_url"));
        // Compact: no raw JSON braces.
        assert!(!summary.contains("{\n"));
    }

    #[test]
    fn bench_show_summary_surfaces_hotspots_from_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "scenario_metrics": [
                            {
                                "scenario_id": "scenario-a",
                                "metrics": {
                                    "work_ms_per_item": 80.0,
                                    "work_queries_per_item": 11.0
                                }
                            },
                            {
                                "scenario_id": "scenario-b",
                                "metrics": {
                                    "work_ms_per_item": 240.0,
                                    "work_queries_per_item": 23.0
                                }
                            }
                        ]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Hotspots:\n"));
        assert!(summary.contains("  Slowest timing metrics:\n"));
        assert!(summary.contains("    scenario-b work_ms_per_item=240\n"));
        assert!(summary.contains("  Hottest metric families:\n"));
        assert!(summary.contains("    work total=34 metrics=2\n"));
        assert!(summary.contains("Artifacts: none recorded\n"));
    }

    #[test]
    fn bench_show_summary_marks_failed_hotspots_from_run_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "scenario_metrics": [
                            {
                                "scenario_id": "admin-page-coverage",
                                "metrics": {
                                    "duration_ms": 42000.0,
                                    "success_rate": 0.0,
                                    "http_error_count": 62.0,
                                    "status_counts": {
                                        "500": 47,
                                        "403": 15
                                    }
                                }
                            }
                        ]
                    },
                    "artifacts": [
                        {
                            "id": "fatal-log",
                            "run_id": "bench-run-42",
                            "scenario_id": "admin-page-coverage",
                            "kind": "log",
                            "type": "file",
                            "path": "/tmp/fatal.log",
                            "fatal_signatures": ["PHP Fatal error: sample"]
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains(
            "admin-page-coverage duration_ms=42000 [failed: success_rate=0 http_errors=62 statuses=403:15,500:47 fatal=PHP Fatal error: sample]\n"
        ));
        assert!(summary.contains("  Failure context:\n"));
        assert!(summary.contains(
            "    admin-page-coverage: success_rate=0 http_errors=62 statuses=403:15,500:47 fatal=PHP Fatal error: sample\n"
        ));
    }

    #[test]
    fn bench_show_summary_surfaces_coverage_from_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "coverage_summary": {
                            "surface_count": 44,
                            "exercised_count": 30,
                            "skipped_count": 8,
                            "failed_count": 1,
                            "coverage_gaps": [
                                "api::create",
                                "api::delete",
                                "cli::delete"
                            ]
                        }
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Coverage:\n"));
        assert!(
            summary.contains("  Surfaces: discovered=44 exercised=30 skipped_unsafe=8 failed=1\n")
        );
        assert!(summary.contains("  Coverage gaps: 3\n"));
        assert!(summary.contains("    api: 2\n"));
        assert!(summary.contains("    cli: 1\n"));
    }

    #[test]
    fn fuzz_show_summary_surfaces_generic_coverage_and_case_artifacts() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "fuzz-run-7",
                    "kind": "fuzz",
                    "status": "fail",
                    "metadata": {
                        "coverage_summary": {
                            "declared_count": 12,
                            "executable_count": 10,
                            "proven_count": 9,
                            "surface_count": 12,
                            "operation_count": 18,
                            "exercised_count": 9,
                            "failed_count": 2,
                            "skipped_reason_counts": {
                                "requires_confirmation": 2,
                                "missing_fixture": 1
                            },
                            "coverage_gaps": [
                                "parser::unicode",
                                "parser::empty",
                                "serializer::nested"
                            ]
                        }
                    },
                    "artifacts": [
                        {
                            "id": "seed-1",
                            "run_id": "fuzz-run-7",
                            "kind": "failing_case",
                            "type": "file",
                            "path": "/tmp/fuzz/failing-case.json"
                        },
                        {
                            "id": "repro-1",
                            "run_id": "fuzz-run-7",
                            "name": "repro-case",
                            "type": "file",
                            "path": "/tmp/fuzz/repro.txt"
                        },
                        {
                            "id": "coverage-report",
                            "run_id": "fuzz-run-7",
                            "kind": "coverage",
                            "type": "file",
                            "path": "/tmp/fuzz/coverage.json"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Run fuzz-run-7 (fuzz)\n"));
        assert!(summary.contains("Coverage:\n"));
        assert!(summary.contains("  Surfaces: discovered=12 exercised=9 failed=2\n"));
        assert!(summary.contains("  Proof states: declared=12 executable=10 proven=9\n"));
        assert!(summary.contains("  Operations: 18\n"));
        assert!(summary.contains("  Coverage gaps: 3\n"));
        assert!(summary.contains("  Skipped reasons:\n"));
        assert!(summary.contains("    requires_confirmation: 2\n"));
        assert!(summary.contains("    missing_fixture: 1\n"));
        assert!(summary.contains("    parser: 2\n"));
        assert!(summary.contains("    serializer: 1\n"));
        assert!(summary.contains("Key artifacts:\n"));
        assert!(summary.contains("  global/seed-1: /tmp/fuzz/failing-case.json\n"));
        assert!(summary.contains("  global/repro-case: /tmp/fuzz/repro.txt\n"));
        assert!(summary.contains("  global/coverage-report: /tmp/fuzz/coverage.json\n"));
        assert!(
            summary.contains("    get: homeboy runs artifact get fuzz-run-7 seed-1 -o <path>\n")
        );
        assert!(!summary.contains("Reports:\n"));
    }

    #[test]
    fn bench_show_summary_filters_followup_reports_by_scenario_when_available() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "component_id": "homeboy",
                    "metadata": {
                        "scenario_metrics": [{"scenario_id": "cold", "metrics": {"p95_ms": 42.0}}]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains(
            "  history: homeboy runs list --kind bench --component homeboy --scenario cold\n"
        ));
        assert!(summary.contains(
            "  distribution: homeboy runs distribution --kind bench --component homeboy --scenario cold --field <metadata.path>\n"
        ));
    }

    #[test]
    fn fuzz_show_summary_surfaces_generic_hotspots_from_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("fuzz-results.json");
        std::fs::write(
            &artifact_path,
            serde_json::json!({
                "schema": "homeboy/fuzz-campaign/v1",
                "id": "campaign-1",
                "hotspots": [
                    { "id": "parser::unicode", "score": 4.5, "label": "Unicode parser" },
                    { "id": "serializer::nested", "count": 2 }
                ]
            })
            .to_string(),
        )
        .expect("write artifact");
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "fuzz-run-7",
                    "kind": "fuzz",
                    "status": "fail",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "fuzz-results",
                            "run_id": "fuzz-run-7",
                            "kind": "fuzz_results",
                            "type": "file",
                            "path": artifact_path
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Hotspots:\n"));
        assert!(summary.contains("  Fuzz hotspots:\n"));
        assert!(summary
            .contains("    #1 parser::unicode (Unicode parser) score=4.5 occurrences=1 runs=1\n"));
        assert!(summary.contains("    #2 serializer::nested score=2 occurrences=1 runs=1\n"));
    }

    #[test]
    fn bench_show_summary_surfaces_regression_threshold_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "fail",
                    "metadata": {
                        "baseline_thresholds": [
                            {
                                "scenario_id": "generic-case",
                                "metric": "work_units",
                                "current_value": 60.0,
                                "baseline_value": 50.0,
                                "threshold_value": 5.0,
                                "passed": false
                            }
                        ]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Regression thresholds:\n"));
        assert!(
            summary.contains("  generic-case work_units current=60 baseline=50 threshold=5 FAIL\n")
        );
    }

    #[test]
    fn show_summary_surfaces_key_artifacts_before_full_artifact_list() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run-1",
                    "kind": "test",
                    "status": "pass",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "artifact-coverage",
                            "run_id": "run-1",
                            "scenario_id": "scenario-a",
                            "kind": "coverage",
                            "type": "file",
                            "path": "/tmp/coverage.json"
                        },
                        {
                            "id": "artifact-log",
                            "run_id": "run-1",
                            "scenario_id": "scenario-a",
                            "kind": "log",
                            "type": "file",
                            "path": "/tmp/log.txt"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        let key_index = summary.find("Key artifacts:\n").expect("key artifacts");
        let artifact_index = summary.find("Artifacts (2):\n").expect("artifacts");

        assert!(key_index < artifact_index);
        assert!(summary.contains("  scenario-a/artifact-coverage: /tmp/coverage.json\n"));
        assert!(summary
            .contains("    get: homeboy runs artifact get run-1 artifact-coverage -o <path>\n"));
        assert!(!summary.contains("Key artifacts:\n  scenario-a/artifact-log"));
    }

    #[test]
    fn show_summary_reports_no_artifacts() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run-1",
                    "kind": "test",
                    "status": "fail",
                    "started_at": "2026-06-19T00:00:00Z",
                    "finished_at": null,
                    "metadata": {},
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        assert!(summary.contains("Artifacts: none recorded\n"));
        assert!(summary.contains("Full output: homeboy runs show run-1 --json\n"));
    }
}
