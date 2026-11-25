//! Test baseline adapter: ratchet on pass/fail counts (#15840).
//!
//! Unlike item-based kinds, the test snapshot is the aggregate pass/fail/skip
//! counts, stored under `homeboy.json` → `baselines.test` by the shared
//! baseline / compare / gate engine. The gate regresses when
//! `passed < baseline.passed` or `failed > baseline.failed`.

use homeboy_engine_primitives::metric_comparison::Verdict;
use homeboy_engine_primitives::workload_compare::KindAdapter;
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

    fn counts(total: u64, passed: u64, failed: u64, skipped: u64) -> TestCounts {
        TestCounts::new(total, passed, failed, skipped)
    }

    #[test]
    fn baseline_round_trips_through_the_test_slot() {
        let dir = tempfile::tempdir().unwrap();
        let slot = BaselineSlot::new(dir.path(), None);
        assert!(engine::load_baseline::<TestAdapter>(&slot).is_none());

        let counts = counts(100, 80, 15, 5);
        engine::save_baseline::<TestAdapter>(&slot, "sample-plugin", &counts).unwrap();
        let loaded = engine::load_baseline::<TestAdapter>(&slot).unwrap();

        assert_eq!(loaded.context_id, "sample-plugin");
        assert_eq!(loaded.metadata, counts);
    }

    #[test]
    fn delta_regresses_only_on_fewer_passes_or_more_failures() {
        let baseline = counts(100, 80, 15, 5);
        // (current, regression, passed_delta, failed_delta, reason_count)
        let cases = [
            (counts(100, 80, 15, 5), false, 0, 0, 0),
            (counts(100, 90, 5, 5), false, 10, -10, 0),
            (counts(100, 75, 15, 10), true, -5, 0, 1),
            (counts(105, 80, 20, 5), true, 0, 5, 1),
            (counts(100, 70, 25, 5), true, -10, 10, 2),
        ];
        for (current, regression, passed_delta, failed_delta, reasons) in cases {
            let result = TestAdapter::delta(&baseline, &current, &());
            assert_eq!(result.regression, regression, "{current:?}");
            assert_eq!(result.passed_delta, passed_delta, "{current:?}");
            assert_eq!(result.failed_delta, failed_delta, "{current:?}");
            assert_eq!(result.reasons.len(), reasons, "{current:?}");
        }
    }
}
