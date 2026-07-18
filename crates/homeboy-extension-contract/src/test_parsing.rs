//! Pure test parsing-result contract types (summary, coverage).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct CoverageOutput {
    pub lines_pct: f64,
    pub lines_total: u64,
    pub lines_covered: u64,
    pub methods_pct: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered_files: Vec<UncoveredFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UncoveredFile {
    pub file: String,
    pub line_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestFailureSummaryItem {
    pub test_name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestSummaryOutput {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<TestFailureSummaryItem>,
    pub exit_code: i32,
}
