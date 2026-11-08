//! The core-shipped runtime shell helpers emit their JSON through the real
//! `homeboy` binary (`HOMEBOY_RUNTIME_EXECUTABLE`), never through Python.
//!
//! Every helper runs with `python3`/`python` shims first on PATH that fail
//! loudly, so any reintroduced interpreter dependency breaks these tests.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn helper(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-core/src/extension/runtime")
        .join(name)
}

fn run_helper(dir: &Path, helper_name: &str, script: &str) -> Output {
    run_helper_with_env(dir, helper_name, script, &[])
}

fn run_helper_with_env(
    dir: &Path,
    helper_name: &str,
    script: &str,
    envs: &[(&str, &str)],
) -> Output {
    let poison = dir.join("poison-bin");
    std::fs::create_dir_all(&poison).expect("poison dir");
    for interpreter in ["python3", "python"] {
        let shim = poison.join(interpreter);
        std::fs::write(
            &shim,
            "#!/bin/sh\necho \"runtime helper invoked $0\" >&2\nexit 97\n",
        )
        .expect("write shim");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}'; {script}",
            helper(helper_name).display()
        ))
        .env(
            "PATH",
            format!(
                "{}:{}",
                poison.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOMEBOY_RUNTIME_EXECUTABLE", env!("CARGO_BIN_EXE_homeboy"))
        .envs(envs.iter().copied())
        .output()
        .expect("run bash")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sha256_hex(identity: &str) -> String {
    homeboy_engine_primitives::content_hash::sha256_hex(identity.as_bytes())
}

#[test]
fn sidecar_writer_appends_and_merges_json_arrays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("lint-findings.json");
    let source = dir.path().join("extra-findings.json");
    std::fs::write(
        &source,
        r#"[{"tool":"lint","message":"three","fingerprint":"third"}]"#,
    )
    .expect("source");

    let output = run_helper(
        dir.path(),
        "sidecar-writer.sh",
        &format!(
            "HOMEBOY_LINT_FINDINGS_FILE={target}; homeboy_append_lint_finding '{{\"tool\":\"lint\",\"message\":\"one\",\"fingerprint\":\"first\"}}'; homeboy_append_lint_finding '{{\"tool\":\"lint\",\"message\":\"two\",\"fingerprint\":\"second\"}}'; homeboy_merge_lint_findings {source}; cat {target}",
            target = target.display(),
            source = source.display(),
        ),
    );

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        r#"[{"tool":"lint","message":"one","fingerprint":"first"},{"tool":"lint","message":"two","fingerprint":"second"},{"tool":"lint","message":"three","fingerprint":"third"}]
"#
    );
}

#[test]
fn sidecar_writer_supports_test_failure_fix_result_and_results_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let failures = dir.path().join("test-failures.json");
    let fixes = dir.path().join("fix-results.json");
    let results = dir.path().join("nested").join("test-results.json");

    let output = run_helper(
        dir.path(),
        "sidecar-writer.sh",
        &format!(
            "HOMEBOY_TEST_FAILURES_FILE={failures}; HOMEBOY_FIX_RESULTS_FILE={fixes}; HOMEBOY_TEST_RESULTS_FILE={results}; homeboy_write_test_failures '{{\"test_id\":\"suite::case\",\"message\":\"failed\"}}'; homeboy_write_fix_results '{{\"file\":\"src/lib.rs\",\"message\":\"formatted\"}}'; homeboy_write_test_results_json '{{\"total\":5,\"passed\":3,\"failed\":1,\"skipped\":1}}'; cat {failures} {fixes} {results}",
            failures = failures.display(),
            fixes = fixes.display(),
            results = results.display(),
        ),
    );

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        r#"[{"test_id":"suite::case","message":"failed"}]
[{"file":"src/lib.rs","message":"formatted"}]
{"total":5,"passed":3,"failed":1,"skipped":1}
"#
    );
}

#[test]
fn sidecar_writer_supports_annotation_source_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let annotations = dir.path().join("annotations");
    let source = dir.path().join("annotations-extra.json");
    std::fs::write(&source, r#"[{"file":"b.txt","line":2}]"#).expect("source");

    let output = run_helper(
        dir.path(),
        "sidecar-writer.sh",
        &format!(
            "HOMEBOY_ANNOTATIONS_DIR={annotations}; homeboy_write_annotations fixture-a '{{\"file\":\"a.txt\",\"line\":1}}'; homeboy_merge_annotations fixture-b {source}; cat {annotations}/fixture-a.json {annotations}/fixture-b.json",
            annotations = annotations.display(),
            source = source.display(),
        ),
    );

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "[{\"file\":\"a.txt\",\"line\":1}]\n[{\"file\":\"b.txt\",\"line\":2}]\n"
    );
}

#[test]
fn sidecar_writer_requires_runtime_executable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("lint-findings.json");
    let output = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{}'; HOMEBOY_LINT_FINDINGS_FILE={}; homeboy_append_lint_finding '{{}}'",
            helper("sidecar-writer.sh").display(),
            target.display()
        ))
        .env_remove("HOMEBOY_RUNTIME_EXECUTABLE")
        .output()
        .expect("run bash");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HOMEBOY_RUNTIME_EXECUTABLE"));
    assert!(!target.exists());
}

#[test]
fn emit_test_failure_uses_identity_override_for_fingerprint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let identity = "rust:test:mymod::tests::it_works";
    let output = run_helper(
        dir.path(),
        "emit-test-failure.sh",
        &format!(
            "homeboy_emit_test_failure --test-id 'mymod::tests::it_works' --message 'Rust test failed: mymod::tests::it_works' --identity '{identity}' --stdout-excerpt 'tail'"
        ),
    );

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "{{\"test_id\":\"mymod::tests::it_works\",\"suite\":null,\"file\":null,\"line\":null,\"message\":\"Rust test failed: mymod::tests::it_works\",\"failure_type\":\"test_failure\",\"fingerprint\":\"{}\",\"stdout_excerpt\":\"tail\",\"stderr_excerpt\":\"\"}}\n",
            sha256_hex(identity)
        )
    );
}

