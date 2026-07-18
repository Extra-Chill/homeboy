//! Pure test-analysis contract types (failure clustering + categorization).

use serde::{Deserialize, Serialize};

/// A single test failure parsed from test runner output.
///
/// This is domain input for failure clustering and analysis. Persisted and
/// command-output finding records should project through `HomeboyFinding`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    /// Fully qualified test name (e.g., "Namespace\\ClassTest::testMethod").
    pub test_name: String,
    /// Test file path relative to component root.
    pub test_file: String,
    /// Error/failure type (e.g., "Error", "PHPUnit\\Framework\\AssertionFailedError").
    pub error_type: String,
    /// Error message.
    pub message: String,
    /// Optional: the source file in the stack trace (deepest non-test frame).
    #[serde(default)]
    pub source_file: String,
    /// Optional: source line number.
    #[serde(default)]
    pub source_line: u32,
}

/// Full test analysis input from extension.
///
/// This preserves structured test-runner facts so analysis can cluster related
/// failures before the findings layer emits canonical `HomeboyFinding` values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestAnalysisInput {
    /// All test failures.
    pub failures: Vec<TestFailure>,
    /// Total tests run.
    #[serde(default)]
    pub total: u64,
    /// Total passed.
    #[serde(default)]
    pub passed: u64,
}

/// A cluster of test failures sharing a common root cause.
#[derive(Debug, Clone, Serialize)]
pub struct FailureCluster {
    /// Cluster identifier (derived from the pattern).
    pub id: String,
    /// Human-readable pattern description.
    pub pattern: String,
    /// Category of the failure pattern.
    pub category: FailureCategory,
    /// Number of failures in this cluster.
    pub count: usize,
    /// Test files affected.
    pub affected_files: Vec<String>,
    /// Representative test names (first few).
    pub example_tests: Vec<String>,
    /// Suggested fix if pattern is recognized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Category of a failure cluster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Method/function doesn't exist.
    MissingMethod,
    /// Class not found.
    MissingClass,
    /// Wrong return type (expected X, got Y).
    ReturnTypeChange,
    /// Wrong error code or message.
    ErrorCodeChange,
    /// Assertion mismatch (expected vs actual).
    AssertionMismatch,
    /// Mock/stub configuration error.
    MockError,
    /// Fatal error (crash, redeclare, etc.).
    FatalError,
    /// Argument count or type mismatch.
    SignatureChange,
    /// Database or environment issue.
    EnvironmentError,
    /// Uncategorized failure.
    Other,
}

/// Full analysis output.
#[derive(Debug, Clone, Serialize)]
pub struct TestAnalysis {
    /// Component that was analyzed.
    pub component: String,
    /// Total test failures.
    pub total_failures: usize,
    /// Total tests run.
    pub total_tests: u64,
    /// Total passing.
    pub total_passed: u64,
    /// Failure clusters, sorted by count (largest first).
    pub clusters: Vec<FailureCluster>,
    /// Human-readable hints.
    pub hints: Vec<String>,
}
