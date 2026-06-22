use std::path::Path;

use homeboy::commands::report::{render_failure_digest_from_args, FailureDigestArgs};

#[path = "support/mod.rs"]
mod support;

use support::write_file;

const LINT_JSON: &str = include_str!("fixtures/failure_digest/lint.json");
const TEST_JSON: &str = include_str!("fixtures/failure_digest/test.json");
const AUDIT_JSON: &str = include_str!("fixtures/failure_digest/audit.json");
const TOOLING_JSON: &str = include_str!("fixtures/failure_digest/tooling.json");

fn tmp_dir(name: &str) -> tempfile::TempDir {
    support::temp_dir(&format!("failure-digest-{name}"))
}

fn render(dir: &Path, results: &str, autofix_enabled: bool, autofix_attempted: bool) -> String {
    render_failure_digest_from_args(&FailureDigestArgs {
        output_dir: dir.to_string_lossy().to_string(),
        results: results.to_string(),
        run_url: Some("https://github.com/Extra-Chill/homeboy/actions/runs/123".to_string()),
        tooling_json: None,
        commands: Some("audit,lint,test".to_string()),
        autofix_commands: None,
        autofix_enabled,
        autofix_attempted,
        format: "markdown".to_string(),
    })
    .expect("failure digest should render")
}

fn trace_json(status: &str, summary: &str) -> String {
    format!(
        r#"{{
            "success": {success},
            "data": {{
                "passed": {success},
                "status": "{status}",
                "component": "studio",
                "exit_code": {exit_code},
                "artifacts": [
                    {{"label":"main log","path":"artifacts/main.log","content":"raw log body that should never appear"}},
                    {{"label":"process tree","path":"artifacts/process-tree.txt"}}
                ],
                "results": {{
                    "component_id": "studio",
                    "scenario_id": "close-window-running-site",
                    "status": "{status}",
                    "summary": "{summary}",
                    "timeline": [],
                    "assertions": [],
                    "artifacts": [
                        {{"label":"main log","path":"artifacts/main.log","content":"raw log body that should never appear"}},
                        {{"label":"process tree","path":"artifacts/process-tree.txt"}}
                    ]
                }}
            }}
        }}"#,
        success = status == "pass",
        exit_code = if status == "pass" { 0 } else { 1 }
    )
}

fn lint_json_with_findings(count: usize) -> String {
    let findings = (1..=count)
        .map(|idx| {
            format!(
                r#"{{"tool":"eslint","rule":"no-debugger","severity":"error","file":"src/file{idx}.js","line":{idx},"message":"Unexpected debugger statement"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    format!(
        r#"{{
            "success": false,
            "data": {{
                "passed": false,
                "summary": "{count} lint finding(s)",
                "findings": [{findings}]
            }}
        }}"#
    )
}

fn trace_json_with_span_summaries() -> &'static str {
    r#"{
        "success": false,
        "data": {
            "passed": false,
            "status": "fail",
            "component": "studio",
            "exit_code": 1,
            "span_summaries": [
                {
                    "id": "phase.boot_to_ready",
                    "from": "runner.boot",
                    "to": "runner.ready",
                    "status": "ok",
                    "duration_ms": 125,
                    "from_t_ms": 10,
                    "to_t_ms": 135,
                    "metadata": {
                        "category": "startup",
                        "critical": true,
                        "blocking": true,
                        "cacheable": true
                    }
                },
                {
                    "id": "phase.ready_to_open",
                    "from": "runner.ready",
                    "to": "app.opened",
                    "status": "skipped",
                    "missing": ["app.opened"],
                    "message": "span endpoint missing from timeline",
                    "metadata": {
                        "category": "ui",
                        "prewarmable": true
                    }
                }
            ],
            "results": {
                "component_id": "studio",
                "scenario_id": "close-window-running-site",
                "status": "fail",
                "summary": "Window reopened after close.",
                "timeline": [],
                "assertions": [],
                "artifacts": []
            }
        }
    }"#
}

