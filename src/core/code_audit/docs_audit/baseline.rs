//! Docs audit baseline — delegates to the generic `utils::baseline` primitive.
//!
//! Baselines broken references so CI only fails on NEW broken refs.

use std::path::Path;

use crate::baseline::{self as generic, BaselineConfig, Fingerprintable};

use super::{AuditResult, BrokenReference};

// ============================================================================
// Baseline key
// ============================================================================

/// Key used in `homeboy.json` → `baselines.docs`.
const BASELINE_KEY: &str = "docs";

// ============================================================================
// Docs-specific metadata
// ============================================================================

/// Domain-specific metadata stored alongside the generic baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocsBaselineMetadata {
    /// Total docs scanned at baseline time.
    pub docs_scanned: usize,
    /// Total undocumented features at baseline time.
    pub undocumented_features: usize,
}

// ============================================================================
// Fingerprintable implementation for broken references
// ============================================================================

/// Wrapper that implements [`Fingerprintable`] for broken references.
///
/// Uses `doc::claim` as the identity. Line numbers are excluded because
/// editing a doc file shifts line numbers without changing the reference.
struct DocsFinding<'a>(&'a BrokenReference);

impl Fingerprintable for DocsFinding<'_> {
    fn fingerprint(&self) -> String {
        format!("{}::{}", self.0.doc, self.0.claim)
    }

    fn description(&self) -> String {
        self.0.action.clone()
    }

    fn context_label(&self) -> String {
        self.0.doc.clone()
    }
}

// ============================================================================
// Public types (re-exports from generic)
// ============================================================================

/// A saved docs baseline snapshot.
pub type DocsBaseline = generic::Baseline<DocsBaselineMetadata>;

/// Result of comparing docs audit against a baseline.
pub type BaselineComparison = generic::Comparison;

// ============================================================================
// Public API
// ============================================================================

/// Save the current docs audit result as a baseline.
pub fn save_baseline(
    source_path: &Path,
    result: &AuditResult,
) -> crate::error::Result<std::path::PathBuf> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);

    let metadata = DocsBaselineMetadata {
        docs_scanned: result.summary.docs_scanned,
        undocumented_features: result.summary.undocumented_features,
    };

    let items: Vec<DocsFinding> = result.broken_references.iter().map(DocsFinding).collect();

    generic::save(&config, &result.component_id, &items, metadata)
}

/// Load a docs baseline if one exists.
pub fn load_baseline(source_path: &Path) -> Option<DocsBaseline> {
    let config = BaselineConfig::new(source_path, BASELINE_KEY);
    generic::load::<DocsBaselineMetadata>(&config)
        .ok()
        .flatten()
}

/// Compare docs audit result against a saved baseline.
pub fn compare(result: &AuditResult, baseline: &DocsBaseline) -> BaselineComparison {
    let items: Vec<DocsFinding> = result.broken_references.iter().map(DocsFinding).collect();
    generic::compare(&items, baseline)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::docs_audit::{AlignmentSummary, ClaimConfidence};

    fn make_broken_ref(doc: &str, claim: &str) -> BrokenReference {
        BrokenReference {
            doc: doc.to_string(),
            line: 10,
            claim: claim.to_string(),
            confidence: ClaimConfidence::Real,
            doc_context: None,
            action: "Stale reference".to_string(),
        }
    }

    fn make_result(broken: Vec<BrokenReference>) -> AuditResult {
        AuditResult {
            component_id: "test".to_string(),
            baseline_ref: None,
            summary: AlignmentSummary {
                docs_scanned: 5,
                priority_docs: 0,
                broken_references: broken.len(),
                unchanged_docs: 5,
                total_features: 0,
                documented_features: 0,
                undocumented_features: 0,
            },
            changed_files: vec![],
            priority_docs: vec![],
            broken_references: broken,
            undocumented_features: vec![],
            detected_features: vec![],
        }
    }

    #[test]
    fn save_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let result = make_result(vec![
            make_broken_ref("api.md", "file path `src/old.rs`"),
            make_broken_ref("guide.md", "class `OldClass`"),
        ]);

        save_baseline(dir.path(), &result).unwrap();
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.context_id, "test");
        assert_eq!(loaded.item_count, 2);
        assert_eq!(loaded.metadata.docs_scanned, 5);
    }

    #[test]
    fn compare_detects_new_broken_ref() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_result(vec![make_broken_ref("api.md", "file path `src/old.rs`")]);

        save_baseline(dir.path(), &original).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = make_result(vec![
            make_broken_ref("api.md", "file path `src/old.rs`"),
            make_broken_ref("guide.md", "file path `src/removed.rs`"),
        ]);

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
    }

    #[test]
    fn compare_detects_resolved_ref() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_result(vec![
            make_broken_ref("api.md", "file path `src/old.rs`"),
            make_broken_ref("guide.md", "class `OldClass`"),
        ]);

        save_baseline(dir.path(), &original).unwrap();
        let baseline = load_baseline(dir.path()).unwrap();

        let current = make_result(vec![make_broken_ref("api.md", "file path `src/old.rs`")]);

        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
    }

    #[test]
    fn fingerprint_ignores_line_number() {
        let ref1 = BrokenReference {
            doc: "api.md".to_string(),
            line: 10,
            claim: "file path `src/old.rs`".to_string(),
            confidence: ClaimConfidence::Real,
            doc_context: None,
            action: "Stale".to_string(),
        };
        let ref2 = BrokenReference {
            doc: "api.md".to_string(),
            line: 42, // different line
            claim: "file path `src/old.rs`".to_string(),
            confidence: ClaimConfidence::Real,
            doc_context: None,
            action: "Stale".to_string(),
        };
        assert_eq!(
            DocsFinding(&ref1).fingerprint(),
            DocsFinding(&ref2).fingerprint()
        );
    }
}
