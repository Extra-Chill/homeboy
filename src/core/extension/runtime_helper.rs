use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::paths;
use std::env;
use std::fs;
use std::path::PathBuf;

mod assets;

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
    legacy_fallback: bool,
}

const HELPERS: &[RuntimeHelper] = &[
    RuntimeHelper {
        filename: "runner-steps.sh",
        content: assets::RUNNER_STEPS_SH,
        env_var: RUNNER_STEPS_ENV,
        legacy_fallback: false,
    },
    RuntimeHelper {
        filename: "failure-trap.sh",
        content: assets::FAILURE_TRAP_SH,
        env_var: FAILURE_TRAP_ENV,
        legacy_fallback: false,
    },
    RuntimeHelper {
        filename: "write-test-results.sh",
        content: assets::WRITE_TEST_RESULTS_SH,
        env_var: WRITE_TEST_RESULTS_ENV,
        legacy_fallback: false,
    },
    RuntimeHelper {
        filename: "resolve-context.sh",
        content: assets::RESOLVE_CONTEXT_SH,
        env_var: RESOLVE_CONTEXT_ENV,
        legacy_fallback: false,
    },
    RuntimeHelper {
        filename: "bench-helper.sh",
        content: assets::BENCH_HELPER_SH,
        env_var: BENCH_HELPER_SH_ENV,
        legacy_fallback: true,
    },
    RuntimeHelper {
        filename: "bench-helper.mjs",
        content: assets::BENCH_HELPER_JS,
        env_var: BENCH_HELPER_JS_ENV,
        legacy_fallback: true,
    },
    RuntimeHelper {
        filename: "bench-helper.php",
        content: assets::BENCH_HELPER_PHP,
        env_var: BENCH_HELPER_PHP_ENV,
        legacy_fallback: true,
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

#[cfg(not(windows))]
fn legacy_runtime_dir() -> Result<Option<PathBuf>> {
    let home = env::var("HOME").map_err(|_| {
        Error::internal_unexpected("HOME environment variable not set on Unix-like system")
    })?;
    Ok(Some(PathBuf::from(home).join(".homeboy").join("runtime")))
}

#[cfg(windows)]
fn legacy_runtime_dir() -> Result<Option<PathBuf>> {
    Ok(None)
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

    let legacy_runtime_dir = legacy_runtime_dir()?;
    if let Some(ref legacy_dir) = legacy_runtime_dir {
        fs::create_dir_all(legacy_dir).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("create legacy homeboy runtime directory".to_string()),
            )
        })?;
    }

    let mut env_pairs = Vec::with_capacity(HELPERS.len());
    for helper in HELPERS {
        let path = ensure_helper(&runtime_dir, helper)?;
        if helper.legacy_fallback {
            if let Some(ref legacy_dir) = legacy_runtime_dir {
                ensure_helper(legacy_dir, helper)?;
            }
        }
        env_pairs.push((
            helper.env_var.to_string(),
            path.to_string_lossy().to_string(),
        ));
    }

    Ok(env_pairs)
}

#[cfg(test)]
mod tests {
    include!("runtime_helper/tests.rs");
}

#[cfg(test)]
#[path = "../../../tests/core/extension/runtime_helper_test.rs"]
mod runtime_helper_test;
