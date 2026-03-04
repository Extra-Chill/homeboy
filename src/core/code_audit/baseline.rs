//! Audit-specific baseline — delegates to the generic `utils::baseline` primitive.
//!
//! Provides the audit domain's [`Fingerprintable`] implementation for findings,
//! plus backward-compatible wrappers (`save_baseline`, `load_baseline`, `compare`)
//! that the audit command uses directly.

use std::path::Path;

use crate::baseline::{self as generic, BaselineConfig, Fingerprintable};

use super::findings::Finding;
use super::CodeAuditResult;

// ============================================================================
// Baseline key
// ============================================================================

/// Key used in `homeboy.json` → `baselines.audit`.
const BASELINE_KEY: &str = "audit";

// ============================================================================
// Audit-specific metadata
// ============================================================================

/// Domain-specific metadata stored alongside the generic baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditBaselineMetadata {
    /// Total outlier files at baseline time.
    pub outliers_count: usize,
    /// Alignment score at baseline time.
    pub alignment_score: Option<f32>,
    /// Set of known outlier file paths (accepted drift).
    pub known_outliers: Vec<String>,
}

// ============================================================================
// Fingerprintable implementation for audit findings
// ============================================================================

/// Wrapper that implements [`Fingerprintable`] for audit findings.
///
/// Uses `convention::file::kind` as the core identity. The description is
/// excluded because structural findings embed volatile values (e.g. exact
/// line counts) that change when a file grows by even one line. Including
/// them would cause the same finding to appear as "resolved + new" on every
/// minor change, defeating the baseline ratchet.
struct AuditFinding<'a>(&'a Finding);

impl Fingerprintable for AuditFinding<'_> {
    fn fingerprint(&self) -> String {
        format!("{}::{}::{:?}", self.0.convention, self.0.file, self.0.kind)
    }

    fn description(&self) -> String {
        self.0.description.clone()
    }

    fn context_label(&self) -> String {
        self.0.convention.clone()
    }
}

// ============================================================================
// Backward-compatible public types
// ============================================================================

/// A saved baseline snapshot (backward-compatible alias).
///
/// This is the generic baseline parameterized with audit metadata.
pub type AuditBaseline = generic::Baseline<AuditBaselineMetadata>;

/// Result of comparing an audit against a baseline.
pub type BaselineComparison = generic::Comparison;

/// A finding that wasn't in the baseline.
pub type NewFinding = generic::NewItem;

// ============================================================================
// Backward-compatible public API
// ============================================================================

/// Get the baseline file path for a source directory.
///
/// Now points to `homeboy.json` instead of `.homeboy/audit-baseline.json`.
pub fn baseline_path(source_path: &Path) -> std::path::PathBuf {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    config.json_path()
}

/// Save the current audit result as a baseline.
pub fn save_baseline(result: &CodeAuditResult) -> Result<std::path::PathBuf, String> {
    let source = Path::new(&result.source_path);
    let config = BaselineConfig::new(source, BASELINE_KEY);

    let known_outliers: Vec<String> = result
        .conventions
        .iter()
        .flat_map(|c| c.outliers.iter().map(|o| o.file.clone()))
        .collect();

    let metadata = AuditBaselineMetadata {
        outliers_count: known_outliers.len(),
        alignment_score: result.summary.alignment_score,
        known_outliers,
    };

    let items: Vec<AuditFinding> = result.findings.iter().map(AuditFinding).collect();

    generic::save(&config, &result.component_id, &items, metadata)
        .map_err(|e| e.message)
}

/// Load a baseline if one exists for the given source path.
pub fn load_baseline(source_path: &Path) -> Option<AuditBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<AuditBaselineMetadata>(&config).ok().flatten()
}

