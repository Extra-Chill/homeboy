//! Test-drift convention contract type.

use serde::{Deserialize, Serialize};

/// Test drift convention: how source and test files are selected for drift scans.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestDriftConfig {
    /// Source directories to scan (relative to component root).
    pub source_dirs: Vec<String>,
    /// Test directories to scan (relative to component root).
    pub test_dirs: Vec<String>,
    /// File extensions to include when building source/test glob patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Whether the language supports inline tests. Stored for consumers that
    /// need it; drift scanning still uses source/test glob patterns.
    #[serde(default)]
    pub inline_tests: bool,
}
