use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::paths;
use std::fs;
use std::path::PathBuf;

const RUNNER_STEPS_SH: &str = include_str!("runtime/runner-steps.sh");
const FAILURE_TRAP_SH: &str = include_str!("runtime/failure-trap.sh");
const WRITE_TEST_RESULTS_SH: &str = include_str!("runtime/write-test-results.sh");

pub const RUNNER_STEPS_ENV: &str = "HOMEBOY_RUNTIME_RUNNER_STEPS";
pub const FAILURE_TRAP_ENV: &str = "HOMEBOY_RUNTIME_FAILURE_TRAP";
pub const WRITE_TEST_RESULTS_ENV: &str = "HOMEBOY_RUNTIME_WRITE_TEST_RESULTS";

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
    }

    #[test]
    fn test_ensure_all_helpers_some_create_homeboy_runtime_directory_to_string() {

        let _result = ensure_all_helpers();
    }

    #[test]
    fn test_ensure_all_helpers_default_path() {

        let _result = ensure_all_helpers();
    }

    #[test]
    fn test_ensure_all_helpers_default_path_2() {

        let _result = ensure_all_helpers();
    }

    #[test]
    fn test_ensure_all_helpers_ok_env_pairs() {

        let result = ensure_all_helpers();
        assert!(result.is_ok(), "expected Ok for: Ok(env_pairs)");
    }

    #[test]
    fn test_ensure_all_helpers_has_expected_effects() {
        // Expected effects: mutation

        let _ = ensure_all_helpers();
    }

}
