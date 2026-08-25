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

use std::path::Path;

pub(crate) use claims::ClaimType;
pub(crate) use verify::VerifyResult;

/// Find all markdown files in the docs directory.
///
/// Excludes configured doc targets using file-name matching (case-insensitive).
/// Filenames excluded from docs audit by default.
/// CHANGELOG files are historically referential by design — they reference
/// old functions, modules, and paths that no longer exist. Flagging them
/// as stale/broken doc references is noise, not signal.
const DEFAULT_DOC_EXCLUDES: &[&str] = &["changelog.md"];

pub(crate) fn find_doc_files(docs_path: &Path, excluded_targets: &[String]) -> Vec<String> {
    use homeboy_engine_primitives::codebase_scan::{self, ExtensionFilter, ScanConfig};

    if !docs_path.exists() {
        return Vec::new();
    }

    let mut excluded_filenames: std::collections::HashSet<String> = excluded_targets
        .iter()
        .filter_map(|p| Path::new(p).file_name())
        .filter_map(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .collect();
    excluded_filenames.extend(DEFAULT_DOC_EXCLUDES.iter().map(|s| s.to_string()));

    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["md".to_string()]),
        skip_hidden: true,
        ..Default::default()
    };

    let files = codebase_scan::walk_files(docs_path, &config);
    let mut docs: Vec<String> = files
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            if excluded_filenames.contains(&name.to_lowercase()) {
                return None;
            }
            let rel = path.strip_prefix(docs_path).ok()?;
            Some(rel.to_string_lossy().to_string())
        })
        .collect();

    docs.sort();
    docs
}

/// Collect audit ignore patterns from all linked extensions.
pub(crate) fn collect_extension_ignore_patterns(extension_ids: &[String]) -> Vec<String> {
    let mut patterns = Vec::new();
    for extension_id in extension_ids {
        if let Some(manifest) = crate::extension_manifests::load_audit_manifest(extension_id) {
            patterns.extend(manifest.audit_ignore_claim_patterns);
        }
    }
    patterns
}

#[cfg(test)]
mod tests {
    use std::fs;

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
    fn test_find_doc_files_default_excludes_changelog() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();

        // CHANGELOG is excluded by default (historically referential by design)
        let files = find_doc_files(docs_path, &[]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "guide.md");
    }

    #[test]
    fn test_find_doc_files_custom_excluded_target() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path();

        fs::write(docs_path.join("guide.md"), "# Guide\n").unwrap();
        fs::write(docs_path.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        fs::write(docs_path.join("CHANGES.md"), "# Changes\n").unwrap();

        // CHANGES.md excluded by caller, CHANGELOG.md excluded by default
        let files = find_doc_files(docs_path, &["CHANGES.md".to_string()]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "guide.md");
    }
}
