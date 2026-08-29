//! Shared, bounded contract for declared test execution.
//!
//! This deliberately starts with the one policy every adapter can enforce: a
//! positive suite deadline. Adapters add per-test limits, isolation, output,
//! and concurrency only when they can enforce and report them.

use std::time::Duration;

/// Environment setting understood by the hermetic Cargo test runner.
pub const SUITE_TIMEOUT_ENV: &str = "HOMEBOY_TEST_TIMEOUT_SECONDS";

/// Conservative bound for direct `cargo test` calls that do not have a typed
/// owner. Product entry points should construct a plan explicitly.
pub const DEFAULT_SUITE_TIMEOUT_SECONDS: u64 = 25 * 60;

/// The minimal product-agnostic contract shared by declared test adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestExecutionPlan {
    suite_timeout: Duration,
}

impl TestExecutionPlan {
    /// A suite deadline must be positive. Zero is never an implicit request for
    /// unbounded execution.
    pub fn with_suite_timeout(suite_timeout: Duration) -> Result<Self, &'static str> {
        if suite_timeout.is_zero() {
            return Err("test suite timeout must be greater than zero");
        }
        Ok(Self { suite_timeout })
    }

    pub fn suite_timeout(self) -> Duration {
        self.suite_timeout
    }

    /// Pass the resolved deadline to an adapter that uses the global Cargo
    /// runner. This prevents that runner from silently choosing a second policy.
    pub fn suite_timeout_env(self) -> (&'static str, String) {
        (SUITE_TIMEOUT_ENV, self.suite_timeout.as_secs().to_string())
    }
}

/// Resolve the declared review-test deadline. Invalid and zero inherited
/// values retain the safe default; direct runner invocation rejects them so an
/// explicit zero cannot turn a binary into an unbounded process.
pub fn suite_timeout_from_env() -> TestExecutionPlan {
    let seconds = std::env::var(SUITE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_SUITE_TIMEOUT_SECONDS);
    TestExecutionPlan::with_suite_timeout(Duration::from_secs(seconds))
        .expect("default test suite timeout is positive")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_timeout_is_positive_and_exported_for_the_runner() {
        let plan = TestExecutionPlan::with_suite_timeout(Duration::from_secs(42)).unwrap();
        assert_eq!(
            plan.suite_timeout_env(),
            (SUITE_TIMEOUT_ENV, "42".to_string())
        );
        assert!(TestExecutionPlan::with_suite_timeout(Duration::ZERO).is_err());
    }
}
