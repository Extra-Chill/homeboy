//! Test baseline — ratchet for test pass/fail counts.
//!
//! Unlike other baselines that track individual findings (Fingerprintable items),
//! the test baseline tracks aggregate pass/fail/skip counts. The ratchet check is:
//! - `passed >= baseline.passed` (can't lose passing tests)
//! - `failed <= baseline.failed` (can't introduce new failures)
//!
//! This lets CI go green with the current test state as a floor, while preventing
//! regressions. Every PR that fixes tests ratchets the baseline forward.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::baseline::{self as generic, BaselineConfig};
use crate::error::Result;

// ============================================================================
// Baseline key
// ============================================================================

/// Key used in `homeboy.json` → `baselines.test`.
const BASELINE_KEY: &str = "test";

// ============================================================================
// Test counts
// ============================================================================

/// Aggregate test results — the data that gets baselined.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestCounts {
    /// Total test count.
    pub total: u64,
    /// Passing tests.
    pub passed: u64,
    /// Failing tests.
    pub failed: u64,
    /// Skipped/incomplete tests.
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

// ============================================================================
// Baseline comparison
// ============================================================================

/// Result of comparing current test counts against a saved baseline.
#[derive(Debug, Clone, Serialize)]
pub struct TestBaselineComparison {
    /// The baseline counts (what we're comparing against).
    pub baseline: TestCounts,
    /// The current counts.
    pub current: TestCounts,
    /// Change in passing tests (positive = more passing).
    pub passed_delta: i64,
    /// Change in failing tests (positive = more failing).
    pub failed_delta: i64,
    /// Whether the ratchet check failed (regression detected).
    pub regression: bool,
    /// Human-readable reasons for regression.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

// ============================================================================
// Stored baseline type
// ============================================================================

/// A saved test baseline snapshot.
pub type TestBaseline = generic::Baseline<TestCounts>;

// ============================================================================
// Public API
// ============================================================================

/// Save the current test counts as a baseline.
pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    counts: &TestCounts,
) -> Result<std::path::PathBuf> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);

    // We store the counts as metadata. The generic baseline expects Fingerprintable
    // items, but for test counts we don't have individual items — just aggregates.
    // We pass an empty item list and rely solely on the metadata for comparison.
    let empty: Vec<EmptyItem> = Vec::new();
    generic::save(&config, component_id, &empty, counts.clone())
}

/// Load a test baseline if one exists.
pub fn load_baseline(source_path: &Path) -> Option<TestBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<TestCounts>(&config).ok().flatten()
}

/// Load a test baseline from a git ref (e.g., `origin/main`).
///
/// Uses `git show <ref>:homeboy.json` to read the baseline without checkout.
/// Returns `None` if the ref doesn't have a test baseline.
pub fn load_baseline_from_ref(source_path: &str, git_ref: &str) -> Option<TestBaseline> {
    generic::load_from_git_ref::<TestCounts>(source_path, git_ref, BASELINE_KEY)
}

/// Compare current test counts against a saved baseline.
///
/// The ratchet rule:
/// - `regression = true` if passed < baseline.passed OR failed > baseline.failed
/// - Improvements (more passes or fewer failures) are tracked but don't fail
pub fn compare(current: &TestCounts, baseline: &TestBaseline) -> TestBaselineComparison {
    let baseline_counts = &baseline.metadata;

    let passed_delta = current.passed as i64 - baseline_counts.passed as i64;
    let failed_delta = current.failed as i64 - baseline_counts.failed as i64;

    let mut reasons = Vec::new();

    if current.passed < baseline_counts.passed {
        reasons.push(format!(
            "Passing tests decreased: {} \u{2192} {} ({})",
            baseline_counts.passed, current.passed, passed_delta
        ));
    }

    if current.failed > baseline_counts.failed {
        reasons.push(format!(
            "Failing tests increased: {} \u{2192} {} (+{})",
            baseline_counts.failed, current.failed, failed_delta
        ));
    }

    let regression = !reasons.is_empty();

    TestBaselineComparison {
        baseline: baseline_counts.clone(),
        current: current.clone(),
        passed_delta,
        failed_delta,
        regression,
        reasons,
    }
}

// ============================================================================
// Internal: empty Fingerprintable for generic save()
// ============================================================================

/// Placeholder for the generic baseline save — test baselines don't use
/// individual items, only aggregate counts in metadata.
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(total: u64, passed: u64, failed: u64, skipped: u64) -> TestCounts {
        TestCounts::new(total, passed, failed, skipped)
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let c = counts(100, 80, 15, 5);

        save_baseline(dir.path(), "data-machine", &c).unwrap();
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.context_id, "data-machine");
        assert_eq!(loaded.metadata, c);
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

    #[test]
    fn compare_fewer_passing_is_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(100, 75, 20, 5);
        let result = compare(&current, &baseline);
        assert!(result.regression);
        assert_eq!(result.passed_delta, -5);
        assert_eq!(result.reasons.len(), 2);
    }

    #[test]
    fn compare_more_failing_is_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(105, 80, 20, 5);
        let result = compare(&current, &baseline);
        assert!(result.regression);
        assert_eq!(result.failed_delta, 5);
        assert_eq!(result.reasons.len(), 1);
        assert!(result.reasons[0].contains("Failing tests increased"));
    }

    #[test]
    fn compare_new_tests_all_passing_is_not_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(120, 100, 15, 5);
        let result = compare(&current, &baseline);
        assert!(!result.regression);
        assert_eq!(result.passed_delta, 20);
        assert_eq!(result.failed_delta, 0);
    }

    #[test]
    fn save_preserves_other_baselines() {
        let dir = tempfile::tempdir().unwrap();

        let audit_config = generic::BaselineConfig::new(dir.path(), "audit");
        let empty: Vec<EmptyItem> = Vec::new();
        generic::save(&audit_config, "test", &empty, ()).unwrap();

        let c = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &c).unwrap();

        let test_baseline = load_baseline(dir.path());
        assert!(test_baseline.is_some());

        let audit_baseline = generic::load::<()>(&audit_config).unwrap();
        assert!(audit_baseline.is_some());
    }

    #[test]
    fn save_overwrites_previous_baseline() {
        let dir = tempfile::tempdir().unwrap();

        let c1 = counts(100, 80, 15, 5);
        save_baseline(dir.path(), "test", &c1).unwrap();

        let c2 = counts(120, 100, 15, 5);
        save_baseline(dir.path(), "test", &c2).unwrap();

        let loaded = load_baseline(dir.path()).unwrap();
        assert_eq!(loaded.metadata, c2);
    }

    #[test]
    fn compare_zero_baseline_any_failure_is_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baseline_counts = counts(50, 50, 0, 0);
        save_baseline(dir.path(), "test", &baseline_counts).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = counts(50, 49, 1, 0);
        let result = compare(&current, &baseline);
        assert!(result.regression);
        assert_eq!(result.reasons.len(), 2);
    }
}