/// Compare an audit result against a saved baseline.
pub fn compare(result: &CodeAuditResult, baseline: &AuditBaseline) -> BaselineComparison {
    let items: Vec<AuditFinding> = result.findings.iter().map(AuditFinding).collect();
    generic::compare(&items, baseline)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::DeviationKind;
    use crate::code_audit::findings::{Finding, Severity};
    use crate::code_audit::{AuditSummary, CodeAuditResult};

    fn make_finding(convention: &str, file: &str, description: &str) -> Finding {
        Finding {
            convention: convention.to_string(),
            severity: Severity::Warning,
            file: file.to_string(),
            description: description.to_string(),
            suggestion: String::new(),
            kind: DeviationKind::MissingMethod,
        }
    }

    fn make_result(findings: Vec<Finding>, test_name: &str) -> CodeAuditResult {
        let dir = std::env::temp_dir().join(format!("homeboy_baseline_{}", test_name));
        let _ = std::fs::remove_dir_all(&dir); // Clean slate
        let _ = std::fs::create_dir_all(&dir);
        CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 10,
                conventions_detected: 1,
                outliers_found: findings.len(),
                alignment_score: Some(0.8),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings,
            duplicate_groups: vec![],
        }
    }

    #[test]
    fn save_and_load_baseline() {
        let result = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "save_load",
        );

        let path = save_baseline(&result).unwrap();
        assert!(path.exists());

        let loaded = load_baseline(Path::new(&result.source_path)).unwrap();
        assert_eq!(loaded.context_id, "test");
        assert_eq!(loaded.item_count, 2);
        assert_eq!(loaded.known_fingerprints.len(), 2);

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn compare_no_new_drift() {
        let result = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "no_new_drift",
        );
        let _ = save_baseline(&result).unwrap();
        let baseline = load_baseline(Path::new(&result.source_path)).unwrap();

        let comparison = compare(&result, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert!(comparison.resolved_fingerprints.is_empty());
        assert_eq!(comparison.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn compare_detects_new_drift() {
        let result_original = make_result(
            vec![make_finding("Flow", "a.php", "Missing method: execute")],
            "new_drift",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        // New finding added — reuse same source_path
        let mut current = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "new_drift_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.new_items[0].fingerprint, "Flow::c.php::MissingMethod");
        assert_eq!(comparison.delta, 1);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn compare_detects_resolved_drift() {
        let result_original = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "resolved_drift",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        let mut current = make_result(
            vec![make_finding("Flow", "a.php", "Missing method: execute")],
            "resolved_drift_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, -1);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn compare_new_and_resolved_simultaneously() {
        let result_original = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "b.php", "Missing method: validate"),
            ],
            "new_and_resolved",
        );
        let _ = save_baseline(&result_original).unwrap();
        let baseline = load_baseline(Path::new(&result_original.source_path)).unwrap();

        // b.php fixed, but c.php introduced
        let mut current = make_result(
            vec![
                make_finding("Flow", "a.php", "Missing method: execute"),
                make_finding("Flow", "c.php", "Missing method: register"),
            ],
            "new_and_resolved_current",
        );
        current.source_path = result_original.source_path.clone();

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, 0);

        let _ = std::fs::remove_dir_all(Path::new(&result_original.source_path));
    }

    #[test]
    fn no_baseline_returns_none() {
        let result = load_baseline(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn audit_metadata_roundtrips() {
        let result = make_result(
            vec![make_finding("Flow", "a.php", "Missing method")],
            "metadata_roundtrip",
        );

        let _ = save_baseline(&result).unwrap();
        let loaded = load_baseline(Path::new(&result.source_path)).unwrap();

        assert_eq!(loaded.metadata.alignment_score, Some(0.8));

        let _ = std::fs::remove_dir_all(Path::new(&result.source_path));
    }

    #[test]
    fn fingerprint_is_stable() {
        let f1 = make_finding("Flow", "a.php", "Missing method: execute");
        let f2 = make_finding("Flow", "a.php", "Missing method: execute");
        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f2).fingerprint()
        );

        // Different file = different fingerprint
        let f3 = make_finding("Flow", "b.php", "Missing method: execute");
        assert_ne!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f3).fingerprint()
        );
    }

    #[test]
    fn fingerprint_ignores_description() {
        let f1 = Finding {
            convention: "structural".to_string(),
            severity: Severity::Warning,
            file: "deploy.rs".to_string(),
            description: "File has 2484 lines (threshold: 500)".to_string(),
            suggestion: String::new(),
            kind: DeviationKind::GodFile,
        };
        let f2 = Finding {
            convention: "structural".to_string(),
            severity: Severity::Warning,
            file: "deploy.rs".to_string(),
            description: "File has 2645 lines (threshold: 500)".to_string(),
            suggestion: String::new(),
            kind: DeviationKind::GodFile,
        };
        assert_eq!(
            AuditFinding(&f1).fingerprint(),
            AuditFinding(&f2).fingerprint(),
            "fingerprint should not change when line count changes"
        );
    }
}
