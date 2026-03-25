//! methods — extracted from test_coverage.rs.

use std::collections::{HashMap, HashSet};
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::super::test_mapping::{
    build_source_name_index, partition_fingerprints, source_to_test_path, test_to_source_path,
};
use crate::extension::TestMappingConfig;
use std::path::Path;
use regex::Regex;


/// Collect test method names from a fingerprint.
pub(crate) fn collect_test_methods_from_fp(fp: &FileFingerprint, config: &TestMappingConfig) -> Vec<String> {
    fp.methods
        .iter()
        .filter(|m| m.starts_with(&config.method_prefix))
        .cloned()
        .collect()
}

/// Check 4: Orphaned test methods — test methods whose source method no longer exists.
///
/// For each `test_X` method, checks whether `X` exists in the source file's methods.
/// This catches tests left behind when a function is deleted from the source — the kind
/// of breakage that `#[cfg(test)]` hides from normal compilation.
pub(crate) fn find_orphaned_test_methods(
    findings: &mut Vec<Finding>,
    file_path: &str,
    test_methods: &[String],
    source_methods: &HashSet<&str>,
    config: &TestMappingConfig,
) {
    for test_method in test_methods {
        let Some(expected_source) = test_method.strip_prefix(&config.method_prefix) else {
            continue;
        };

        // Skip if the test doesn't follow the naming convention (no source method implied)
        if expected_source.is_empty() {
            continue;
        }

        // If the source method exists (exact match), this test is valid
        if source_methods.contains(expected_source) {
            continue;
        }

        // Prefix match: test_compare_detects_new_drift should match source method
        // "compare" because it's testing a specific scenario of that method.
        // We require the prefix to be followed by '_' to prevent false matches
        // (e.g., test_parse_this should not match source method "par").
        let has_prefix_match = source_methods.iter().any(|source_method| {
            let prefix = format!("{}_", source_method);
            expected_source.starts_with(&prefix)
        });
        if has_prefix_match {
            continue;
        }

        // Behavior-driven test names: test_detects_exact_duplicate describes a
        // behavior, not a method reference. Short names (1-2 segments like
        // "old_function" or "pause") are likely real method references. But
        // longer compound names (3+ segments like "detects_exact_duplicate" or
        // "audit_metadata_roundtrips") are probably behavioral descriptions
        // unless the first segment matches a source method.
        let segment_count = expected_source.split('_').count();
        if segment_count >= 3 {
            let first_word = expected_source.split('_').next().unwrap_or(expected_source);
            let any_method_starts_with_first_word = source_methods
                .iter()
                .any(|m| m.starts_with(first_word) || first_word.starts_with(m));
            if !any_method_starts_with_first_word {
                continue;
            }
        }

        findings.push(Finding {
            convention: "test_coverage".to_string(),
            severity: Severity::Warning,
            file: file_path.to_string(),
            description: format!(
                "Test method '{}' references '{}' which no longer exists in the source",
                test_method, expected_source
            ),
            suggestion: format!(
                "Remove the orphaned test '{}' or rename it to match an existing method",
                test_method
            ),
            kind: AuditFinding::OrphanedTest,
        });
    }
}
