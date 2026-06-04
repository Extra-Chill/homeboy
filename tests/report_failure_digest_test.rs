use std::fs;
use std::path::{Path, PathBuf};

use homeboy::commands::report::{render_failure_digest_from_args, FailureDigestArgs};

const LINT_JSON: &str = include_str!("fixtures/failure_digest/lint.json");
const TEST_JSON: &str = include_str!("fixtures/failure_digest/test.json");
const AUDIT_JSON: &str = include_str!("fixtures/failure_digest/audit.json");
const TOOLING_JSON: &str = include_str!("fixtures/failure_digest/tooling.json");

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-failure-digest-{name}-{nanos}"))
}

fn write_file(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("fixture should be written");
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
                "reasons": ["HOMEBOY_WP_CODEBOX_BIN selected a local WP Codebox runner path"],
                "homeboy": {"path":"/repo/homeboy","sha":"abc123","branch":"main","dirty":false},
                "wp_codebox": {"path":"/repo/wp-codebox","sha":"def456","branch":"main","dirty":true},
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
                    "fingerprint": "rest.max_response_bytes:/wp-json/datamachine/v1/pipelines?per_page=100",
                    "source": { "kind": "budget", "label": "profile:wordpress-rest" },
                    "metadata": {
                        "code": "rest.max_response_bytes",
                        "context_label": "profile:wordpress-rest",
                        "actual": 4378195,
                        "expected": 250000,
                        "unit": "bytes",
                        "subject": "/wp-json/datamachine/v1/pipelines?per_page=100",
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
    let dir = tmp_dir("lint");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "lint.json", LINT_JSON);

    let markdown = render(&dir, r#"{"lint":"fail"}"#, false, false);

    assert!(markdown.contains("## Failure Digest"));
    assert!(markdown.contains("### Lint Failure Digest"));
    assert!(markdown.contains("- Lint summary: **3 lint finding(s)**"));
    assert!(markdown.contains("<details><summary>Top lint violations</summary>"));
    assert!(markdown
        .contains("- Full lint log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));
    assert!(!markdown.contains("### Test Failure Digest"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_test_failure_digest_from_fixture() {
    let dir = tmp_dir("test");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "test.json", TEST_JSON);

    let markdown = render(&dir, r#"{"test":"fail"}"#, false, false);

    assert!(markdown.contains("### Test Failure Digest"));
    assert!(markdown.contains("- Failed tests: **2**"));
    assert!(markdown
        .contains("1. test_widget_renders — expected widget output — tests/widget_test.rs:42"));
    assert!(markdown.contains(
        "2. test_widget_handles_empty_state — empty state missing — tests/widget_test.rs"
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_audit_failure_digest_from_fixture() {
    let dir = tmp_dir("audit");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "audit.json", AUDIT_JSON);

    let markdown = render(&dir, r#"{"audit":"fail"}"#, false, false);

    assert!(markdown.contains("### Audit Failure Digest"));
    assert!(markdown.contains("- Alignment score: **0.812**"));
    assert!(markdown.contains("- Severity counts: **high: 1, low: 1, medium: 1**"));
    assert!(markdown.contains("- New findings since baseline: **1**"));
    assert!(markdown.contains("1. **src/report.rs** — new report module lacks tests (`abc123`)"));
    assert!(markdown.contains("**src/render.rs** — god_file — file is too large"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_mixed_failures_with_autofix_enabled_not_attempted() {
    let dir = tmp_dir("mixed-autofix");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "lint.json", LINT_JSON);
    write_file(&dir, "test.json", TEST_JSON);

    let markdown = render(
        &dir,
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

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_mixed_failures_after_autofix_attempted_as_human_needed() {
    let dir = tmp_dir("attempted");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "lint.json", LINT_JSON);

    let markdown = render(&dir, r#"{"lint":"fail"}"#, true, true);

    assert!(markdown.contains("- Overall: **human_needed**"));
    assert!(markdown.contains("- Autofix attempted this run: **yes**"));
    assert!(markdown.contains("- Human-needed failed commands:"));
    assert!(markdown.contains("  - `lint`"));
    assert!(markdown.contains("- Failed commands with available automated fixes:"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn missing_json_renders_explicit_structured_details_unavailable() {
    let dir = tmp_dir("missing");
    fs::create_dir_all(&dir).expect("temp dir should exist");

    let markdown = render(
        &dir,
        r#"{"lint":"fail","test":"fail","audit":"fail"}"#,
        false,
        false,
    );

    assert!(markdown.contains("- No structured lint details available."));
    assert!(markdown.contains("- No structured test failure details available."));
    assert!(markdown.contains("- No structured audit findings available."));
    assert!(markdown
        .contains("- Full audit log: https://github.com/Extra-Chill/homeboy/actions/runs/123"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn error_statuses_render_command_sections_and_classify_as_failures() {
    let dir = tmp_dir("error-statuses");
    fs::create_dir_all(&dir).expect("temp dir should exist");

    let markdown = render(
        &dir,
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

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_tooling_metadata_from_json_file() {
    let dir = tmp_dir("tooling");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    let tooling_path = dir.join("tooling.json");
    write_file(&dir, "tooling.json", TOOLING_JSON);

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

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_trace_pass_status_and_artifact_paths() {
    let dir = tmp_dir("trace-pass");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(
        &dir,
        "trace.json",
        &trace_json("pass", "Window stayed closed."),
    );

    let markdown = render(&dir, r#"{"trace":"pass"}"#, false, false);

    assert!(markdown.contains("### Trace: studio / close-window-running-site"));
    assert!(markdown.contains("**Status:** PASS"));
    assert!(markdown.contains("- Window stayed closed."));
    assert!(markdown.contains("- main log: artifacts/main.log"));
    assert!(markdown.contains("- process tree: artifacts/process-tree.txt"));
    assert!(!markdown.contains("raw log body that should never appear"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_bench_status_and_artifact_paths() {
    let dir = tmp_dir("bench-pass");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "bench.json", bench_json());

    let markdown = render(&dir, r#"{"bench":"pass"}"#, false, false);

    assert!(markdown.contains("### Bench: studio"));
    assert!(markdown.contains("**Status:** PASSED"));
    assert!(markdown.contains("**Budget findings**"));
    assert!(markdown.contains(
        "| `rest.max_response_bytes` | /wp-json/datamachine/v1/pipelines?per_page=100 | 4378195 | 250000 | bytes | REST response exceeded 250 KB budget |"
    ));
    assert!(markdown.contains(
        "- scenario `wp-admin-load` / run 0 — Playwright trace (playwright-trace): bench-artifacts/wp-admin-load/trace.zip"
    ));
    assert!(markdown.contains(
        "- scenario `wp-admin-load` — Final screenshot (screenshot): bench-artifacts/wp-admin-load/final.png"
    ));
    assert!(!markdown.contains("trace bytes should never appear"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_trace_fail_status_without_inlining_artifact_content() {
    let dir = tmp_dir("trace-fail");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(
        &dir,
        "trace.json",
        &trace_json("fail", "Window reopened after close."),
    );

    let markdown = render(&dir, r#"{"trace":"fail"}"#, false, false);

    assert!(markdown.contains("**Status:** FAIL"));
    assert!(markdown.contains("- Window reopened after close."));
    assert!(markdown.contains("- main log: artifacts/main.log"));
    assert!(!markdown.contains("raw log body that should never appear"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_trace_span_summaries_with_metadata_and_missing_endpoints() {
    let dir = tmp_dir("trace-span-summaries");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "trace.json", trace_json_with_span_summaries());

    let markdown = render(&dir, r#"{"trace":"fail"}"#, false, false);

    assert!(markdown.contains("**Spans**"));
    assert!(markdown.contains("| `phase.boot_to_ready` | `runner.boot` | `runner.ready` | 125ms | ok | category=startup, critical, blocking, cacheable |"));
    assert!(markdown.contains("| `phase.ready_to_open` | `runner.ready` | `app.opened` | - | skipped: missing `app.opened`: span endpoint missing from timeline | category=ui, prewarmable |"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_trace_toolchain_provenance() {
    let dir = tmp_dir("trace-toolchain");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(&dir, "trace.json", trace_json_with_toolchain());

    let markdown = render(&dir, r#"{"trace":"pass"}"#, false, false);

    assert!(markdown.contains("**Toolchain provenance**"));
    assert!(markdown.contains("- Mode: `development`; canonical: **no**"));
    assert!(
        markdown.contains("- Homeboy: `/repo/homeboy` @ `abc123` (branch `main`, dirty `false`)")
    );
    assert!(markdown
        .contains("- WP Codebox: `/repo/wp-codebox` @ `def456` (branch `main`, dirty `true`)"));
    assert!(
        markdown.contains("- Target: `/repo/studio` @ `789abc` (branch `trunk`, dirty `false`)")
    );
    assert!(markdown.contains("HOMEBOY_WP_CODEBOX_BIN selected a local WP Codebox runner path"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_trace_error_status_from_failure_envelope() {
    let dir = tmp_dir("trace-error");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_file(
        &dir,
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

    let markdown = render(&dir, r#"{"trace":"error"}"#, false, false);

    assert!(markdown.contains("### Trace: studio / close-window-running-site"));
    assert!(markdown.contains("**Status:** ERROR"));
    assert!(markdown.contains("- runner failed before assertions"));
    assert!(markdown.contains("- No structured trace artifacts available."));

    let _ = fs::remove_dir_all(&dir);
}
