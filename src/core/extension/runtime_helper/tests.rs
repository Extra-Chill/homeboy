use super::*;
use crate::test_support::{home_env_guard, with_isolated_home};

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
            pairs.iter().any(|(k, _)| k == RUNNER_PRELUDE_ENV),
            "runner prelude helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == COMMAND_CAPTURE_ENV),
            "command capture helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == BASH_PREFLIGHT_ENV),
            "bash preflight helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == WRITE_TEST_RESULTS_ENV),
            "write test results helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == EMIT_LINT_FINDING_ENV),
            "emit lint finding helper should be in pairs"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == EMIT_TEST_FAILURE_ENV),
            "emit test failure helper should be in pairs"
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
fn helper_path_resolves_by_filename_and_env_var() {
    with_isolated_home(|_| {
        let by_filename = helper_path("command-capture.sh").expect("helper path by filename");
        let by_env = helper_path(COMMAND_CAPTURE_ENV).expect("helper path by env var");

        assert_eq!(by_filename, by_env);
        assert!(by_filename.is_file());
    });
}

#[test]
fn runner_prelude_initializes_context_steps_trap_and_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    std::fs::write(
        runtime_dir.join("runner-prelude.sh"),
        assets::RUNNER_PRELUDE_SH,
    )
    .expect("write prelude");
    std::fs::write(
        runtime_dir.join("resolve-context.sh"),
        assets::RESOLVE_CONTEXT_SH,
    )
    .expect("write resolve context");
    std::fs::write(runtime_dir.join("runner-steps.sh"), assets::RUNNER_STEPS_SH)
        .expect("write runner steps");
    std::fs::write(runtime_dir.join("failure-trap.sh"), assets::FAILURE_TRAP_SH)
        .expect("write failure trap");
    std::fs::write(
        runtime_dir.join("sidecar-writer.sh"),
        assets::SIDECAR_WRITER_SH,
    )
    .expect("write sidecar writer");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_runner_init --bash 4 --component-alias PLUGIN_PATH --steps --failure-trap --sidecar-writer; should_run_step test; type homeboy_append_lint_finding >/dev/null; printf '%s|%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$PLUGIN_PATH\" \"$FAILED_STEP\"",
            runtime_dir.join("runner-prelude.sh").display()
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
        "/tmp/ext|/tmp/project|/tmp/project|"
    );
}

#[test]
fn ensure_all_helpers_falls_back_when_home_is_unavailable() {
    let _guard = home_env_guard();
    let prior = std::env::var("HOME").ok();
    std::env::remove_var("HOME");

    let pairs = ensure_all_helpers().expect("helpers should fall back to temp runtime dir");
    let sidecar = pairs
        .iter()
        .find_map(|(key, value)| (key == SIDECAR_WRITER_ENV).then_some(value))
        .expect("sidecar writer helper");
    let sidecar_exists = std::path::Path::new(sidecar).is_file();

    match prior {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert!(
        sidecar_exists,
        "sidecar helper should be written under fallback runtime dir: {sidecar}"
    );
}

#[test]
fn sidecar_writer_appends_and_merges_json_arrays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("sidecar-writer.sh");
    let target_path = dir.path().join("lint-findings.json");
    let source_path = dir.path().join("extra-findings.json");
    std::fs::write(&helper_path, assets::SIDECAR_WRITER_SH).expect("write helper");
    std::fs::write(
        &source_path,
        r#"[{"tool":"lint","message":"three","fingerprint":"third"}]"#,
    )
    .expect("source");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_LINT_FINDINGS_FILE={}; homeboy_append_lint_finding '{{\"tool\":\"lint\",\"message\":\"one\",\"fingerprint\":\"first\"}}'; homeboy_append_lint_finding '{{\"tool\":\"lint\",\"message\":\"two\",\"fingerprint\":\"second\"}}'; homeboy_merge_lint_findings {}; cat {}",
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
        r#"[{"tool":"lint","message":"one","fingerprint":"first"},{"tool":"lint","message":"two","fingerprint":"second"},{"tool":"lint","message":"three","fingerprint":"third"}]
"#
    );
}

