use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::paths;
use std::fs;
use std::path::PathBuf;

const RUNNER_STEPS_SH: &str = include_str!("runtime/runner-steps.sh");
const FAILURE_TRAP_SH: &str = include_str!("runtime/failure-trap.sh");
const WRITE_TEST_RESULTS_SH: &str = include_str!("runtime/write-test-results.sh");
const RESOLVE_CONTEXT_SH: &str = include_str!("runtime/resolve-context.sh");
const BENCH_HELPER_SH: &str = include_str!("runtime/bench-helper.sh");
const BENCH_HELPER_JS: &str = include_str!("runtime/bench-helper.mjs");
const BENCH_HELPER_PHP: &str = include_str!("runtime/bench-helper.php");

pub const RUNNER_STEPS_ENV: &str = "HOMEBOY_RUNTIME_RUNNER_STEPS";
pub const FAILURE_TRAP_ENV: &str = "HOMEBOY_RUNTIME_FAILURE_TRAP";
pub const WRITE_TEST_RESULTS_ENV: &str = "HOMEBOY_RUNTIME_WRITE_TEST_RESULTS";
pub const RESOLVE_CONTEXT_ENV: &str = "HOMEBOY_RUNTIME_RESOLVE_CONTEXT";
pub const BENCH_HELPER_SH_ENV: &str = "HOMEBOY_RUNTIME_BENCH_HELPER_SH";
pub const BENCH_HELPER_JS_ENV: &str = "HOMEBOY_RUNTIME_BENCH_HELPER_JS";
pub const BENCH_HELPER_PHP_ENV: &str = "HOMEBOY_RUNTIME_BENCH_HELPER_PHP";

struct RuntimeHelper {
    filename: &'static str,
    content: &'static str,
    env_var: &'static str,
}

const HELPERS: &[RuntimeHelper] = &[
    RuntimeHelper {
        filename: "runner-steps.sh",
        content: RUNNER_STEPS_SH,
        env_var: RUNNER_STEPS_ENV,
    },
    RuntimeHelper {
        filename: "failure-trap.sh",
        content: FAILURE_TRAP_SH,
        env_var: FAILURE_TRAP_ENV,
    },
    RuntimeHelper {
        filename: "write-test-results.sh",
        content: WRITE_TEST_RESULTS_SH,
        env_var: WRITE_TEST_RESULTS_ENV,
    },
    RuntimeHelper {
        filename: "resolve-context.sh",
        content: RESOLVE_CONTEXT_SH,
        env_var: RESOLVE_CONTEXT_ENV,
    },
    RuntimeHelper {
        filename: "bench-helper.sh",
        content: BENCH_HELPER_SH,
        env_var: BENCH_HELPER_SH_ENV,
    },
    RuntimeHelper {
        filename: "bench-helper.mjs",
        content: BENCH_HELPER_JS,
        env_var: BENCH_HELPER_JS_ENV,
    },
    RuntimeHelper {
        filename: "bench-helper.php",
        content: BENCH_HELPER_PHP,
        env_var: BENCH_HELPER_PHP_ENV,
    },
];

/// Write a single runtime helper to disk if it's missing or stale.
fn ensure_helper(runtime_dir: &std::path::Path, helper: &RuntimeHelper) -> Result<PathBuf> {
    let helper_path = runtime_dir.join(helper.filename);
    let current = fs::read_to_string(&helper_path).ok();

    if current.as_deref() != Some(helper.content) {
        local_files::write_file_atomic(
            &helper_path,
            helper.content,
            &format!("write runtime {} helper", helper.filename),
        )?;
    }

    Ok(helper_path)
}

/// Ensure all runtime helpers are written and return (env_var, path) pairs.
pub fn ensure_all_helpers() -> Result<Vec<(String, String)>> {
    let runtime_dir = paths::homeboy()?.join("runtime");
    fs::create_dir_all(&runtime_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create homeboy runtime directory".to_string()),
        )
    })?;

    let mut env_pairs = Vec::with_capacity(HELPERS.len());
    for helper in HELPERS {
        let path = ensure_helper(&runtime_dir, helper)?;
        env_pairs.push((
            helper.env_var.to_string(),
            path.to_string_lossy().to_string(),
        ));
    }

    Ok(env_pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_all_helpers_writes_all_files() {
        let pairs = ensure_all_helpers().expect("all helpers should be written");
        assert_eq!(pairs.len(), HELPERS.len());

        for (i, (env_var, path)) in pairs.iter().enumerate() {
            assert_eq!(env_var, HELPERS[i].env_var);
            let contents = std::fs::read_to_string(path).expect("helper should be readable");
            assert_eq!(contents, HELPERS[i].content);
        }

        // Verify specific helpers are present
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
    }

    #[test]
    fn resolve_context_helper_exports_homeboy_env_and_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("resolve-context.sh");
        std::fs::write(&helper_path, RESOLVE_CONTEXT_SH).expect("write helper");

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
        std::fs::write(&helper_path, RESOLVE_CONTEXT_SH).expect("write helper");

        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "cd {}; source {}; SCRIPT_DIR={}; homeboy_resolve_context; printf '%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$COMPONENT_ID\"",
                component_dir.display(),
                helper_path.display(),
                script_dir.display()
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
        std::fs::write(&helper_path, BENCH_HELPER_SH).expect("write helper");

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
        for content in [BENCH_HELPER_JS, BENCH_HELPER_PHP] {
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
}
