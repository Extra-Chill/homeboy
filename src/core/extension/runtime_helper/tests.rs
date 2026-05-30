use super::*;
use crate::test_support::with_isolated_home;

#[test]
fn ensure_all_helpers_writes_all_files() {
    with_isolated_home(|_| {
        let pairs = ensure_all_helpers().expect("all helpers should be written");
        assert_eq!(pairs.len(), HELPERS.len());

        for (i, (env_var, path)) in pairs.iter().enumerate() {
            assert_eq!(env_var, HELPERS[i].env_var);
            let contents = std::fs::read_to_string(path).expect("helper should be readable");
            assert_eq!(contents, HELPERS[i].content);
        }

        assert!(
            pairs.iter().any(|(k, _)| k == FAILURE_TRAP_ENV),
            "failure trap helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == WRITE_TEST_RESULTS_ENV),
            "write test results helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == SIDECAR_WRITER_ENV),
            "sidecar writer helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == RESOLVE_CONTEXT_ENV),
            "resolve context helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == BENCH_HELPER_JS_ENV),
            "bench JS helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == BENCH_HELPER_PHP_ENV),
            "bench PHP helper should be in pairs"
        );
    });
}

#[test]
fn sidecar_writer_appends_and_merges_json_arrays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("sidecar-writer.sh");
    let target_path = dir.path().join("lint-findings.json");
    let source_path = dir.path().join("extra-findings.json");
    std::fs::write(&helper_path, assets::SIDECAR_WRITER_SH).expect("write helper");
    std::fs::write(&source_path, r#"[{"id":"third","message":"three"}]"#).expect("source");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_LINT_FINDINGS_FILE={}; homeboy_append_lint_finding '{{\"id\":\"first\",\"message\":\"one\"}}'; homeboy_append_lint_finding '{{\"id\":\"second\",\"message\":\"two\"}}'; homeboy_merge_lint_findings {}; cat {}",
            helper_path.display(),
            target_path.display(),
            source_path.display(),
            target_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        r#"[{"id":"first","message":"one"},{"id":"second","message":"two"},{"id":"third","message":"three"}]
"#
    );
}

#[test]
fn sidecar_writer_supports_test_failure_and_fix_result_wrappers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("sidecar-writer.sh");
    let failures_path = dir.path().join("test-failures.json");
    let fixes_path = dir.path().join("fix-results.json");
    std::fs::write(&helper_path, assets::SIDECAR_WRITER_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_TEST_FAILURES_FILE={}; HOMEBOY_FIX_RESULTS_FILE={}; homeboy_write_test_failures '{{\"test_id\":\"suite::case\",\"message\":\"failed\"}}'; homeboy_write_fix_results '{{\"file\":\"src/lib.rs\",\"message\":\"formatted\"}}'; printf '%s\n%s' \"$(cat {})\" \"$(cat {})\"",
            helper_path.display(),
            failures_path.display(),
            fixes_path.display(),
            failures_path.display(),
            fixes_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        r#"[{"test_id":"suite::case","message":"failed"}]
[{"file":"src/lib.rs","message":"formatted"}]"#
    );
}

#[test]
fn ensure_all_helpers_writes_legacy_bench_fallbacks() {
    with_isolated_home(|home| {
        ensure_all_helpers().expect("all helpers should be written");

        for filename in ["bench-helper.sh", "bench-helper.mjs", "bench-helper.php"] {
            let path = home.path().join(".homeboy").join("runtime").join(filename);
            assert!(
                path.exists(),
                "legacy bench helper fallback should exist: {}",
                path.display()
            );
        }

        assert!(
            !home
                .path()
                .join(".homeboy")
                .join("runtime")
                .join("runner-steps.sh")
                .exists(),
            "legacy runtime dir should only carry bench fallbacks"
        );
    });
}

#[test]
fn resolve_context_helper_exports_homeboy_env_and_aliases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("resolve-context.sh");
    std::fs::write(&helper_path, assets::RESOLVE_CONTEXT_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_resolve_context --component-alias PLUGIN_PATH; printf '%s|%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$COMPONENT_ID\" \"$PLUGIN_PATH\"",
            helper_path.display()
        ))
        .env("HOMEBOY_EXTENSION_PATH", "/tmp/ext")
        .env("HOMEBOY_COMPONENT_PATH", "/tmp/project")
        .env("HOMEBOY_COMPONENT_ID", "demo")
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "/tmp/ext|/tmp/project|demo|/tmp/project"
    );
}

#[test]
fn resolve_context_helper_supports_direct_invocation_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let extension_dir = dir.path().join("extension");
    let script_dir = extension_dir.join("scripts/test");
    let component_dir = dir.path().join("component");
    std::fs::create_dir_all(&script_dir).expect("script dir");
    std::fs::create_dir_all(&component_dir).expect("component dir");
    std::fs::write(extension_dir.join("extension.json"), "{}").expect("manifest marker");
    let helper_path = dir.path().join("resolve-context.sh");
    std::fs::write(&helper_path, assets::RESOLVE_CONTEXT_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "cd {}; source {}; SCRIPT_DIR={}; homeboy_resolve_context; printf '%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$COMPONENT_ID\"",
            component_dir.display(),
            helper_path.display(),
            script_dir.display()
        ))
        .env_remove("HOMEBOY_EXTENSION_PATH")
        .env_remove("HOMEBOY_COMPONENT_PATH")
        .env_remove("HOMEBOY_COMPONENT_ID")
        .env_remove("HOMEBOY_PROJECT_PATH")
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "{}|{}|component",
            extension_dir.display(),
            component_dir.display()
        )
    );
}