fn shasum_hex(algo: &str, identity: &str) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("shasum")
        .arg("-a")
        .arg(algo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shasum");
    child
        .stdin
        .take()
        .expect("shasum stdin")
        .write_all(identity.as_bytes())
        .expect("write identity");
    let output = child.wait_with_output().expect("shasum output");
    assert!(output.status.success(), "shasum should succeed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("hash field")
        .to_string()
}

#[test]
fn emit_lint_finding_shim_emits_normalized_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("emit-lint-finding.sh");
    std::fs::write(&helper_path, assets::EMIT_LINT_FINDING_SH).expect("write helper");

    // Line 1 is long enough to prove the 240-char excerpt truncation; line 2 is a
    // normal short line.
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("src dir");
    let long_line = "a".repeat(300);
    std::fs::write(src_dir.join("lib.rs"), format!("{long_line}\nlet x = 1;\n"))
        .expect("write source");

    let identity = "rust:clippy:src/lib.rs:1:5:unused:unused variable";
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_emit_lint_finding --root {} --id '{}' --file src/lib.rs --line 1 --column 5 --severity warning --source clippy --code unused --category correctness --message 'unused variable' --fixable false",
            helper_path.display(),
            dir.path().display(),
            identity
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let record: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("record is valid json");
    assert_eq!(record["id"], identity);
    assert_eq!(record["file"], "src/lib.rs");
    assert_eq!(record["line"], 1);
    assert_eq!(record["column"], 5);
    assert_eq!(record["severity"], "warning");
    assert_eq!(record["source"], "clippy");
    assert_eq!(record["code"], "unused");
    assert_eq!(record["category"], "correctness");
    assert_eq!(record["message"], "unused variable");
    assert_eq!(record["fixable"], false);

    // Fingerprint matches the per-language builders byte-for-byte: sha1 of identity.
    assert_eq!(
        record["fingerprint"].as_str().expect("fingerprint string"),
        shasum_hex("1", identity)
    );

    // Excerpt is the first 240 characters of the source line, like the reference builders.
    let excerpt = record["excerpt"].as_str().expect("excerpt string");
    assert_eq!(excerpt.chars().count(), 240);
    assert_eq!(excerpt, "a".repeat(240));
}

#[test]
fn emit_lint_finding_shim_null_excerpt_when_line_out_of_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("emit-lint-finding.sh");
    std::fs::write(&helper_path, assets::EMIT_LINT_FINDING_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_emit_lint_finding --root {} --id 'go:gofmt:missing.go:1' --file missing.go --line 1 --source gofmt --code formatting --category format --message 'File is not gofmt-formatted' --fixable true",
            helper_path.display(),
            dir.path().display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("record is valid json");
    assert!(record["excerpt"].is_null(), "excerpt should be null");
    assert_eq!(record["fixable"], true);
}

#[test]
fn emit_test_failure_shim_uses_identity_override_for_fingerprint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("emit-test-failure.sh");
    std::fs::write(&helper_path, assets::EMIT_TEST_FAILURE_SH).expect("write helper");

    // Identity override reproduces the per-language fingerprint (e.g. rust's
    // sha256("rust:test:<name>")) byte-for-byte.
    let identity = "rust:test:mymod::tests::it_works";
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_emit_test_failure --test-id 'mymod::tests::it_works' --message 'Rust test failed: mymod::tests::it_works' --identity '{}' --stdout-excerpt 'tail'",
            helper_path.display(),
            identity
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("record is valid json");
    assert_eq!(record["test_id"], "mymod::tests::it_works");
    assert!(record["suite"].is_null(), "suite should be null when empty");
    assert!(record["file"].is_null(), "file should be null when empty");
    assert!(record["line"].is_null(), "line should be null when absent");
    assert_eq!(record["failure_type"], "test_failure");
    assert_eq!(record["stdout_excerpt"], "tail");
    assert_eq!(record["stderr_excerpt"], "");
    assert_eq!(
        record["fingerprint"].as_str().expect("fingerprint string"),
        shasum_hex("256", identity)
    );
}

#[test]
fn emit_test_failure_shim_derives_canonical_fingerprint_and_supports_infra_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("emit-test-failure.sh");
    std::fs::write(&helper_path, assets::EMIT_TEST_FAILURE_SH).expect("write helper");

    // No --identity: the shim derives the canonical NUL-joined identity
    // [test_id, file, line, failure_type, first_message_line], matching the
    // WordPress make_failure_fingerprint contract.
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_emit_test_failure --test-id 'Foo::testBar' --suite phpunit --file src/Foo.php --line 42 --failure-type AssertionError --message 'Failed asserting that false is true.' --stdout-excerpt 'out'",
            helper_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("record is valid json");
    assert_eq!(record["test_id"], "Foo::testBar");
    assert_eq!(record["suite"], "phpunit");
    assert_eq!(record["file"], "src/Foo.php");
    assert_eq!(record["line"], 42);
    assert_eq!(record["failure_type"], "AssertionError");

    let identity = ["Foo::testBar", "src/Foo.php", "42", "AssertionError", "Failed asserting that false is true."]
        .join("\0");
    assert_eq!(
        record["fingerprint"].as_str().expect("fingerprint string"),
        shasum_hex("256", &identity)
    );

    // Infra-fallback record: failure_type is "infrastructure".
    let infra = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_emit_test_failure --test-id 'cargo test' --failure-type infrastructure --message 'cargo test failed before individual test failures could be parsed' --identity 'rust:cargo-test:failed'",
            helper_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        infra.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&infra.stderr)
    );
    let infra_record: serde_json::Value =
        serde_json::from_slice(&infra.stdout).expect("infra record is valid json");
    assert_eq!(infra_record["failure_type"], "infrastructure");
    assert_eq!(
        infra_record["fingerprint"].as_str().expect("fingerprint string"),
        shasum_hex("256", "rust:cargo-test:failed")
    );
}

