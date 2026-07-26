use homeboy_core::error::{Error, Result};
use homeboy_core::paths;
use homeboy_engine_primitives::local_files;
use homeboy_extension_contract::RuntimeHelperRequirement;
use serde::Deserialize;
use sha2::{Digest, Sha256};
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
pub const RUNTIME_SETTINGS_HELPER_ID: &str = "runtime-settings";
pub const RUNTIME_SETTINGS_HELPER_ENV: &str = "HOMEBOY_RUNTIME_SETTINGS_HELPER";

struct RuntimeHelper {
    id: &'static str,
    filename: &'static str,
    content: &'static str,
    env_var: &'static str,
}

const HELPERS: &[RuntimeHelper] = &[
    RuntimeHelper {
        id: "runner-steps",
        filename: "runner-steps.sh",
        content: assets::RUNNER_STEPS_SH,
        env_var: RUNNER_STEPS_ENV,
    },
    RuntimeHelper {
        id: "runner-prelude",
        filename: "runner-prelude.sh",
        content: assets::RUNNER_PRELUDE_SH,
        env_var: RUNNER_PRELUDE_ENV,
    },
    RuntimeHelper {
        id: "command-capture",
        filename: "command-capture.sh",
        content: assets::COMMAND_CAPTURE_SH,
        env_var: COMMAND_CAPTURE_ENV,
    },
    RuntimeHelper {
        id: "bash-preflight",
        filename: "bash-preflight.sh",
        content: assets::BASH_PREFLIGHT_SH,
        env_var: BASH_PREFLIGHT_ENV,
    },
    RuntimeHelper {
        id: "failure-trap",
        filename: "failure-trap.sh",
        content: assets::FAILURE_TRAP_SH,
        env_var: FAILURE_TRAP_ENV,
    },
    RuntimeHelper {
        id: "write-test-results",
        filename: "write-test-results.sh",
        content: assets::WRITE_TEST_RESULTS_SH,
        env_var: WRITE_TEST_RESULTS_ENV,
    },
    RuntimeHelper {
        id: "emit-lint-finding",
        filename: "emit-lint-finding.sh",
        content: assets::EMIT_LINT_FINDING_SH,
        env_var: EMIT_LINT_FINDING_ENV,
    },
    RuntimeHelper {
        id: "emit-test-failure",
        filename: "emit-test-failure.sh",
        content: assets::EMIT_TEST_FAILURE_SH,
        env_var: EMIT_TEST_FAILURE_ENV,
    },
    RuntimeHelper {
        id: "sidecar-writer",
        filename: "sidecar-writer.sh",
        content: assets::SIDECAR_WRITER_SH,
        env_var: SIDECAR_WRITER_ENV,
    },
    RuntimeHelper {
        id: "resolve-context",
        filename: "resolve-context.sh",
        content: assets::RESOLVE_CONTEXT_SH,
        env_var: RESOLVE_CONTEXT_ENV,
    },
    RuntimeHelper {
        id: "disposable-local-db",
        filename: "disposable-local-db.sh",
        content: assets::DISPOSABLE_LOCAL_DB_SH,
        env_var: DISPOSABLE_LOCAL_DB_ENV,
    },
    RuntimeHelper {
        id: "bench-helper-sh",
        filename: "bench-helper.sh",
        content: assets::BENCH_HELPER_SH,
        env_var: BENCH_HELPER_SH_ENV,
    },
    RuntimeHelper {
        id: "bench-helper-js",
        filename: "bench-helper.mjs",
        content: assets::BENCH_HELPER_JS,
        env_var: BENCH_HELPER_JS_ENV,
    },
];

const DECLARABLE_HELPERS: &[RuntimeHelper] = &[RuntimeHelper {
    id: RUNTIME_SETTINGS_HELPER_ID,
    filename: "settings.sh",
    content: assets::SETTINGS_SH,
    env_var: RUNTIME_SETTINGS_HELPER_ENV,
}];

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct RuntimeHelperProvision {
    pub id: String,
    pub env_var: String,
    pub path: String,
    pub revision: String,
    pub source: String,
}

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

fn helper_revision(helper: &RuntimeHelper) -> String {
    format!("sha256:{:x}", Sha256::digest(helper.content.as_bytes()))
}

/// Materialize manifest-declared helpers under an identity-and-revision path.
/// The content-addressed path keeps an admitted extension's helper immutable if
/// another command later refreshes the core runtime helpers.
pub fn provision_declared_helpers(
    requirements: &[RuntimeHelperRequirement],
) -> Result<Vec<RuntimeHelperProvision>> {
    let runtime_dir = paths::homeboy()
        .map(|path| path.join("runtime/helpers"))
        .unwrap_or_else(|_| env::temp_dir().join("homeboy-runtime/helpers"));
    let mut provisions = Vec::with_capacity(requirements.len());
    for requirement in requirements {
        let id = requirement.id.trim();
        let helper = DECLARABLE_HELPERS
            .iter()
            .find(|helper| helper.id == id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runtime_helpers",
                    format!("runtime helper identity `{id}` is not supplied by Homeboy core"),
                    Some(id.to_string()),
                    Some(vec![
                        "Declare a helper identity supported by the installed Homeboy core."
                            .to_string(),
                    ]),
                )
            })?;
        let revision = helper_revision(helper);
        if let Some(expected) = requirement
            .revision
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if expected != revision {
                return Err(Error::validation_invalid_argument(
                    "runtime_helpers.revision",
                    format!("runtime helper `{id}` requires revision `{expected}`, but Homeboy core provides `{revision}`"),
                    Some(expected.to_string()),
                    Some(vec!["Install a compatible Homeboy core or update the extension helper declaration.".to_string()]),
                ));
            }
        }
        let path = runtime_dir.join(id).join(&revision).join(helper.filename);
        if !path.is_file() {
            let parent = path.parent().expect("helper path has a parent");
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "create runtime helper directory {}",
                        parent.display()
                    )),
                )
            })?;
            local_files::write_file_atomic(
                &path,
                helper.content,
                &format!("materialize runtime helper {id}"),
            )?;
        }
        provisions.push(RuntimeHelperProvision {
            id: id.to_string(),
            env_var: helper.env_var.to_string(),
            path: path.to_string_lossy().to_string(),
            revision,
            source: "homeboy-core-embedded".to_string(),
        });
    }
    provisions.sort_by(|left, right| left.id.cmp(&right.id));
    provisions.dedup_by(|left, right| left.id == right.id);
    Ok(provisions)
}

pub fn declared_helper_env_names(requirements: &[RuntimeHelperRequirement]) -> Vec<String> {
    let mut names = requirements
        .iter()
        .filter_map(|requirement| {
            DECLARABLE_HELPERS
                .iter()
                .find(|helper| helper.id == requirement.id.trim())
                .map(|helper| helper.env_var.to_string())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
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
