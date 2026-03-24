//! Run directory — single coordination point for pipeline step I/O.
//!
//! Each pipeline run gets a directory where steps write their outputs
//! and read predecessor outputs. This replaces the ad-hoc pattern of
//! creating random temp files and passing their paths via individual
//! `HOMEBOY_*_FILE` environment variables.
//!
//! ## Layout
//!
//! ```text
//! {run_dir}/
//!   lint-findings.json     ← lint step output
//!   test-results.json      ← test step output
//!   test-failures.json     ← test failure details
//!   coverage.json          ← test coverage data
//!   fix-plan.json          ← planned fixes (autofix)
//!   fix-results.json       ← applied fixes (autofix)
//!   annotations/           ← CI annotation files
//! ```
//!
//! ## Backward compatibility
//!
//! During migration, homeboy sets both `HOMEBOY_RUN_DIR` and the legacy
//! per-file env vars (e.g. `HOMEBOY_LINT_FINDINGS_FILE`). The legacy vars
//! point into the run dir, so extension scripts that use the old vars
//! continue working. New scripts can use `HOMEBOY_RUN_DIR` directly.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Well-known filenames for step outputs within a run directory.
pub mod files {
    pub const LINT_FINDINGS: &str = "lint-findings.json";
    pub const TEST_RESULTS: &str = "test-results.json";
    pub const TEST_FAILURES: &str = "test-failures.json";
    pub const COVERAGE: &str = "coverage.json";
    pub const FIX_PLAN: &str = "fix-plan.json";
    pub const FIX_RESULTS: &str = "fix-results.json";
    pub const ANNOTATIONS_DIR: &str = "annotations";
}

/// Environment variable name for the run directory.
pub const RUN_DIR_ENV: &str = "HOMEBOY_RUN_DIR";

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
        let path = super::temp::runtime_temp_dir("homeboy-run")?;
        // Create annotations subdirectory
        let annotations = path.join(files::ANNOTATIONS_DIR);
        std::fs::create_dir_all(&annotations).map_err(|e| {
            Error::internal_io(e.to_string(), Some("create annotations dir".to_string()))
        })?;
        Ok(Self { path })
    }

    /// Wrap an existing directory as a run dir (e.g. from `HOMEBOY_RUN_DIR` env var).
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
    /// Returns `(key, value)` pairs that map the legacy `HOMEBOY_*_FILE`
    /// env vars to files within this run directory. Extension scripts that
    /// still use the old vars will read/write the correct locations.
    pub fn legacy_env_vars(&self) -> Vec<(String, String)> {
        vec![
            (
                "HOMEBOY_RUN_DIR".to_string(),
                self.path.to_string_lossy().to_string(),
            ),
            (
                "HOMEBOY_LINT_FINDINGS_FILE".to_string(),
                self.step_file(files::LINT_FINDINGS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_TEST_RESULTS_FILE".to_string(),
                self.step_file(files::TEST_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_TEST_FAILURES_FILE".to_string(),
                self.step_file(files::TEST_FAILURES)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_COVERAGE_FILE".to_string(),
                self.step_file(files::COVERAGE)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_FIX_PLAN_FILE".to_string(),
                self.step_file(files::FIX_PLAN)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_FIX_RESULTS_FILE".to_string(),
                self.step_file(files::FIX_RESULTS)
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "HOMEBOY_ANNOTATIONS_DIR".to_string(),
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
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir.path().is_dir());
        assert!(run_dir.annotations_dir().is_dir());

        // Well-known paths
        assert!(run_dir
            .step_file(files::LINT_FINDINGS)
            .to_string_lossy()
            .ends_with("lint-findings.json"));
        assert!(run_dir
            .step_file(files::TEST_RESULTS)
            .to_string_lossy()
            .ends_with("test-results.json"));

        // Legacy env vars
        let env_vars = run_dir.legacy_env_vars();
        assert!(env_vars
            .iter()
            .any(|(k, _)| k == "HOMEBOY_RUN_DIR"));
        assert!(env_vars
            .iter()
            .any(|(k, _)| k == "HOMEBOY_LINT_FINDINGS_FILE"));

        // Cleanup
        let path = run_dir.path().to_path_buf();
        run_dir.cleanup();
        assert!(!path.exists());
    }

    #[test]
    fn read_step_output_missing_returns_none() {
        let run_dir = RunDir::create().expect("should create run dir");
        assert!(run_dir.read_step_output(files::LINT_FINDINGS).is_none());
        run_dir.cleanup();
    }

    #[test]
    fn read_step_output_present() {
        let run_dir = RunDir::create().expect("should create run dir");
        let path = run_dir.step_file(files::TEST_RESULTS);
        std::fs::write(&path, r#"{"total":10,"passed":10,"failed":0}"#)
            .expect("write test file");

        let output = run_dir
            .read_step_output(files::TEST_RESULTS)
            .expect("should read");
        assert_eq!(output["total"], 10);
        assert_eq!(output["passed"], 10);

        run_dir.cleanup();
    }

    #[test]
    fn list_outputs() {
        let run_dir = RunDir::create().expect("should create run dir");
        std::fs::write(run_dir.step_file(files::LINT_FINDINGS), "[]").unwrap();
        std::fs::write(run_dir.step_file(files::TEST_RESULTS), "{}").unwrap();

        let outputs = run_dir.list_outputs();
        assert!(outputs.contains(&"lint-findings.json".to_string()));
        assert!(outputs.contains(&"test-results.json".to_string()));

        run_dir.cleanup();
    }
}
