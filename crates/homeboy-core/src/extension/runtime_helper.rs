use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::paths;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

mod assets;

pub const RUNNER_STEPS_ENV: &str = "HOMEBOY_RUNTIME_RUNNER_STEPS";
pub const RUNNER_PRELUDE_ENV: &str = "HOMEBOY_RUNTIME_RUNNER_PRELUDE";
pub const COMMAND_CAPTURE_ENV: &str = "HOMEBOY_RUNTIME_COMMAND_CAPTURE";
pub const BASH_PREFLIGHT_ENV: &str = "HOMEBOY_RUNTIME_BASH_PREFLIGHT";
pub const FAILURE_TRAP_ENV: &str = "HOMEBOY_RUNTIME_FAILURE_TRAP";
pub const WRITE_TEST_RESULTS_ENV: &str = "HOMEBOY_RUNTIME_WRITE_TEST_RESULTS";
pub const EMIT_LINT_FINDING_ENV: &str = "HOMEBOY_RUNTIME_EMIT_LINT_FINDING";
pub const EMIT_TEST_FAILURE_ENV: &str = "HOMEBOY_RUNTIME_EMIT_TEST_FAILURE";
pub const SIDECAR_WRITER_ENV: &str = "HOMEBOY_RUNTIME_SIDECAR_WRITER";
pub const RESOLVE_CONTEXT_ENV: &str = "HOMEBOY_RUNTIME_RESOLVE_CONTEXT";
pub const DISPOSABLE_LOCAL_DB_ENV: &str = "HOMEBOY_RUNTIME_DISPOSABLE_LOCAL_DB";
pub const BENCH_HELPER_SH_ENV: &str = "HOMEBOY_RUNTIME_BENCH_HELPER_SH";
pub const BENCH_HELPER_JS_ENV: &str = "HOMEBOY_RUNTIME_BENCH_HELPER_JS";

struct RuntimeHelper {
    filename: &'static str,
    content: &'static str,
    env_var: &'static str,
}

const HELPERS: &[RuntimeHelper] = &[
    RuntimeHelper {
        filename: "runner-steps.sh",
        content: assets::RUNNER_STEPS_SH,
        env_var: RUNNER_STEPS_ENV,
    },
    RuntimeHelper {
        filename: "runner-prelude.sh",
        content: assets::RUNNER_PRELUDE_SH,
        env_var: RUNNER_PRELUDE_ENV,
    },
    RuntimeHelper {
        filename: "command-capture.sh",
        content: assets::COMMAND_CAPTURE_SH,
        env_var: COMMAND_CAPTURE_ENV,
    },
    RuntimeHelper {
        filename: "bash-preflight.sh",
        content: assets::BASH_PREFLIGHT_SH,
        env_var: BASH_PREFLIGHT_ENV,
    },
    RuntimeHelper {
        filename: "failure-trap.sh",
        content: assets::FAILURE_TRAP_SH,
        env_var: FAILURE_TRAP_ENV,
    },
    RuntimeHelper {
        filename: "write-test-results.sh",
        content: assets::WRITE_TEST_RESULTS_SH,
        env_var: WRITE_TEST_RESULTS_ENV,
    },
    RuntimeHelper {
        filename: "emit-lint-finding.sh",
        content: assets::EMIT_LINT_FINDING_SH,
        env_var: EMIT_LINT_FINDING_ENV,
    },
    RuntimeHelper {
        filename: "emit-test-failure.sh",
        content: assets::EMIT_TEST_FAILURE_SH,
        env_var: EMIT_TEST_FAILURE_ENV,
    },
    RuntimeHelper {
        filename: "sidecar-writer.sh",
        content: assets::SIDECAR_WRITER_SH,
        env_var: SIDECAR_WRITER_ENV,
    },
    RuntimeHelper {
        filename: "resolve-context.sh",
        content: assets::RESOLVE_CONTEXT_SH,
        env_var: RESOLVE_CONTEXT_ENV,
    },
    RuntimeHelper {
        filename: "disposable-local-db.sh",
        content: assets::DISPOSABLE_LOCAL_DB_SH,
        env_var: DISPOSABLE_LOCAL_DB_ENV,
    },
    RuntimeHelper {
        filename: "bench-helper.sh",
        content: assets::BENCH_HELPER_SH,
        env_var: BENCH_HELPER_SH_ENV,
    },
    RuntimeHelper {
        filename: "bench-helper.mjs",
        content: assets::BENCH_HELPER_JS,
        env_var: BENCH_HELPER_JS_ENV,
    },
];

#[derive(Debug, Deserialize)]
struct DeclaredRuntimeHelper {
    filename: String,
    source: String,
    env_var: String,
}

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

fn ensure_declared_helper(
    runtime_dir: &std::path::Path,
    helper: &DeclaredRuntimeHelper,
) -> Result<PathBuf> {
    let source = PathBuf::from(&helper.source);
    let content = fs::read_to_string(&source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read declared runtime helper {}", source.display())),
        )
    })?;
    let helper_path = runtime_dir.join(&helper.filename);
    let current = fs::read_to_string(&helper_path).ok();
    if current.as_deref() != Some(content.as_str()) {
        local_files::write_file_atomic(
            &helper_path,
            &content,
            &format!("write declared runtime helper {}", helper.filename),
        )?;
    }
    Ok(helper_path)
}

fn declared_helpers() -> Result<Vec<DeclaredRuntimeHelper>> {
    let Ok(raw) = env::var("HOMEBOY_RUNTIME_HELPERS_JSON") else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_argument(
            "HOMEBOY_RUNTIME_HELPERS_JSON",
            format!("declared runtime helpers JSON is invalid: {e}"),
            None,
            None,
        )
    })
}

/// Ensure all runtime helpers are written and return (env_var, path) pairs.
pub fn ensure_all_helpers() -> Result<Vec<(String, String)>> {
    let runtime_dir = paths::homeboy()
        .map(|path| path.join("runtime"))
        .unwrap_or_else(|_| env::temp_dir().join("homeboy-runtime"));
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

    for helper in declared_helpers()? {
        let path = ensure_declared_helper(&runtime_dir, &helper)?;
        env_pairs.push((helper.env_var, path.to_string_lossy().to_string()));
    }

    Ok(env_pairs)
}

pub fn helper_path(name: &str) -> Result<PathBuf> {
    let normalized = name.trim();
    let pairs = ensure_all_helpers()?;
    let path = pairs
        .into_iter()
        .find_map(|(key, path)| {
            let filename = PathBuf::from(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string);
            (normalized == key || filename.as_deref() == Some(normalized)).then(|| PathBuf::from(path))
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "helper",
                format!("unknown runtime helper `{normalized}`"),
                None,
                Some(vec![
                    "Known helpers are built-in helpers plus HOMEBOY_RUNTIME_HELPERS_JSON declarations".to_string(),
                ]),
            )
        })?;

    if !path.is_file() {
        return Err(Error::internal_unexpected(format!(
            "runtime helper `{normalized}` path does not exist: {}",
            path.display()
        )));
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    include!("runtime_helper/tests.rs");
}
