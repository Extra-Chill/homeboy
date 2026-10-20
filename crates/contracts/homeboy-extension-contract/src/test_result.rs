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
    /// Scope mode the run was originally planned with before Homeboy widened
    /// it (e.g. `"changed"` when a changed-scope run was re-planned as
    /// `mode: "full"`). `None` when the planned scope ran as-is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub widened_from: Option<String>,
    /// Typed reason the scope was widened (e.g. `"harness_changed"`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub widening_reason: Option<String>,
    /// Changed files that triggered the widening (e.g. the test-harness
    /// config files whose change only a full-suite run can evidence).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub widening_files: Vec<String>,
}

/// Scope mode for a full-suite run.
pub const TEST_SCOPE_MODE_FULL: &str = "full";
/// Scope mode for a changed-since run.
pub const TEST_SCOPE_MODE_CHANGED: &str = "changed";
/// Widening reason: a test-harness config file changed and the changed scope
/// selected zero tests, so only the full suite can evidence the change.
pub const TEST_SCOPE_WIDENING_HARNESS_CHANGED: &str = "harness_changed";

impl TestScopeOutput {
    /// Re-plan this changed scope as a full-suite scope, recording why.
    ///
    /// The returned receipt keeps `changed_since` and the original
    /// `source_changes_without_tests` so the evidence remains auditable, while
    /// `mode` becomes `"full"` and the widening fields name the trigger.
    pub fn widened_to_full(&self, reason: &str, files: Vec<String>) -> Self {
        Self {
            mode: TEST_SCOPE_MODE_FULL.to_string(),
            changed_since: self.changed_since.clone(),
            selected_count: 0,
            selected_files: Vec::new(),
            source_changes_without_tests: self.source_changes_without_tests.clone(),
            widened_from: Some(self.mode.clone()),
            widening_reason: Some(reason.to_string()),
            widening_files: files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widened_to_full_records_reason_and_files() {
        let changed = TestScopeOutput {
            mode: TEST_SCOPE_MODE_CHANGED.to_string(),
            changed_since: Some("base".to_string()),
            selected_count: 0,
            selected_files: Vec::new(),
            source_changes_without_tests: vec![".github/workflows/ci.yml".to_string()],
            widened_from: None,
            widening_reason: None,
            widening_files: Vec::new(),
        };
        let full = changed.widened_to_full(
            TEST_SCOPE_WIDENING_HARNESS_CHANGED,
            vec![".github/workflows/ci.yml".to_string()],
        );
        let json = serde_json::to_value(&full).expect("serialize");
        assert_eq!(json["mode"], "full");
        assert_eq!(json["widened_from"], "changed");
        assert_eq!(json["widening_reason"], "harness_changed");
        assert_eq!(json["widening_files"][0], ".github/workflows/ci.yml");
        assert_eq!(json["changed_since"], "base");

        let plain = serde_json::to_value(&changed).expect("serialize");
        assert!(plain.get("widened_from").is_none());
        assert!(plain.get("widening_reason").is_none());
        assert!(plain.get("widening_files").is_none());
    }
}
