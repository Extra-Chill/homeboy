//! Run directory — single coordination point for pipeline step I/O.
//!
//! Each pipeline run gets a directory where steps write their outputs
//! and read predecessor outputs. This replaces the ad-hoc pattern of
//! creating random temp files and passing their paths via individual
//! product-prefixed environment variables.
//!
//! ## Layout
//!
//! ```text
//! {run_dir}/
//!   lint-findings.json     ← lint step output
//!   test-results.json      ← test step output
//!   test-failures.json     ← test failure details
//!   coverage.json          ← test coverage data
//!   fix-results.json       ← applied fixes (autofix)
//!   annotations/           ← CI annotation files
//! ```
//!
//! ## Backward compatibility
//!
//! During migration, the CLI sets both the run-dir env var and the legacy
//! per-file env vars. The legacy vars
//! point into the run dir, so extension scripts that use the old vars
//! continue working. New scripts can use the run-dir env var directly.

use crate::core::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Well-known filenames for step outputs within a run directory.
pub mod files {
    pub const LINT_FINDINGS: &str = "lint-findings.json";
    pub const LINT_PRODUCERS: &str = "lint-producers.json";
    pub const TEST_RESULTS: &str = "test-results.json";
    pub const TEST_FAILURES: &str = "test-failures.json";
    pub const COVERAGE: &str = "coverage.json";
    pub const FIX_RESULTS: &str = "fix-results.json";
    pub const BENCH_RESULTS: &str = "bench-results.json";
    pub const TRACE_RESULTS: &str = "trace.json";
    pub const RESOURCE_SUMMARY: &str = "resource-summary.json";
    pub const EXTENSION_CHILDREN_DIR: &str = "extension-children";
    pub const ANNOTATIONS_DIR: &str = "annotations";
}

/// Environment variable name for the run directory.
pub fn run_dir_env() -> String {
    crate::core::product_identity::PRODUCT_IDENTITY.env_var("RUN_DIR")
}

/// A run directory for a single pipeline execution.
///
/// Created once per `homeboy lint`, `homeboy test`, `homeboy refactor`, etc.
/// Provides well-known paths for step outputs and generates backward-compatible
/// env var mappings for extension scripts.
#[derive(Debug, Clone)]
pub struct RunDir {
    path: PathBuf,
}

impl RunDir {
    /// Create a new run directory under the runtime temp root.
    ///
    /// The directory is created immediately. It persists until the caller
    /// drops or explicitly cleans it up — homeboy's temp pruner handles
    /// orphans from killed processes.
    pub fn create() -> Result<Self> {
        let path = super::temp::runtime_temp_dir(
            crate::core::product_identity::PRODUCT_IDENTITY.run_dir_prefix,
        )?;
        // Create annotations subdirectory
        let annotations = path.join(files::ANNOTATIONS_DIR);
        std::fs::create_dir_all(&annotations).map_err(|e| {
            Error::internal_io(e.to_string(), Some("create annotations dir".to_string()))
        })?;
        Ok(Self { path })
    }

    /// Wrap an existing directory as a run dir.
    pub fn from_existing(path: PathBuf) -> Result<Self> {
        if !path.is_dir() {
            return Err(Error::internal_io(
                format!("run dir does not exist: {}", path.display()),
                Some("open run dir".to_string()),
            ));
        }
        Ok(Self { path })
    }

    /// The root path of this run directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to a well-known step output file.
    pub fn step_file(&self, filename: &str) -> PathBuf {
        self.path.join(filename)
    }

    /// Path to the annotations subdirectory.
    pub fn annotations_dir(&self) -> PathBuf {
        self.path.join(files::ANNOTATIONS_DIR)
    }

