//! Test baseline adapter: ratchet on pass/fail counts (#15840).
//!
//! Unlike item-based kinds, the test snapshot is the aggregate pass/fail/skip
//! counts, stored under `homeboy.json` → `baselines.test` by the shared
//! baseline / compare / gate engine. The gate regresses when
//! `passed < baseline.passed` or `failed > baseline.failed`.

use homeboy_engine_primitives::workload_compare::{KindAdapter, Verdict};
pub use homeboy_extension_contract::test_result::TestCounts;
pub use homeboy_extension_contract::test_workflow::TestBaselineComparison;

/// The test-counts [`KindAdapter`].
pub struct TestAdapter;

impl KindAdapter for TestAdapter {
    const KIND: &'static str = "test";
    type Source = TestCounts;
    type Snapshot = TestCounts;
    type Options = ();
    type Delta = TestBaselineComparison;

    fn snapshot(counts: &TestCounts) -> TestCounts {
        counts.clone()
    }

    fn delta(baseline: &TestCounts, current: &TestCounts, _: &()) -> TestBaselineComparison {
        let passed_delta = current.passed as i64 - baseline.passed as i64;
        let failed_delta = current.failed as i64 - baseline.failed as i64;

        let mut reasons = Vec::new();
        if current.passed < baseline.passed {
            reasons.push(format!(
                "Passing tests decreased: {} → {} ({})",
                baseline.passed, current.passed, passed_delta
            ));
        }
        if current.failed > baseline.failed {
            reasons.push(format!(
                "Failing tests increased: {} → {} (+{})",
                baseline.failed, current.failed, failed_delta
            ));
        }

        TestBaselineComparison {
            baseline: baseline.clone(),
            current: current.clone(),
            passed_delta,
            failed_delta,
            regression: !reasons.is_empty(),
            reasons,
        }
    }

    fn gate(comparison: &TestBaselineComparison) -> Verdict {
        let mut improvements = Vec::new();
        if comparison.passed_delta > 0 {
            improvements.push("passing tests increased".to_string());
        }
        if comparison.failed_delta < 0 {
            improvements.push("failing tests decreased".to_string());
        }
        Verdict {
            regressions: comparison.reasons.clone(),
            advisories: Vec::new(),
            improvements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_engine_primitives::workload_compare::{self as engine, BaselineSlot};
    use std::path::{Path, PathBuf};

    type TestBaseline = homeboy_engine_primitives::baseline::Baseline<TestCounts>;

    // Storage and the compare entry point now belong to the shared engine.
    // These keep every test below pinned to the same observable behaviour.
    fn save_baseline(
        root: &Path,
        component_id: &str,
        counts: &TestCounts,
    ) -> homeboy_core::Result<PathBuf> {
        engine::save_baseline::<TestAdapter>(&BaselineSlot::new(root, None), component_id, counts)
    }

    fn load_baseline(root: &Path) -> Option<TestBaseline> {
        engine::load_baseline::<TestAdapter>(&BaselineSlot::new(root, None))
    }

    fn compare(current: &TestCounts, baseline: &TestBaseline) -> TestBaselineComparison {
        TestAdapter::delta(&baseline.metadata, current, &())
    }

    fn counts(total: u64, passed: u64, failed: u64, skipped: u64) -> TestCounts {
        TestCounts::new(total, passed, failed, skipped)
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let counts = counts(100, 80, 15, 5);

        save_baseline(dir.path(), "sample-plugin", &counts).unwrap();
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.context_id, "sample-plugin");
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