#[test]
fn bench_shell_helper_writes_empty_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.sh");
    let results_path = dir.path().join("bench-results.json");
    std::fs::write(&helper_path, assets::BENCH_HELPER_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_BENCH_RESULTS_FILE={}; homeboy_write_empty_bench_results demo 7; cat {}",
            helper_path.display(),
            results_path.display(),
            results_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"component_id\":\"demo\",\"iterations\":7,\"scenarios\":[]}\n"
    );
}

#[test]
fn bench_shell_helper_writes_scenarios_from_payload_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.sh");
    let results_path = dir.path().join("bench-results.json");
    let payload_path = dir.path().join("payload.json");
    let extras_path = dir.path().join("extras.json");
    std::fs::write(&helper_path, assets::BENCH_HELPER_SH).expect("write helper");
    std::fs::write(
        &payload_path,
        r#"{
  "timings_ns": [1000000, 3000000, 5000000],
  "peak_rss_bytes": 4096,
  "source": "custom",
  "metadata": {"fixture": true},
  "artifacts": {"stdout": {"path": "bench.log", "kind": "text"}},
  "metrics": {"custom_ms": 9.5}
}"#,
    )
    .expect("write payload");
    std::fs::write(
        &extras_path,
        r#"{
  "metadata": {"runner": {"status": "ok"}},
  "metric_groups": {"phases": {"setup_ms": 12}},
  "timeline": [{"id": "setup", "duration_ms": 12}]
}"#,
    )
    .expect("write extras");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_write_bench_results_from_payload_files --results-file {} --component demo --iterations 3 --extras-file {} 'bench-fast={}'; cat {}",
            helper_path.display(),
            results_path.display(),
            extras_path.display(),
            payload_path.display(),
            results_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(value["component_id"], "demo");
    assert_eq!(value["iterations"], 3);
    assert_eq!(value["metadata"]["runner"]["status"], "ok");
    assert_eq!(value["metric_groups"]["phases"]["setup_ms"], 12);
    assert_eq!(value["timeline"][0]["id"], "setup");
    assert_eq!(value["scenarios"][0]["id"], "bench-fast");
    assert_eq!(value["scenarios"][0]["iterations"], 3);
    assert_eq!(value["scenarios"][0]["metrics"]["mean_ms"], 3.0);
    assert_eq!(value["scenarios"][0]["metrics"]["p50_ms"], 3.0);
    assert_eq!(value["scenarios"][0]["metrics"]["custom_ms"], 9.5);
    assert_eq!(value["scenarios"][0]["memory"]["peak_bytes"], 4096);
    assert_eq!(value["scenarios"][0]["source"], "custom");
    assert_eq!(value["scenarios"][0]["metadata"]["fixture"], true);
    assert_eq!(value["scenarios"][0]["artifacts"]["stdout"]["path"], "bench.log");
}

#[test]
fn bench_shell_helper_writes_scenario_inventory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.sh");
    let results_path = dir.path().join("bench-results.json");
    std::fs::write(&helper_path, assets::BENCH_HELPER_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_write_bench_scenario_inventory --results-file {} --component demo --iterations 11 'bench-http=src/bin/bench-http.rs=rust-bin'; cat {}",
            helper_path.display(),
            results_path.display(),
            results_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(value["component_id"], "demo");
    assert_eq!(value["iterations"], 0);
    assert_eq!(value["scenarios"][0]["id"], "bench-http");
    assert_eq!(value["scenarios"][0]["iterations"], 0);
    assert_eq!(value["scenarios"][0]["default_iterations"], 11);
    assert_eq!(value["scenarios"][0]["file"], "src/bin/bench-http.rs");
    assert_eq!(value["scenarios"][0]["source"], "rust-bin");
}

#[test]
fn bench_js_helper_emits_compact_progress_to_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.mjs");
    let runner_path = dir.path().join("runner.mjs");
    std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
    std::fs::write(
        &runner_path,
        r#"import { homeboyBenchProgress } from './bench-helper.mjs';
homeboyBenchProgress({ scenario: 'studio-agent-site-build', run: 'bfb', elapsed_ms: 252000, turn: 18, tools: 23, last: 'wp_cli page_update' });
"#,
    )
    .expect("write runner");

    let output = std::process::Command::new("node")
        .arg(&runner_path)
        .env("HOMEBOY_BENCH_PROGRESS", "1")
        .env("HOMEBOY_BENCH_PROGRESS_STREAM", "stderr")
        .output()
        .expect("run node");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "studio-agent-site-build [bfb] 04:12 turn=18 tools=23 last=wp_cli page_update\n"
    );
}

#[test]
fn bench_js_helper_keeps_progress_quiet_when_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.mjs");
    let runner_path = dir.path().join("runner.mjs");
    std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
    std::fs::write(
        &runner_path,
        r#"import { homeboyBenchProgress } from './bench-helper.mjs';
homeboyBenchProgress({ scenario: 'studio-agent-site-build', phase: 'setup' });
"#,
    )
    .expect("write runner");

    let output = std::process::Command::new("node")
        .arg(&runner_path)
        .env("HOMEBOY_BENCH_PROGRESS", "0")
        .output()
        .expect("run node");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn bench_runtime_helpers_document_shared_contract() {
    for content in [
        assets::BENCH_HELPER_JS,
        assets::BENCH_HELPER_PHP,
        assets::BENCH_HELPER_SH,
    ] {
        assert!(
            content.contains("R-7 percentile"),
            "helper should document percentile method"
        );
        assert!(
            content.contains("p * (n - 1)") || content.contains("$p * ($n - 1)"),
            "helper should use R-7 rank formula"
        );
        assert!(
            content.contains("scenario") && content.contains("slug"),
            "helper should own scenario slugging"
        );
        assert!(
            content.contains("component_id") && content.contains("scenarios"),
            "helper should own BenchResults envelope shape"
        );
    }
}