    /// Generate backward-compatible env var pairs for extension scripts.
    ///
    /// Returns `(key, value)` pairs that map the legacy product-prefixed
    /// file environment variables
    /// env vars to files within this run directory. Extension scripts that
    /// still use the old vars will read/write the correct locations.
    pub fn legacy_env_vars(&self) -> Vec<(String, String)> {
        vec![
            (run_dir_env(), self.path.to_string_lossy().to_string()),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("LINT_FINDINGS_FILE"),
                self.step_file(files::LINT_FINDINGS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("LINT_PRODUCERS_FILE"),
                self.step_file(files::LINT_PRODUCERS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("TEST_RESULTS_FILE"),
                self.step_file(files::TEST_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("TEST_FAILURES_FILE"),
                self.step_file(files::TEST_FAILURES)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("COVERAGE_FILE"),
                self.step_file(files::COVERAGE)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("FIX_RESULTS_FILE"),
                self.step_file(files::FIX_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("BENCH_RESULTS_FILE"),
                self.step_file(files::BENCH_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("TRACE_RESULTS_FILE"),
                self.step_file(files::TRACE_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                crate::core::product_identity::PRODUCT_IDENTITY.env_var("ANNOTATIONS_DIR"),
                self.annotations_dir().to_string_lossy().to_string(),
            ),
        ]
    }

    /// Read a step output file as a JSON value, returning None if missing.
    pub fn read_step_output(&self, filename: &str) -> Option<serde_json::Value> {
        let path = self.step_file(filename);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// List all step output files present in this run directory.
    pub fn list_outputs(&self) -> Vec<String> {
        let mut outputs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".json") {
                        outputs.push(name.to_string());
                    }
                }
            }
        }
        outputs.sort();
        outputs
    }

    /// Clean up the run directory. Called after the pipeline completes.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_run_dir() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir.path().is_dir());

        // Well-known paths
        assert!(run_dir
            .step_file(files::LINT_FINDINGS)
            .to_string_lossy()
            .ends_with("lint-findings.json"));
        assert!(run_dir
            .step_file(files::TEST_RESULTS)
            .to_string_lossy()
            .ends_with("test-results.json"));

        // Cleanup
        let path = run_dir.path().to_path_buf();
        run_dir.cleanup();
        assert!(!path.exists());
    }

    #[test]
    fn test_path() {
        crate::test_support::with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("should create run dir");
            assert!(run_dir.path().is_dir());
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_annotations_dir() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir.annotations_dir().is_dir());
        run_dir.cleanup();
    }

    #[test]
    fn test_step_file() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir
            .step_file(files::TRACE_RESULTS)
            .to_string_lossy()
            .ends_with("trace.json"));
        run_dir.cleanup();
    }

    #[test]
    fn test_legacy_env_vars() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        let env_vars = run_dir.legacy_env_vars();
        assert!(env_vars.iter().any(|(k, _)| k == "HOMEBOY_RUN_DIR"));
        assert!(env_vars
            .iter()
            .any(|(k, _)| k == "HOMEBOY_TRACE_RESULTS_FILE"));
        run_dir.cleanup();
    }

    #[test]
    fn test_from_existing() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        let path = run_dir.path().to_path_buf();
        let existing = RunDir::from_existing(path.clone()).expect("existing run dir should load");
        assert_eq!(existing.path(), path.as_path());
        existing.cleanup();
    }

    #[test]
    fn test_cleanup() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        let path = run_dir.path().to_path_buf();
        run_dir.cleanup();
        assert!(!path.exists());
    }

    #[test]
    fn read_step_output_missing_returns_none() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir.read_step_output(files::LINT_FINDINGS).is_none());
        run_dir.cleanup();
    }

    #[test]
    fn read_step_output_present() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        let path = run_dir.step_file(files::TEST_RESULTS);
        std::fs::write(&path, r#"{"total":10,"passed":10,"failed":0}"#).expect("write test file");

        let output = run_dir
            .read_step_output(files::TEST_RESULTS)
            .expect("should read");
        assert_eq!(output["total"], 10);
        assert_eq!(output["passed"], 10);

        run_dir.cleanup();
    }

    #[test]
    fn list_outputs() {
        let _guard = crate::test_support::home_env_guard();
        let run_dir = RunDir::create().expect("should create run dir");
        std::fs::write(run_dir.step_file(files::LINT_FINDINGS), "[]").unwrap();
        std::fs::write(run_dir.step_file(files::TEST_RESULTS), "{}").unwrap();

        let outputs = run_dir.list_outputs();
        assert!(outputs.contains(&"lint-findings.json".to_string()));
        assert!(outputs.contains(&"test-results.json".to_string()));

        run_dir.cleanup();
    }
}
