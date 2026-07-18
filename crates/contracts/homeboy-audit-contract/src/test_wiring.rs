use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TestWiringConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<TestWiringPolicy>,
}

impl TestWiringConfig {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    pub(super) fn merge(&mut self, other: &TestWiringConfig) {
        for policy in &other.policies {
            if !self
                .policies
                .iter()
                .any(|existing| existing.id == policy.id)
            {
                self.policies.push(policy.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestWiringPolicy {
    /// Stable policy id used when merging extension and component config.
    pub id: String,
    /// Source files to scan for explicit wiring markers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_path_globs: Vec<String>,
    /// Test files that need policy evaluation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_path_globs: Vec<String>,
    /// Test files discovered by the native test runner without explicit wiring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_discovered_test_path_globs: Vec<String>,
    /// Test support/helper files exempt from explicit wiring checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub support_test_path_globs: Vec<String>,
    /// Whether matched, non-exempt test files must be referenced by source markers.
    #[serde(default)]
    pub require_explicit_wiring: bool,
    /// Regex patterns proving source-to-test wiring. `{test_path}` is replaced
    /// with a regex-escaped repository-relative test path before matching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explicit_wiring_marker_patterns: Vec<String>,
    /// Report convention label.
    #[serde(default = "default_test_wiring_convention")]
    pub convention: String,
    /// Finding severity: `warning` or `info`.
    #[serde(default = "default_test_wiring_severity")]
    pub severity: String,
    /// Finding description template. Supports `{test_path}`.
    #[serde(default = "default_test_wiring_description")]
    pub description: String,
    /// Finding suggestion template. Supports `{test_path}`.
    #[serde(default = "default_test_wiring_suggestion")]
    pub suggestion: String,
}

fn default_test_wiring_convention() -> String {
    "test_wiring".to_string()
}

fn default_test_wiring_severity() -> String {
    "warning".to_string()
}

fn default_test_wiring_description() -> String {
    "Test file `{test_path}` is not wired into the configured test harness".to_string()
}

fn default_test_wiring_suggestion() -> String {
    "Wire `{test_path}` using the configured explicit wiring convention".to_string()
}