fn trace_json_with_toolchain() -> &'static str {
    r#"{
        "success": true,
        "data": {
            "passed": true,
            "status": "pass",
            "component": "studio",
            "exit_code": 0,
            "toolchain": {
                "canonical": false,
                "mode": "development",
                "reasons": ["HOMEBOY_SAMPLE_RUNTIME_BIN selected a local sample runtime path"],
                "homeboy": {"path":"/repo/homeboy","sha":"abc123","branch":"main","dirty":false},
                "toolchains": {
                    "Sample Runtime": {"path":"/repo/sample-runtime","sha":"def456","branch":"main","dirty":true}
                },
                "node": "v24.0.0"
            },
            "components": {
                "target": {"path":"/repo/studio","sha":"789abc","branch":"trunk","dirty":false},
                "dependencies": []
            },
            "results": {
                "component_id": "studio",
                "scenario_id": "close-window-running-site",
                "status": "pass",
                "summary": "Window stayed closed.",
                "timeline": [],
                "assertions": [],
                "artifacts": []
            }
        }
    }"#
}

fn trace_json_with_sample_runtime_partial_manifest() -> &'static str {
    r#"{
        "success": false,
        "data": {
            "passed": false,
            "status": "error",
            "component": "studio",
            "exit_code": 2,
            "runtime_diagnostics": {
                "manifest_path": "sample-runtime-artifacts/runtime-123/manifest.json",
                "runtime_metadata_path": "sample-runtime-artifacts/runtime-123/runtime.json",
                "commands_log_path": "sample-runtime-artifacts/runtime-123/commands.jsonl",
                "events_log_path": "sample-runtime-artifacts/runtime-123/events.jsonl",
                "browser_summary_path": "sample-runtime-artifacts/runtime-123/browser-summary.json",
                "current_phase": "browser.probe.wait_for_ready",
                "current_command": "browser probe /wp-admin/",
                "last_command": "runtime setup"
            },
            "failure": {
                "component_id": "studio",
                "scenario_id": "close-window-running-site",
                "exit_code": 2,
                "stderr_excerpt": "browser probe timed out before final artifacts"
            }
        }
    }"#
}

fn bench_json() -> &'static str {
    r#"{
        "success": true,
        "data": {
            "passed": true,
            "status": "passed",
            "component": "studio",
            "exit_code": 0,
            "iterations": 5,
            "budget_findings": [
                {
                    "tool": "budget",
                    "rule": "rest.max_response_bytes",
                    "category": "budget",
                    "severity": "error",
                    "message": "REST response exceeded 250 KB budget",
                    "fingerprint": "rest.max_response_bytes:/wp-json/sample/v1/items?per_page=100",
                    "source": { "kind": "budget", "label": "profile:wordpress-rest" },
                    "metadata": {
                        "code": "rest.max_response_bytes",
                        "context_label": "profile:wordpress-rest",
                        "actual": 4378195,
                        "expected": 250000,
                        "unit": "bytes",
                        "subject": "/wp-json/sample/v1/items?per_page=100",
                        "passed": false
                    },
                    "raw": { "category": "budget" }
                }
            ],
            "artifacts": [
                {
                    "scenario_id": "wp-admin-load",
                    "run_index": 0,
                    "name": "trace",
                    "path": "bench-artifacts/wp-admin-load/trace.zip",
                    "kind": "playwright-trace",
                    "label": "Playwright trace",
                    "content": "trace bytes should never appear"
                },
                {
                    "scenario_id": "wp-admin-load",
                    "name": "screenshot",
                    "path": "bench-artifacts/wp-admin-load/final.png",
                    "kind": "screenshot",
                    "label": "Final screenshot"
                }
            ],
            "results": {
                "component_id": "studio",
                "iterations": 5,
                "scenarios": []
            }
        }
    }"#
}

