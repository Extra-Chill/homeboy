//! Pure test result contract types (counts, changed-scope selection).

use serde::{Deserialize, Serialize};

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
pub struct TestScopeOutput {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    pub selected_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_files: Vec<String>,
    /// Changed files that are production or test source (per the component's
    /// drift source/test patterns) yet selected zero tests. A non-empty list
    /// with `selected_count == 0` is a false-green: source changed but the
    /// changed-scope gate would otherwise pass without running any test.
    /// Documentation/config-only changes leave this empty so a genuine no-test
    /// scope can still pass. (#8340)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub source_changes_without_tests: Vec<String>,
}
