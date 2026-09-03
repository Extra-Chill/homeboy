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

/// The bounded policy shared by direct and declared test execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestExecutionPlan {
    suite_timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter: Option<TestExecutionAdapter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
}

/// The declared adapter responsible for interpreting the test command.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionAdapter {
    /// Homeboy's extension-aware test adapter. Its argv begins exactly with
    /// `homeboy review test`; shell programs use the explicit gate escape hatch.
    HomeboyReviewTest,
}

/// Terminal result emitted by the executor of a declared test plan. Consumers
/// project this value directly rather than inferring a timeout from output.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestExecutionOutcome {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
}

impl TestExecutionPlan {
    /// A suite deadline must be positive. Zero is never an implicit request for
    /// unbounded execution.
    pub fn with_suite_timeout(suite_timeout: Duration) -> Result<Self, &'static str> {
        if suite_timeout.is_zero() {
            return Err("test suite timeout must be greater than zero");
        }
        Ok(Self {
            suite_timeout_seconds: suite_timeout.as_secs(),
            adapter: None,
            command: None,
        })
    }

    /// Build a declared review-test plan. The command is argv rather than shell
    /// source so the adapter cannot be mislabeled as an arbitrary shell gate.
    pub fn declared_homeboy_review_test(
        command: Vec<String>,
        suite_timeout_seconds: u64,
    ) -> Result<Self, &'static str> {
        Self::with_suite_timeout(Duration::from_secs(suite_timeout_seconds)).and_then(|mut plan| {
            plan.adapter = Some(TestExecutionAdapter::HomeboyReviewTest);
            plan.command = Some(command);
            plan.validate()?;
            Ok(plan)
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.suite_timeout_seconds == 0 {
            return Err("test suite timeout must be greater than zero");
        }
        match (self.adapter, self.command.as_deref()) {
            (None, None) => Ok(()),
            (Some(TestExecutionAdapter::HomeboyReviewTest), Some(command))
                if command.starts_with(&[
                    "homeboy".to_string(),
                    "review".to_string(),
                    "test".to_string(),
                ]) =>
            {
                Ok(())
            }
            (Some(TestExecutionAdapter::HomeboyReviewTest), Some(_)) => {
                Err("homeboy_review_test command must begin with `homeboy review test`")
            }
            _ => Err("declared test plans require both an adapter and command"),
        }
    }

    /// Return the declared argv, rejecting timeout-only plans where a declared
    /// adapter is required.
    pub fn declared_command(&self) -> Result<&[String], &'static str> {
        self.validate()?;
        self.command
            .as_deref()
            .ok_or("declared test plans require an adapter and command")
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
        assert!(plan.validate().is_ok(), "timeout-only plans remain valid");
    }

    #[test]
    fn declared_plan_requires_a_canonical_review_test_argv() {
        let plan = TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            42,
        )
        .unwrap();
        assert_eq!(
            plan.declared_command().unwrap(),
            ["homeboy", "review", "test"]
        );
        assert!(TestExecutionPlan::declared_homeboy_review_test(
            vec!["cargo".to_string(), "test".to_string()],
            42,
        )
        .is_err());
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn declared_plans_reject_policy_the_executor_cannot_enforce() {
        let error = serde_json::from_str::<TestExecutionPlan>(
            r#"{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":42,"scope":"changed"}"#,
        )
        .expect_err("unsupported scope is not silently ignored");
        assert!(error.to_string().contains("scope"));
    }

    #[test]
    fn declared_plan_round_trips_as_a_concrete_durable_contract() {
        let plan = TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
                "component".to_string(),
            ],
            42,
        )
        .unwrap();
        let persisted = serde_json::to_string(&plan).unwrap();
        assert_eq!(
            serde_json::from_str::<TestExecutionPlan>(&persisted).unwrap(),
            plan
        );
    }
}
