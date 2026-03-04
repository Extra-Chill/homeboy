//! Cleanup baseline — delegates to the generic `utils::baseline` primitive.
//!
//! Baselines config health issues so CI only fails on NEW issues.

use std::path::Path;

use crate::baseline::{self as generic, BaselineConfig, Fingerprintable};

use super::config::ConfigIssue;
use super::CleanupResult;

// ============================================================================
// Baseline key
// ============================================================================

/// Key used in `homeboy.json` → `baselines.cleanup`.
const BASELINE_KEY: &str = "cleanup";

// ============================================================================
// Fingerprintable implementation for config issues
// ============================================================================

/// Wrapper that implements [`Fingerprintable`] for config issues.
///
/// Uses `category::message` as the identity. The message contains enough
/// context (file paths, extension names) to be stable across runs.
struct CleanupFinding<'a>(&'a ConfigIssue);

impl Fingerprintable for CleanupFinding<'_> {
    fn fingerprint(&self) -> String {
        format!("{}::{}", self.0.category, self.0.message)
    }

    fn description(&self) -> String {
        self.0.message.clone()
    }

    fn context_label(&self) -> String {
        self.0.category.clone()
    }
}

// ============================================================================
// Public types (re-exports from generic)
// ============================================================================

/// A saved cleanup baseline snapshot.
pub type CleanupBaseline = generic::Baseline<()>;

/// Result of comparing cleanup against a baseline.
pub type BaselineComparison = generic::Comparison;

// ============================================================================
// Public API
// ============================================================================

/// Save the current cleanup result as a baseline.
pub fn save_baseline(
    source_path: &Path,
    result: &CleanupResult,
) -> crate::error::Result<std::path::PathBuf> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);

    let items: Vec<CleanupFinding> = result.config_issues.iter().map(CleanupFinding).collect();

    generic::save(&config, &result.component_id, &items, ())
}

/// Load a cleanup baseline if one exists.
pub fn load_baseline(source_path: &Path) -> Option<CleanupBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<()>(&config).ok().flatten()
}

/// Compare cleanup result against a saved baseline.
pub fn compare(result: &CleanupResult, baseline: &CleanupBaseline) -> BaselineComparison {
    let items: Vec<CleanupFinding> = result.config_issues.iter().map(CleanupFinding).collect();
    generic::compare(&items, baseline)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::config::IssueSeverity;
    use crate::cleanup::CleanupSummary;

    fn make_issue(category: &str, message: &str) -> ConfigIssue {
        ConfigIssue {
            severity: IssueSeverity::Warning,
            category: category.to_string(),
            message: message.to_string(),
            fix_hint: None,
        }
    }

    fn make_result(issues: Vec<ConfigIssue>) -> CleanupResult {
        CleanupResult {
            component_id: "test".to_string(),
            summary: CleanupSummary {
                config_issues: issues.len(),
            },
            config_issues: issues,
            hints: vec![],
        }
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let result = make_result(vec![
            make_issue("local_path", "local_path does not exist: /old/path"),
            make_issue("extensions", "Extension 'old-ext' could not be loaded"),
        ]);

        save_baseline(dir.path(), &result).unwrap();
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.context_id, "test");
        assert_eq!(loaded.item_count, 2);
    }

    #[test]
    fn compare_detects_new_issue() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_result(vec![make_issue("local_path", "local_path does not exist")]);

        save_baseline(dir.path(), &original).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = make_result(vec![
            make_issue("local_path", "local_path does not exist"),
            make_issue("extensions", "Extension 'bad' could not be loaded"),
        ]);

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
    }

    #[test]
    fn compare_detects_resolved_issue() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_result(vec![
            make_issue("local_path", "local_path does not exist"),
            make_issue("extensions", "Extension 'old' could not be loaded"),
        ]);

        save_baseline(dir.path(), &original).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = make_result(vec![make_issue("local_path", "local_path does not exist")]);

        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
    }
}
