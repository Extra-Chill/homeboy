//! Test baseline — ratchet for test pass/fail counts.
//!
//! Unlike item-based baselines, the test baseline tracks aggregate pass/fail/skip
//! counts. The ratchet check is:
//! - `passed >= baseline.passed`
//! - `failed <= baseline.failed`

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::engine::baseline::{self as generic, BaselineConfig};
use crate::error::Result;
use crate::core::code_audit::baseline::load_baseline_from_ref;

const BASELINE_KEY: &str = "test";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestCounts {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl TestCounts {
    pub fn new(total: u64, passed: u64, failed: u64, skipped: u64) -> Self {
        Self {
            total,
            passed,
            failed,
            skipped,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TestBaselineComparison {
    pub baseline: TestCounts,
    pub current: TestCounts,
    pub passed_delta: i64,
    pub failed_delta: i64,
    pub regression: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

pub type TestBaseline = generic::Baseline<TestCounts>;

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    counts: &TestCounts,
) -> Result<std::path::PathBuf> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    let empty: Vec<EmptyItem> = Vec::new();
    generic::save(&config, component_id, &empty, counts.clone())
}

pub fn load_baseline(source_path: &Path) -> Option<TestBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<TestCounts>(&config).ok().flatten()
}

pub fn compare(current: &TestCounts, baseline: &TestBaseline) -> TestBaselineComparison {
    let baseline_counts = &baseline.metadata;

    let passed_delta = current.passed as i64 - baseline_counts.passed as i64;
    let failed_delta = current.failed as i64 - baseline_counts.failed as i64;

    let mut reasons = Vec::new();

    if current.passed < baseline_counts.passed {
        reasons.push(format!(
            "Passing tests decreased: {} → {} ({})",
            baseline_counts.passed, current.passed, passed_delta
        ));
    }

    if current.failed > baseline_counts.failed {
        reasons.push(format!(
            "Failing tests increased: {} → {} (+{})",
            baseline_counts.failed, current.failed, failed_delta
        ));
    }

    TestBaselineComparison {
        baseline: baseline_counts.clone(),
        current: current.clone(),
        passed_delta,
        failed_delta,
        regression: !reasons.is_empty(),
        reasons,
    }
}

struct EmptyItem;

impl generic::Fingerprintable for EmptyItem {
    fn fingerprint(&self) -> String {
        String::new()
    }
    fn description(&self) -> String {
        String::new()
    }
    fn context_label(&self) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(total: u64, passed: u64, failed: u64, skipped: u64) -> TestCounts {
        TestCounts::new(total, passed, failed, skipped)
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let counts = counts(100, 80, 15, 5);

        save_baseline(dir.path(), "data-machine", &counts).unwrap();
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.context_id, "data-machine");
        assert_eq!(loaded.metadata, counts);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path()).is_none());
    }

    #[test]
    fn compare_no_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(100, 80, 15, 5);
        let result = compare(&current, &baseline);
        assert!(!result.regression);
        assert_eq!(result.passed_delta, 0);
        assert_eq!(result.failed_delta, 0);
        assert!(result.reasons.is_empty());
    }

    #[test]
    fn compare_improvement_is_not_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(100, 90, 5, 5);
        let result = compare(&current, &baseline);
        assert!(!result.regression);
        assert_eq!(result.passed_delta, 10);
        assert_eq!(result.failed_delta, -10);
    }
}