#[test]
fn renders_lint_failure_digest_from_fixture() {
    let guard = tmp_dir("lint");
    let dir = guard.path();
    write_file(dir, "lint.json", LINT_JSON);

    let markdown = render(dir, r#"{"lint":"fail"}"#, false, false);

    assert!(markdown.contains("## Failure Digest"));
    assert!(markdown.contains("### Lint Failure Digest"));
    assert!(markdown.contains("- Lint summary: **3 lint finding(s)**"));
    assert!(markdown.contains("- Actionable lint findings (3 shown):"));
    assert!(markdown.contains(
        "1. `src/widget.php:12` [error] phpcs/Squiz.Commenting.FunctionComment.Missing: Missing function doc comment"
    ));
    assert!(markdown.contains("- Autofix applied: **yes** (1 file(s) modified)"));
    assert!(markdown.contains("<details><summary>Top lint violations</summary>"));
    assert!(markdown
        .contains("- Full lint log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
    assert!(!markdown.contains("### Test Failure Digest"));
}

#[test]
fn caps_lint_findings_in_failure_digest() {
    let guard = tmp_dir("lint-cap");
    let dir = guard.path();
    write_file(dir, "lint.json", &lint_json_with_findings(12));

    let markdown = render(dir, r#"{"lint":"fail"}"#, false, false);

    assert!(markdown.contains("- Actionable lint findings (10 shown):"));
    assert!(markdown.contains(
        "10. `src/file10.js:10` [error] eslint/no-debugger: Unexpected debugger statement"
    ));
    assert!(markdown.contains(
        "- 2 more lint finding(s) omitted from this comment; see `lint.json` or the full lint log."
    ));
    assert!(!markdown.contains("src/file11.js"));
}

#[test]
fn renders_test_failure_digest_from_fixture() {
    let guard = tmp_dir("test");
    let dir = guard.path();
    write_file(dir, "test.json", TEST_JSON);

    let markdown = render(dir, r#"{"test":"fail"}"#, false, false);

    assert!(markdown.contains("### Test Failure Digest"));
    assert!(markdown.contains("- Failed tests: **2**"));
    assert!(markdown
        .contains("1. test_widget_renders — expected widget output — tests/widget_test.rs:42"));
    assert!(markdown.contains(
        "2. test_widget_handles_empty_state — empty state missing — tests/widget_test.rs"
    ));
}

#[test]
fn classifies_failures_as_branch_introduced_when_baseline_has_distinct_failures() {
    let guard = tmp_dir("origin-branch-introduced");
    let dir = guard.path();
    write_file(dir, "test.json", TEST_JSON);
    write_file(
        dir,
        "baseline-test.json",
        r#"{
            "success": false,
            "data": {
                "findings": [
                    {
                        "tool": "test",
                        "message": "existing unrelated failure",
                        "file": "tests/other_test.rs",
                        "metadata": {"test_name": "test_existing_failure"}
                    }
                ]
            }
        }"#,
    );

    let markdown = render(dir, r#"{"test":"fail"}"#, false, false);

    assert!(markdown.contains("### Failure origin classification"));
    assert!(markdown.contains("- `test`: **branch-introduced** (head: 2, baseline: 1, shared: 0)"));
}

#[test]
fn classifies_failures_as_baseline_present_when_all_head_failures_match_baseline() {
    let guard = tmp_dir("origin-baseline-present");
    let dir = guard.path();
    write_file(dir, "test.json", TEST_JSON);
    write_file(dir, "baseline-test.json", TEST_JSON);

    let markdown = render(dir, r#"{"test":"fail"}"#, false, false);

    assert!(markdown.contains("- `test`: **baseline-present** (head: 2, baseline: 2, shared: 2)"));
}

#[test]
fn reports_inconclusive_when_baseline_artifact_cannot_be_parsed() {
    let guard = tmp_dir("origin-baseline-parse-failed");
    let dir = guard.path();
    write_file(dir, "test.json", TEST_JSON);
    write_file(dir, "baseline-test.json", "not json");

    let markdown = render(dir, r#"{"test":"fail"}"#, false, false);

    assert!(markdown.contains("- `test`: **inconclusive** (baseline artifact `"));
    assert!(markdown.contains("failed to parse"));
}

#[test]
fn reports_baseline_red_with_command_and_artifact_evidence() {
    let guard = tmp_dir("origin-baseline-red");
    let dir = guard.path();
    write_file(dir, "lint.json", LINT_JSON);
    write_file(
        dir,
        "baseline-lint.json",
        r#"{
            "success": false,
            "error": {
                "code": "process.failed",
                "message": "FMT SUMMARY: 7 files need formatting",
                "details": {
                    "baseline_command": "cargo fmt --check",
                    "stderr_path": "baseline/lint.stderr.log"
                }
            },
            "data": {
                "artifacts": [
                    {"label":"format summary","path":"baseline/fmt-summary.txt"}
                ]
            }
        }"#,
    );

    let markdown = render(dir, r#"{"lint":"fail"}"#, false, false);

    assert!(markdown.contains("- `lint`: **baseline-red** (head: 3, baseline: 0, shared: 0)"));
    assert!(markdown.contains("  - Baseline command: `cargo fmt --check`"));
    assert!(markdown.contains("  - Baseline artifact: `"));
    assert!(markdown.contains("format summary `baseline/fmt-summary.txt`"));
    assert!(markdown.contains("stderr_path `baseline/lint.stderr.log`"));
}

#[test]
fn renders_audit_failure_digest_from_fixture() {
    let guard = tmp_dir("audit");
    let dir = guard.path();
    write_file(dir, "audit.json", AUDIT_JSON);

    let markdown = render(dir, r#"{"audit":"fail"}"#, false, false);

    assert!(markdown.contains("### Audit Failure Digest"));
    assert!(markdown.contains("- Alignment score: **0.812**"));
    assert!(markdown.contains("- Severity counts: **high: 1, low: 1, medium: 1**"));
    assert!(markdown.contains("- New findings since baseline: **1**"));
    assert!(markdown.contains("1. **src/report.rs** — new report module lacks tests (`abc123`)"));
    assert!(markdown.contains("**src/render.rs** — god_file — file is too large"));
}

#[test]
fn renders_mixed_failures_with_autofix_enabled_not_attempted() {
    let guard = tmp_dir("mixed-autofix");
    let dir = guard.path();
    write_file(dir, "lint.json", LINT_JSON);
    write_file(dir, "test.json", TEST_JSON);

    let markdown = render(
        dir,
        r#"{"lint":"fail","test":"fail","audit":"pass"}"#,
        true,
        false,
    );

    assert!(markdown.contains("### Lint Failure Digest"));
    assert!(markdown.contains("### Test Failure Digest"));
    assert!(markdown.contains("- Overall: **auto_fixable**"));
    assert!(markdown.contains("- Autofix enabled: **yes**"));
    assert!(markdown.contains("- Auto-fixable failed commands:"));
    assert!(markdown.contains("  - `lint`"));
    assert!(markdown.contains("  - `test`"));
}

#[test]
fn renders_mixed_failures_after_autofix_attempted_as_human_needed() {
    let guard = tmp_dir("attempted");
    let dir = guard.path();
    write_file(dir, "lint.json", LINT_JSON);

    let markdown = render(dir, r#"{"lint":"fail"}"#, true, true);

    assert!(markdown.contains("- Overall: **human_needed**"));
    assert!(markdown.contains("- Autofix attempted this run: **yes**"));
    assert!(markdown.contains("- Human-needed failed commands:"));
    assert!(markdown.contains("  - `lint`"));
    assert!(markdown.contains("- Failed commands with available automated fixes:"));
}

#[test]
fn missing_json_renders_explicit_structured_details_unavailable() {
    let guard = tmp_dir("missing");
    let dir = guard.path();

    let markdown = render(
        dir,
        r#"{"lint":"fail","test":"fail","audit":"fail"}"#,
        false,
        false,
    );

    assert!(markdown.contains("- No structured lint details available."));
    assert!(markdown.contains("- No structured test failure details available."));
    assert!(markdown.contains("- No structured audit findings available."));
    assert!(markdown
        .contains("- Full audit log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
}

#[test]
fn error_statuses_render_command_sections_and_classify_as_failures() {
    let guard = tmp_dir("error-statuses");
    let dir = guard.path();

    let markdown = render(
        dir,
        r#"{"lint":"error","test":"error","audit":"error"}"#,
        false,
        false,
    );

    assert!(markdown.contains("### Lint Failure Digest"));
    assert!(markdown.contains("### Test Failure Digest"));
    assert!(markdown.contains("### Audit Failure Digest"));
    assert!(markdown.contains("- No structured lint details available."));
    assert!(markdown.contains("- No structured test failure details available."));
    assert!(markdown.contains("- No structured audit findings available."));
    assert!(markdown.contains("- Overall: **human_needed**"));
    assert!(markdown.contains("- Human-needed failed commands:"));
    assert!(markdown.contains("  - `audit`"));
    assert!(markdown.contains("  - `lint`"));
    assert!(markdown.contains("  - `test`"));
    assert!(markdown
        .contains("- Full lint log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
    assert!(markdown
        .contains("- Full test log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
    assert!(markdown
        .contains("- Full audit log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
}

#[test]
fn renders_tooling_metadata_from_json_file() {
    let guard = tmp_dir("tooling");
    let dir = guard.path();
    let tooling_path = dir.join("tooling.json");
    write_file(dir, "tooling.json", TOOLING_JSON);

    let markdown = render_failure_digest_from_args(&FailureDigestArgs {
        output_dir: dir.to_string_lossy().to_string(),
        results: r#"{"lint":"pass"}"#.to_string(),
        run_url: None,
        tooling_json: Some(tooling_path.to_string_lossy().to_string()),
        commands: Some("lint".to_string()),
        autofix_commands: None,
        autofix_enabled: false,
        autofix_attempted: false,
        format: "markdown".to_string(),
    })
    .expect("failure digest should render");

    assert!(markdown.contains("### Tooling metadata"));
    assert!(markdown.contains("- action_repository: `Extra-Chill/homeboy-action`"));
    assert!(markdown.contains("- extension_id: `wordpress`"));
}

#[test]
fn renders_trace_pass_status_and_artifact_paths() {
    let guard = tmp_dir("trace-pass");
    let dir = guard.path();
    write_file(
        dir,
        "trace.json",
        &trace_json("pass", "Window stayed closed."),
    );

    let markdown = render(dir, r#"{"trace":"pass"}"#, false, false);

    assert!(markdown.contains("### Trace: studio / close-window-running-site"));
    assert!(markdown.contains("**Status:** PASS"));
    assert!(markdown.contains("- Window stayed closed."));
    assert!(markdown.contains("- main log: artifacts/main.log"));
    assert!(markdown.contains("- process tree: artifacts/process-tree.txt"));
    assert!(!markdown.contains("**Sample Runtime runtime diagnostics**"));
    assert!(!markdown.contains("raw log body that should never appear"));
}

#[test]
fn renders_bench_status_and_artifact_paths() {
    let guard = tmp_dir("bench-pass");
    let dir = guard.path();
    write_file(dir, "bench.json", bench_json());

    let markdown = render(dir, r#"{"bench":"pass"}"#, false, false);

    assert!(markdown.contains("### Bench: studio"));
    assert!(markdown.contains("**Status:** PASSED"));
    assert!(markdown.contains("**Budget findings**"));
    assert!(markdown.contains(
        "| `rest.max_response_bytes` | /wp-json/sample/v1/items?per_page=100 | 4378195 | 250000 | bytes | REST response exceeded 250 KB budget |"
    ));
    assert!(markdown.contains(
        "- scenario `wp-admin-load` / run 0 — Playwright trace (playwright-trace): bench-artifacts/wp-admin-load/trace.zip"
    ));
    assert!(markdown.contains(
        "- scenario `wp-admin-load` — Final screenshot (screenshot): bench-artifacts/wp-admin-load/final.png"
    ));
    assert!(!markdown.contains("trace bytes should never appear"));
}

#[test]
fn renders_trace_fail_status_without_inlining_artifact_content() {
    let guard = tmp_dir("trace-fail");
    let dir = guard.path();
    write_file(
        dir,
        "trace.json",
        &trace_json("fail", "Window reopened after close."),
    );

    let markdown = render(dir, r#"{"trace":"fail"}"#, false, false);

    assert!(markdown.contains("**Status:** FAIL"));
    assert!(markdown.contains("- Window reopened after close."));
    assert!(markdown.contains("- main log: artifacts/main.log"));
    assert!(!markdown.contains("raw log body that should never appear"));
}

#[test]
fn renders_trace_span_summaries_with_metadata_and_missing_endpoints() {
    let guard = tmp_dir("trace-span-summaries");
    let dir = guard.path();
    write_file(dir, "trace.json", trace_json_with_span_summaries());

    let markdown = render(dir, r#"{"trace":"fail"}"#, false, false);

    assert!(markdown.contains("**Spans**"));
    assert!(markdown.contains("| `phase.boot_to_ready` | `runner.boot` | `runner.ready` | 125ms | ok | category=startup, critical, blocking, cacheable |"));
    assert!(markdown.contains("| `phase.ready_to_open` | `runner.ready` | `app.opened` | - | skipped: missing `app.opened`: span endpoint missing from timeline | category=ui, prewarmable |"));
}

#[test]
fn renders_trace_toolchain_provenance() {
    let guard = tmp_dir("trace-toolchain");
    let dir = guard.path();
    write_file(dir, "trace.json", trace_json_with_toolchain());

    let markdown = render(dir, r#"{"trace":"pass"}"#, false, false);

    assert!(markdown.contains("**Toolchain provenance**"));
    assert!(markdown.contains("- Mode: `development`; canonical: **no**"));
    assert!(
        markdown.contains("- Homeboy: `/repo/homeboy` @ `abc123` (branch `main`, dirty `false`)")
    );
    assert!(markdown.contains(
        "- Sample Runtime: `/repo/sample-runtime` @ `def456` (branch `main`, dirty `true`)"
    ));
    assert!(
        markdown.contains("- Target: `/repo/studio` @ `789abc` (branch `trunk`, dirty `false`)")
    );
    assert!(markdown.contains("HOMEBOY_SAMPLE_RUNTIME_BIN selected a local sample runtime path"));
}

#[test]
fn renders_sample_runtime_partial_manifest_and_failure_phase() {
    let guard = tmp_dir("trace-runtime-manifest");
    let dir = guard.path();
    write_file(
        dir,
        "trace.json",
        trace_json_with_sample_runtime_partial_manifest(),
    );

    let markdown = render(dir, r#"{"trace":"error"}"#, false, false);

    assert!(markdown.contains("**Runtime diagnostics**"));
    assert!(markdown.contains("- Failure phase: **browser probe hang/failure**"));
    assert!(markdown.contains("- Current/last phase: `browser.probe.wait_for_ready`"));
    assert!(markdown.contains("- Current command: `browser probe /wp-admin/`"));
    assert!(markdown.contains("- Last command: `runtime setup`"));
    assert!(markdown
        .contains("- Artifact manifest: sample-runtime-artifacts/runtime-123/manifest.json"));
    assert!(
        markdown.contains("- Runtime metadata: sample-runtime-artifacts/runtime-123/runtime.json")
    );
    assert!(
        markdown.contains("- Commands log: sample-runtime-artifacts/runtime-123/commands.jsonl")
    );
    assert!(markdown.contains("- Events log: sample-runtime-artifacts/runtime-123/events.jsonl"));
    assert!(markdown
        .contains("- Browser summary: sample-runtime-artifacts/runtime-123/browser-summary.json"));
}

#[test]
fn renders_trace_error_status_from_failure_envelope() {
    let guard = tmp_dir("trace-error");
    let dir = guard.path();
    write_file(
        dir,
        "trace.json",
        r#"{
            "success": false,
            "data": {
                "passed": false,
                "status": "error",
                "component": "studio",
                "exit_code": 2,
                "failure": {
                    "component_id": "studio",
                    "scenario_id": "close-window-running-site",
                    "exit_code": 2,
                    "stderr_excerpt": "runner failed before assertions"
                }
            }
        }"#,
    );

    let markdown = render(dir, r#"{"trace":"error"}"#, false, false);

    assert!(markdown.contains("### Trace: studio / close-window-running-site"));
    assert!(markdown.contains("**Status:** ERROR"));
    assert!(markdown.contains("- runner failed before assertions"));
    assert!(markdown.contains("- No structured trace artifacts available."));
}
