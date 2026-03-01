//! Duplication detection — find identical functions across source files.
//!
//! Uses method body hashes from fingerprinting to detect exact duplicates.
//! Extension scripts normalize whitespace and hash function bodies during
//! fingerprinting — this module groups by hash to find duplicates.

use std::collections::HashMap;

use super::conventions::DeviationKind;
use super::fingerprint::FileFingerprint;
use super::findings::{Finding, Severity};

/// Minimum number of locations for a function to count as duplicated.
const MIN_DUPLICATE_LOCATIONS: usize = 2;

/// Detect duplicated functions across all fingerprinted files.
///
/// Groups functions by their body hash. When two or more files contain a
/// function with the same name and the same normalized body hash, a finding
/// is emitted for each location.
pub fn detect_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // Group: (method_name, body_hash) → list of file paths
    let mut hash_groups: HashMap<(&str, &str), Vec<&str>> = HashMap::new();

    for fp in fingerprints {
        for (method_name, body_hash) in &fp.method_hashes {
            hash_groups
                .entry((method_name.as_str(), body_hash.as_str()))
                .or_default()
                .push(&fp.relative_path);
        }
    }

    let mut findings = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        let other_files: Vec<&str> = locations.to_vec();
        let suggestion = format!(
            "Function `{}` has identical body in {} files. \
             Extract to a shared module and import it.",
            method_name,
            other_files.len()
        );

        // Emit one finding per file that has the duplicate
        for file in &other_files {
            let other_locations: Vec<&&str> = other_files
                .iter()
                .filter(|f| *f != file)
                .collect();
            let also_in = other_locations
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            findings.push(Finding {
                convention: "duplication".to_string(),
                severity: Severity::Warning,
                file: file.to_string(),
                description: format!(
                    "Duplicate function `{}` — also in {}",
                    method_name, also_in
                ),
                suggestion: suggestion.clone(),
                kind: DeviationKind::DuplicateFunction,
            });
        }
    }

    // Sort by file path then description for deterministic output
    findings.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.description.cmp(&b.description)));
    findings
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;

    fn make_fingerprint(
        path: &str,
        methods: &[&str],
        hashes: &[(&str, &str)],
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.iter().map(|s| s.to_string()).collect(),
            registrations: vec![],
            type_name: None,
            implements: vec![],
            namespace: None,
            imports: vec![],
            content: String::new(),
            method_hashes: hashes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn detects_exact_duplicate() {
        let fp1 = make_fingerprint(
            "src/utils/io.rs",
            &["is_zero"],
            &[("is_zero", "abc123")],
        );
        let fp2 = make_fingerprint(
            "src/utils/validation.rs",
            &["is_zero"],
            &[("is_zero", "abc123")],
        );

        let findings = detect_duplicates(&[&fp1, &fp2]);

        assert_eq!(findings.len(), 2, "Should emit one finding per location");
        assert!(findings.iter().all(|f| f.kind == DeviationKind::DuplicateFunction));
        assert!(findings.iter().any(|f| f.file == "src/utils/io.rs"));
        assert!(findings.iter().any(|f| f.file == "src/utils/validation.rs"));
        assert!(findings[0].description.contains("is_zero"));
    }

    #[test]
    fn no_duplicates_different_hashes() {
        let fp1 = make_fingerprint(
            "src/a.rs",
            &["process"],
            &[("process", "hash_a")],
        );
        let fp2 = make_fingerprint(
            "src/b.rs",
            &["process"],
            &[("process", "hash_b")],
        );

        let findings = detect_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Different hashes should not flag duplicates");
    }

    #[test]
    fn no_duplicates_single_location() {
        let fp = make_fingerprint(
            "src/only.rs",
            &["unique_fn"],
            &[("unique_fn", "abc123")],
        );

        let findings = detect_duplicates(&[&fp]);
        assert!(findings.is_empty(), "Single location is not a duplicate");
    }

    #[test]
    fn three_way_duplicate() {
        let fp1 = make_fingerprint("src/a.rs", &["helper"], &[("helper", "same_hash")]);
        let fp2 = make_fingerprint("src/b.rs", &["helper"], &[("helper", "same_hash")]);
        let fp3 = make_fingerprint("src/c.rs", &["helper"], &[("helper", "same_hash")]);

        let findings = detect_duplicates(&[&fp1, &fp2, &fp3]);

        assert_eq!(findings.len(), 3, "Should flag all 3 locations");
        assert!(findings[0].suggestion.contains("3 files"));
    }

    #[test]
    fn empty_method_hashes_no_findings() {
        let fp1 = make_fingerprint("src/a.rs", &["foo", "bar"], &[]);
        let fp2 = make_fingerprint("src/b.rs", &["foo", "bar"], &[]);

        let findings = detect_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "No hashes means no duplication findings");
    }

    #[test]
    fn mixed_duplicates_and_unique() {
        let fp1 = make_fingerprint(
            "src/a.rs",
            &["shared", "unique_a"],
            &[("shared", "same"), ("unique_a", "hash_a")],
        );
        let fp2 = make_fingerprint(
            "src/b.rs",
            &["shared", "unique_b"],
            &[("shared", "same"), ("unique_b", "hash_b")],
        );

        let findings = detect_duplicates(&[&fp1, &fp2]);

        assert_eq!(findings.len(), 2, "Only 'shared' should be flagged");
        assert!(findings.iter().all(|f| f.description.contains("shared")));
    }
}
