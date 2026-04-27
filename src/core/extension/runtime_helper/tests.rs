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
fn bench_runtime_helpers_document_shared_contract() {
    for content in [assets::BENCH_HELPER_JS, assets::BENCH_HELPER_PHP] {
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