#[test]
fn emit_test_failure_derives_canonical_fingerprint_and_supports_infra_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_helper(
        dir.path(),
        "emit-test-failure.sh",
        "homeboy_emit_test_failure --test-id 'Foo::testBar' --suite phpunit --file src/Foo.php --line 42 --failure-type AssertionError --message 'Failed asserting that false is true.' --stdout-excerpt 'out'; homeboy_emit_test_failure --test-id 'cargo test' --failure-type infrastructure --message 'cargo test failed before individual test failures could be parsed' --identity 'rust:cargo-test:failed'",
    );

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let records = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("record json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["suite"], "phpunit");
    assert_eq!(records[0]["file"], "src/Foo.php");
    assert_eq!(records[0]["line"], 42);
    assert_eq!(records[0]["failure_type"], "AssertionError");
    let identity = [
        "Foo::testBar",
        "src/Foo.php",
        "42",
        "AssertionError",
        "Failed asserting that false is true.",
    ]
    .join("\0");
    assert_eq!(records[0]["fingerprint"], sha256_hex(&identity));
    assert_eq!(records[1]["failure_type"], "infrastructure");
    assert_eq!(
        records[1]["fingerprint"],
        sha256_hex("rust:cargo-test:failed")
    );
}

#[test]
fn emit_test_failure_rejects_missing_test_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_helper(
        dir.path(),
        "emit-test-failure.sh",
        "homeboy_emit_test_failure --message boom",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--test-id is required"));
}

#[test]
fn emit_test_failure_bound_excerpt_matches_canonical_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let raw: String = (0..50)
        .map(|i| format!("{}-{}", i, "x".repeat(196)))
        .collect::<Vec<_>>()
        .join("\n");
    let output = run_helper_with_env(
        dir.path(),
        "emit-test-failure.sh",
        "excerpt=\"$(homeboy_test_failure_bound_excerpt \"$RAW\")\"; printf '%s' \"$excerpt\"",
        &[("RAW", raw.as_str())],
    );

    assert_success(&output);
    let excerpt = String::from_utf8_lossy(&output.stdout);
    assert_eq!(excerpt.chars().count(), 4000);
    assert!(excerpt.ends_with("..."));
}

#[test]
fn bench_helper_writes_scenarios_from_payload_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let results = dir.path().join("bench-results.json");
    let payload = dir.path().join("payload.json");
    let extras = dir.path().join("extras.json");
    std::fs::write(
        &payload,
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
        &extras,
        r#"{
  "metadata": {"runner": {"status": "ok"}},
  "metric_groups": {"phases": {"setup_ms": 12}},
  "timeline": [{"id": "setup", "duration_ms": 12}]
}"#,
    )
    .expect("write extras");

    let output = run_helper(
        dir.path(),
        "bench-helper.sh",
        &format!(
            "homeboy_write_bench_results_from_payload_files --results-file {results} --component demo --iterations 3 --extras-file {extras} 'bench-fast={payload}'; cat {results}",
            results = results.display(),
            extras = extras.display(),
            payload = payload.display(),
        ),
    );

    assert_success(&output);
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
    assert_eq!(
        value["scenarios"][0]["artifacts"]["stdout"]["path"],
        "bench.log"
    );
}

#[test]
fn bench_helper_writes_scenario_inventory_from_runner_variables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let results = dir.path().join("bench-results.json");

    // Runner variables are plain (unexported) shell variables here: the
    // helper must forward them rather than rely on the child's environment.
    let output = run_helper(
        dir.path(),
        "bench-helper.sh",
        &format!(
            "HOMEBOY_BENCH_RESULTS_FILE={results}; HOMEBOY_COMPONENT_ID=demo; HOMEBOY_BENCH_ITERATIONS=11; homeboy_write_bench_scenario_inventory 'bench-http=src/bin/bench-http.rs=native-bin'; cat {results}",
            results = results.display(),
        ),
    );

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(value["component_id"], "demo");
    assert_eq!(value["iterations"], 0);
    assert_eq!(value["scenarios"][0]["id"], "bench-http");
    assert_eq!(value["scenarios"][0]["iterations"], 0);
    assert_eq!(value["scenarios"][0]["default_iterations"], 11);
    assert_eq!(value["scenarios"][0]["file"], "src/bin/bench-http.rs");
    assert_eq!(value["scenarios"][0]["source"], "native-bin");
}

#[test]
fn bench_helper_checks_selected_scenarios_artifact_refs_and_pings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pings = dir.path().join("pings").join("responsiveness.jsonl");
    let output = run_helper(
        dir.path(),
        "bench-helper.sh",
        &format!(
            "HOMEBOY_BENCH_SCENARIOS=cold,warm; HOMEBOY_BENCH_RESPONSIVENESS_FILE={pings}; if homeboy_bench_scenario_selected cold && ! homeboy_bench_scenario_selected hot; then homeboy_bench_artifact_ref_json bench.log text 'Bench log'; fi; homeboy_bench_responsiveness_ping setup; homeboy_bench_responsiveness_ping",
            pings = pings.display(),
        ),
    );

    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"path\":\"bench.log\",\"kind\":\"text\",\"label\":\"Bench log\"}\n"
    );
    let lines = std::fs::read_to_string(&pings).expect("pings");
    let lines = lines
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("ping json"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["label"], "setup");
    assert!(lines[0]["t_ms"].is_u64());
    assert!(lines[1].get("label").is_none());
}