#[test]
fn emit_test_failure_bound_excerpt_matches_canonical_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("emit-test-failure.sh");
    std::fs::write(&helper_path, assets::EMIT_TEST_FAILURE_SH).expect("write helper");

    // 50 lines of 200 chars each: first 40 lines kept, then capped at 4000 chars.
    let raw: String = (0..50)
        .map(|i| format!("{}-{}", i, "x".repeat(196)))
        .collect::<Vec<_>>()
        .join("\n");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_test_failure_bound_excerpt \"$RAW\"",
            helper_path.display()
        ))
        .env("RAW", &raw)
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let excerpt = String::from_utf8_lossy(&output.stdout);
    assert_eq!(excerpt.chars().count(), 4000, "excerpt should cap at 4000 chars");
    assert!(excerpt.ends_with("..."), "capped excerpt should end with ellipsis");
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
fn sidecar_writer_supports_test_results_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("sidecar-writer.sh");
    let results_path = dir.path().join("nested").join("test-results.json");
    std::fs::write(&helper_path, assets::SIDECAR_WRITER_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_TEST_RESULTS_FILE={}; homeboy_write_test_results_json '{{\"total\":5,\"passed\":3,\"failed\":1,\"skipped\":1}}'; cat {}",
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
        r#"{"total":5,"passed":3,"failed":1,"skipped":1}
"#
    );
}

#[test]
fn write_test_results_creates_parent_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("write-test-results.sh");
    let results_path = dir.path().join("nested").join("test-results.json");
    std::fs::write(&helper_path, assets::WRITE_TEST_RESULTS_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_TEST_RESULTS_FILE={}; homeboy_write_test_results 5 3 1 1; cat {}",
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
    assert!(String::from_utf8_lossy(&output.stdout).contains(r#""total": 5"#));
}

#[test]
fn sidecar_writer_supports_annotation_source_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("sidecar-writer.sh");
    let annotations_dir = dir.path().join("annotations");
    let source_path = dir.path().join("annotations-extra.json");
    std::fs::write(&helper_path, assets::SIDECAR_WRITER_SH).expect("write helper");
    let source_file = ["b.", "p", "hp"].concat();
    let written_file = ["a.", "p", "hp"].concat();
    std::fs::write(
        &source_path,
        format!(r#"[{{"file":"{source_file}","line":2}}]"#),
    )
    .expect("source");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_ANNOTATIONS_DIR={}; homeboy_write_annotations fixture-a '{{\"file\":\"{}\",\"line\":1}}'; homeboy_merge_annotations fixture-b {}; printf '%s\n%s' \"$(cat {}/fixture-a.json)\" \"$(cat {}/fixture-b.json)\"",
            helper_path.display(),
            annotations_dir.display(),
            written_file,
            source_path.display(),
            annotations_dir.display(),
            annotations_dir.display()
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
        format!(
            r#"[{{"file":"{written_file}","line":1}}]
[{{"file":"{source_file}","line":2}}]"#
        )
    );
}

#[test]
fn ensure_all_helpers_writes_legacy_bench_fallbacks() {
    with_isolated_home(|home| {
        ensure_all_helpers().expect("all helpers should be written");

        for filename in [
            "bench-helper.sh".to_string(),
            "bench-helper.mjs".to_string(),
            ["bench-helper.", "p", "hp"].concat(),
        ] {
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
        assert!(
            !home
                .path()
                .join(".homeboy")
                .join("runtime")
                .join("bash-preflight.sh")
                .exists(),
            "legacy runtime dir should not carry bash preflight"
        );
    });
}

#[test]
fn bash_preflight_helper_accepts_current_bash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bash-preflight.sh");
    std::fs::write(&helper_path, assets::BASH_PREFLIGHT_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_require_bash_version 3; printf ok",
            helper_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok");
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
    assert_eq!(
        value["scenarios"][0]["artifacts"]["stdout"]["path"],
        "bench.log"
    );
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
            "source {}; homeboy_write_bench_scenario_inventory --results-file {} --component demo --iterations 11 'bench-http=src/bin/bench-http.rs=native-bin'; cat {}",
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
    assert_eq!(value["scenarios"][0]["source"], "native-bin");
}

#[test]
fn bench_shell_helper_checks_selected_scenarios_and_artifact_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.sh");
    std::fs::write(&helper_path, assets::BENCH_HELPER_SH).expect("write helper");

    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_BENCH_SCENARIOS=cold,warm; if homeboy_bench_scenario_selected cold && ! homeboy_bench_scenario_selected hot; then homeboy_bench_artifact_ref_json bench.log text 'Bench log'; fi",
            helper_path.display()
        ))
        .output()
        .expect("run bash");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(value["path"], "bench.log");
    assert_eq!(value["kind"], "text");
    assert_eq!(value["label"], "Bench log");
}

