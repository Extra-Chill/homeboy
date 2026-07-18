//! Pure test drift + run/baseline result contract types.

use serde::{Deserialize, Serialize};

use crate::TestCounts;

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// A production change that may cause test drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionChange {
    /// Type of change detected.
    pub change_type: ChangeType,
    /// Production file where the change occurred.
    pub file: String,
    /// The old symbol/value (removed/changed from).
    pub old_symbol: String,
    /// The new symbol/value (added/changed to), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_symbol: Option<String>,
    /// Line number in the diff (approximate).
    #[serde(default)]
    pub line: usize,
}

/// Type of production change detected from git diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// Method/function was renamed.
    MethodRename,
    /// Method/function was removed entirely.
    MethodRemoved,
    /// Class/trait was renamed.
    ClassRename,
    /// Class/trait was removed entirely.
    ClassRemoved,
    /// Error code string changed.
    ErrorCodeChange,
    /// Return type annotation changed.
    ReturnTypeChange,
    /// Method signature changed (different parameters).
    SignatureChange,
    /// File was moved/renamed.
    FileMove,
    /// String constant changed.
    StringChange,
}

/// A test file that references a changed symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftedTest {
    /// Test file path.
    pub test_file: String,
    /// Line number where the old symbol is referenced.
    pub line: usize,
    /// The line content.
    pub content: String,
    /// Reference to the production change that caused the drift.
    pub change_index: usize,
}

/// Full drift report.
#[derive(Debug, Clone, Serialize)]
pub struct DriftReport {
    /// Component name.
    pub component: String,
    /// Git ref used as baseline (tag, commit, branch).
    pub since: String,
    /// Production changes detected.
    pub production_changes: Vec<ProductionChange>,
    /// Tests that reference changed symbols.
    pub drifted_tests: Vec<DriftedTest>,
    /// Total unique test files affected.
    pub total_drifted_files: usize,
    /// Total drift references found.
    pub total_drift_references: usize,
    /// Changes that could be auto-fixed with refactor transform.
    pub auto_fixable: usize,
}

/// Captured tail of a test runner's stdout/stderr.
///
/// Surfaced on failure so the actual tool output
/// is visible in the structured JSON response. The tail is bounded by
/// `RAW_OUTPUT_TAIL_LINES` to keep JSON payloads small while still showing
/// the last error / stack frame, which is almost always the relevant part
/// for bootstrap failures. (#1143)
#[derive(Debug, Clone, Serialize)]
pub struct RawTestOutput {
    /// Last N lines of stdout. Empty string if the runner emitted no stdout.
    pub stdout_tail: String,
    /// Last N lines of stderr. Empty string if the runner emitted no stderr.
    pub stderr_tail: String,
    /// Whether either tail was truncated from the original output.
    pub truncated: bool,
    /// Whether stdout capture itself was bounded before the line tail was built.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub stdout_truncated: bool,
    /// Whether stderr capture itself was bounded before the line tail was built.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub stderr_truncated: bool,
    /// Total stdout bytes observed before bounded capture retained its tail.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stdout_seen_bytes: usize,
    /// Stdout bytes retained in this structured raw-output excerpt.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stdout_retained_bytes: usize,
    /// Total stderr bytes observed before bounded capture retained its tail.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stderr_seen_bytes: usize,
    /// Stderr bytes retained in this structured raw-output excerpt.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stderr_retained_bytes: usize,
    /// Maximum stdout bytes retained by the self-check capture buffer.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stdout_limit_bytes: usize,
    /// Maximum stderr bytes retained by the self-check capture buffer.
    #[serde(skip_serializing_if = "is_zero", default)]
    pub stderr_limit_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixDriftOutput {
    pub since: String,
    pub auto_fixable_changes: usize,
    pub generated_rules: usize,
    pub replacements: usize,
    pub files_modified: usize,
    pub written: bool,
    pub rerun_recommended: bool,
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
