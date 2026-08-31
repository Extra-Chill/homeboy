//! Shared, bounded contract for declared test execution.
//!
//! This deliberately starts with the one policy every adapter can enforce: a
//! positive suite deadline. Adapters add per-test limits, isolation, output,
//! and concurrency only when they can enforce and report them.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Environment setting understood by the hermetic Cargo test runner.
pub const SUITE_TIMEOUT_ENV: &str = "HOMEBOY_TEST_TIMEOUT_SECONDS";

/// Conservative bound for direct `cargo test` calls that do not have a typed
/// owner. Product entry points should construct a plan explicitly.
pub const DEFAULT_SUITE_TIMEOUT_SECONDS: u64 = 25 * 60;

/// The minimal product-agnostic contract shared by declared test adapters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestExecutionPlan {
    pub adapter: TestExecutionAdapter,
    pub command: Vec<String>,
    #[serde(default)]
    pub scope: TestExecutionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_test_timeout_seconds: Option<u64>,
    pub suite_timeout_seconds: u64,
    #[serde(default)]
    pub isolation: TestExecutionIsolation,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_output_budget_bytes")]
    pub output_budget_bytes: usize,
    #[serde(default)]
    pub cleanup_policy: TestExecutionCleanupPolicy,
}

/// The declared adapter responsible for interpreting the test command.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionAdapter {
    /// Homeboy's extension-aware test adapter.
    #[default]
    HomeboyReviewTest,
}

/// The intended population a declared adapter should test.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionScope {
    #[default]
    All,
    Changed,
}

/// Process containment required for a declared test execution.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionIsolation {
    #[default]
    PrivateProcessGroup,
}

/// Cleanup the executor can both enforce and report.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionCleanupPolicy {
    #[default]
    TerminateProcessGroup,
}

fn default_concurrency() -> usize {
    1
}

fn default_output_budget_bytes() -> usize {
    64 * 1024
}

impl TestExecutionPlan {
    /// A suite deadline must be positive. Zero is never an implicit request for
    /// unbounded execution.
    pub fn with_suite_timeout(suite_timeout: Duration) -> Result<Self, &'static str> {
        if suite_timeout.is_zero() {
            return Err("test suite timeout must be greater than zero");
        }
        Ok(Self {
            adapter: TestExecutionAdapter::HomeboyReviewTest,
            command: Vec::new(),
            scope: TestExecutionScope::All,
            per_test_timeout_seconds: None,
            suite_timeout_seconds: suite_timeout.as_secs(),
            isolation: TestExecutionIsolation::PrivateProcessGroup,
            concurrency: default_concurrency(),
            output_budget_bytes: default_output_budget_bytes(),
            cleanup_policy: TestExecutionCleanupPolicy::TerminateProcessGroup,
        })
    }

    /// Build a declared plan. The command is argv rather than shell source so
    /// declarations remain reviewable before an executor projects them.
    pub fn declared(
        command: Vec<String>,
        suite_timeout_seconds: u64,
    ) -> Result<Self, &'static str> {
        if command.is_empty() {
            return Err("test command must not be empty");
        }
        Self::with_suite_timeout(Duration::from_secs(suite_timeout_seconds)).map(|mut plan| {
            plan.command = command;
            plan
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.command.is_empty() {
            return Err("test command must not be empty");
        }
        if self.suite_timeout_seconds == 0 {
            return Err("test suite timeout must be greater than zero");
        }
        if self.per_test_timeout_seconds == Some(0) {
            return Err("per-test timeout must be greater than zero");
        }
        if self.concurrency == 0 {
            return Err("test concurrency must be greater than zero");
        }
        if self.output_budget_bytes == 0 {
            return Err("test output budget must be greater than zero");
        }
        Ok(())
    }

    pub fn suite_timeout(&self) -> Duration {
        Duration::from_secs(self.suite_timeout_seconds)
    }

    /// Pass the resolved deadline to an adapter that uses the global Cargo
    /// runner. This prevents that runner from silently choosing a second policy.
    pub fn suite_timeout_env(&self) -> (&'static str, String) {
        (SUITE_TIMEOUT_ENV, self.suite_timeout_seconds.to_string())
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

    #[test]
    fn declared_plan_carries_only_executor_enforceable_policy() {
        let plan = TestExecutionPlan::declared(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            42,
        )
        .unwrap();
        assert_eq!(plan.adapter, TestExecutionAdapter::HomeboyReviewTest);
        assert_eq!(plan.scope, TestExecutionScope::All);
        assert_eq!(plan.concurrency, 1);
        assert_eq!(plan.output_budget_bytes, 64 * 1024);
        assert_eq!(
            plan.cleanup_policy,
            TestExecutionCleanupPolicy::TerminateProcessGroup
        );
        assert!(TestExecutionPlan::declared(Vec::new(), 42).is_err());
        assert!(plan.validate().is_ok());
    }
}