#[test]
fn bench_js_helper_writes_scenario_inventory_and_filters_selection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.mjs");
    let runner_path = dir.path().join("runner.mjs");
    let results_path = dir.path().join("bench-results.json");
    std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
    std::fs::write(
        &runner_path,
        format!(
            r#"import {{
    homeboyBenchArtifactRef,
    homeboyBenchScenarioInventoryEntry,
    homeboyBenchScenarioSelected,
    homeboyWriteBenchScenarioInventory,
}} from './bench-helper.mjs';

if (!homeboyBenchScenarioSelected('cold')) throw new Error('cold should be selected');
if (homeboyBenchScenarioSelected('hot')) throw new Error('hot should not be selected');

await homeboyWriteBenchScenarioInventory('{results}', 'demo', 9, [
    homeboyBenchScenarioInventoryEntry({{
        id: 'cold',
        file: 'bench/cold.bench.js',
        source: 'in_tree',
        artifacts: {{ log: homeboyBenchArtifactRef('bench.log', {{ kind: 'text', label: 'Bench log' }}) }},
    }}),
]);
"#,
            results = results_path.display()
        ),
    )
    .expect("write runner");

    let output = std::process::Command::new("node")
        .arg(&runner_path)
        .env("HOMEBOY_BENCH_SCENARIOS", "cold,warm")
        .output()
        .expect("run node");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&results_path).expect("read results"))
            .expect("json");
    assert_eq!(value["component_id"], "demo");
    assert_eq!(value["iterations"], 0);
    assert_eq!(value["scenarios"][0]["id"], "cold");
    assert_eq!(value["scenarios"][0]["iterations"], 0);
    assert_eq!(value["scenarios"][0]["default_iterations"], 9);
    assert_eq!(
        value["scenarios"][0]["artifacts"]["log"]["path"],
        "bench.log"
    );
}

#[test]
fn bench_js_helper_records_phase_resource_summary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_path = dir.path().join("bench-helper.mjs");
    let runner_path = dir.path().join("runner.mjs");
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
    std::fs::write(
        &runner_path,
        r#"import { homeboyRunBenchPhase } from './bench-helper.mjs';

const result = await homeboyRunBenchPhase('install', process.execPath, ['-e', 'setTimeout(() => {}, 25)'], {
    sampleIntervalMs: 10,
});
if (result.code !== 0) throw new Error(`phase command failed: ${result.code}`);
"#,
    )
    .expect("write runner");

    let output = std::process::Command::new("node")
        .arg(&runner_path)
        .env("HOMEBOY_RUN_DIR", &run_dir)
        .output()
        .expect("run node");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let children_dir = run_dir.join("extension-children");
    let child_file = std::fs::read_dir(&children_dir)
        .expect("children dir")
        .flatten()
        .find(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .expect("phase resource file")
        .path();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(child_file).expect("read child resource"))
            .expect("json");
    assert_eq!(value["phase"], "install");
    assert!(value["command_label"].as_str().is_some());
    assert!(value["samples"]
        .as_array()
        .is_some_and(|samples| !samples.is_empty()));
    assert_eq!(value["samples"][0]["phase"], "install");
}

#[test]
fn bench_php_helper_documents_inventory_selection_and_artifact_helpers() {
    assert!(assets::BENCH_HELPER_PHP.contains("function homeboy_bench_scenario_selected"));
    assert!(assets::BENCH_HELPER_PHP.contains("function homeboy_bench_scenario_inventory_envelope"));
    assert!(assets::BENCH_HELPER_PHP.contains("function homeboy_bench_artifact_ref"));
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
homeboyBenchProgress({ scenario: 'studio-agent-site-build', run: 'bfb', elapsed_ms: 252000, turn: 18, tools: 23, last: 'cli page_update' });
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
        "studio-agent-site-build [bfb] 04:12 turn=18 tools=23 last=cli page_update\n"
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
