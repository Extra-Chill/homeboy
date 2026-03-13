//! Documentation audit system for extracting and verifying claims from markdown files.
//!
//! This module provides doc-centric primitives for documentation auditing:
//! 1. Extract claims from documentation (file paths, identifiers, code examples)
//! 2. Verify claims against the actual codebase
//!
//! The unified audit system (`core/code_audit/mod.rs`) uses these primitives
//! for doc drift detection. The `docs generate --from-audit` command uses the
//! result types for documentation generation.

pub(crate) mod claims;
pub(crate) mod verify;

use std::fs;
use std::path::Path;

pub use claims::{Claim, ClaimConfidence, ClaimType};
pub use verify::VerifyResult;

use crate::{component, extension, is_zero};

/// A doc that needs content review due to referenced files changing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriorityDoc {
    pub doc: String,
    pub reason: String,
    pub changed_files_referenced: Vec<String>,
    pub code_examples: usize,
    pub action: String,
}

/// A feature found in source code with no mention in documentation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UndocumentedFeature {
    pub name: String,
    pub source_file: String,
    pub line: usize,
    pub pattern: String,
}

/// A feature detected in source code (documented or not).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DetectedFeature {
    pub name: String,
    pub source_file: String,
    pub line: usize,
    pub pattern: String,
    pub documented: bool,
    /// Doc comment lines found directly above the feature (e.g. `///`, `/**`, `#`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fields or items inside the feature's block (struct fields, enum variants, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FeatureField>>,
}

/// A field or item inside a detected feature's block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeatureField {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A broken reference that needs fixing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrokenReference {
    pub doc: String,
    pub line: usize,
    pub claim: String,
    pub confidence: ClaimConfidence,
    /// Surrounding lines from the doc file for context (up to 3 lines around the reference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_context: Option<Vec<String>>,
    pub action: String,
}

/// Summary counts for the alignment report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlignmentSummary {
    pub docs_scanned: usize,
    pub priority_docs: usize,
    pub broken_references: usize,
    pub unchanged_docs: usize,
    /// Total features detected by extension-defined patterns (omitted when 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_features: usize,
    /// Features with at least one mention in documentation (omitted when 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub documented_features: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undocumented_features: usize,
}

/// Result of auditing a component's documentation for content alignment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditResult {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    pub summary: AlignmentSummary,
    pub changed_files: Vec<String>,
    pub priority_docs: Vec<PriorityDoc>,
    pub broken_references: Vec<BrokenReference>,
    pub undocumented_features: Vec<UndocumentedFeature>,
    /// All detected features (only populated when `--features` flag is set).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detected_features: Vec<DetectedFeature>,
}

/// Find all markdown files in the docs directory.
///
/// Excludes configured doc targets using file-name matching (case-insensitive).
pub(crate) fn find_doc_files(docs_path: &Path, excluded_targets: &[String]) -> Vec<String> {
    let mut docs = Vec::new();

    if !docs_path.exists() {
        return docs;
    }

    let excluded_filenames: std::collections::HashSet<String> = excluded_targets
        .iter()
        .filter_map(|p| Path::new(p).file_name())
        .filter_map(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .collect();

    fn scan_docs(
        dir: &Path,
        prefix: &str,
        docs: &mut Vec<String>,
        excluded_filenames: &std::collections::HashSet<String>,
    ) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_file() && name.ends_with(".md") {
                    if excluded_filenames.contains(&name.to_lowercase()) {
                        continue;
                    }

                    let relative = if prefix.is_empty() {
                        name
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    docs.push(relative);
                } else if path.is_dir() {
                    let new_prefix = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    scan_docs(&path, &new_prefix, docs, excluded_filenames);
                }
            }
        }
    }

    scan_docs(docs_path, "", &mut docs, &excluded_filenames);
    docs.sort();
    docs
}

/// Collect audit ignore patterns from all linked extensions.
pub(crate) fn collect_extension_ignore_patterns(comp: &component::Component) -> Vec<String> {
    let mut patterns = Vec::new();
    if let Some(ref extensions) = comp.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                patterns.extend(manifest.audit_ignore_claim_patterns().to_vec());
            }
        }
    }
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_doc_files_excludes_configured_targets() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(
            docs_path.join("CHANGELOG.md"),
            "# Changelog\n## v1.0\n- Removed old/path.rs\n",
        )
        .unwrap();
        fs::write(docs_path.join("api.md"), "# API\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGELOG.md".to_string()]);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"api.md".to_string()));
        assert!(files.contains(&"guide.md".to_string()));
        assert!(!files.iter().any(|f| f.to_lowercase().contains("changelog")));
    }

    #[test]
    fn test_find_doc_files_exclusion_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("changelog.md"), "# Changes\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGELOG.md".to_string()]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "guide.md");
    }

    #[test]
    fn test_find_doc_files_no_exclusion_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();

        // Without exclusion, changelog should be included
        let files = find_doc_files(docs_path, &[]);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f == "CHANGELOG.md"));
    }

    #[test]
    fn test_find_doc_files_custom_excluded_target() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        fs::write(docs_path.join("CHANGES.md"), "# Changes\n").unwrap();

        let files = find_doc_files(docs_path, &["CHANGES.md".to_string()]);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"CHANGELOG.md".to_string()));
        assert!(files.contains(&"guide.md".to_string()));
        assert!(!files.iter().any(|f| f == "CHANGES.md"));
    }
}
