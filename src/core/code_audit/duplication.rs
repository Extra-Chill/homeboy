//! Duplication detection — find identical functions across source files.
//!
//! Uses method body hashes from fingerprinting to detect exact duplicates.
//! Extension scripts normalize whitespace and hash function bodies during
//! fingerprinting — this module groups by hash to find duplicates.
//!
//! Two outputs:
//! - `detect_duplicates()` → flat `Vec<Finding>` for the audit report
//! - `detect_duplicate_groups()` → structured `Vec<DuplicateGroup>` for the fixer

use std::collections::HashMap;

use super::conventions::DeviationKind;
use super::fingerprint::FileFingerprint;
use super::findings::{Finding, Severity};

/// Minimum number of locations for a function to count as duplicated.
const MIN_DUPLICATE_LOCATIONS: usize = 2;

/// A group of files containing an identical function.
///
/// The fixer uses this to keep the canonical copy and remove the rest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    /// The duplicated function name.
    pub function_name: String,
    /// File chosen to keep the function (canonical location).
    pub canonical_file: String,
    /// Files where the duplicate should be removed and replaced with an import.
    pub remove_from: Vec<String>,
}

/// Build grouped duplication data from fingerprints.
///
/// For each group of identical functions, picks a canonical file (shortest
/// path, then alphabetical) and lists the rest as removal targets.
fn build_groups(fingerprints: &[&FileFingerprint]) -> HashMap<(String, String), Vec<String>> {
    let mut hash_groups: HashMap<(String, String), Vec<String>> = HashMap::new();

    for fp in fingerprints {
        for (method_name, body_hash) in &fp.method_hashes {
            hash_groups
                .entry((method_name.clone(), body_hash.clone()))
                .or_default()
                .push(fp.relative_path.clone());
        }
    }

    hash_groups
}

/// Pick the canonical file from a list of locations.
///
/// Heuristics (in order):
/// 1. Files in a `utils/` directory are preferred (already shared)
/// 2. Shortest path (most general module)
/// 3. Alphabetical (deterministic tiebreaker)
fn pick_canonical(locations: &[String]) -> String {
    let mut sorted = locations.to_vec();
    sorted.sort_by(|a, b| {
        let a_utils = a.contains("/utils/") || a.contains("/utils.");
        let b_utils = b.contains("/utils/") || b.contains("/utils.");
        // utils files first
        b_utils
            .cmp(&a_utils)
            // then shortest path
            .then_with(|| a.len().cmp(&b.len()))
            // then alphabetical
            .then_with(|| a.cmp(b))
    });
    sorted[0].clone()
}

/// Detect duplicate groups with canonical file selection.
///
/// Returns structured data the fixer uses to remove duplicates.
pub fn detect_duplicate_groups(fingerprints: &[&FileFingerprint]) -> Vec<DuplicateGroup> {
    let hash_groups = build_groups(fingerprints);
    let mut groups = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        let canonical = pick_canonical(locations);
        let remove_from: Vec<String> = locations
            .iter()
            .filter(|f| **f != canonical)
            .cloned()
            .collect();

        groups.push(DuplicateGroup {
            function_name: method_name.clone(),
            canonical_file: canonical,
            remove_from,
        });
    }

    groups.sort_by(|a, b| a.function_name.cmp(&b.function_name));
    groups
}

/// Detect duplicated functions across all fingerprinted files.
///
/// Groups functions by their body hash. When two or more files contain a
/// function with the same name and the same normalized body hash, a finding
/// is emitted for each location.
pub fn detect_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let hash_groups = build_groups(fingerprints);
    let mut findings = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        let suggestion = format!(
            "Function `{}` has identical body in {} files. \
             Extract to a shared module and import it.",
            method_name,
            locations.len()
        );

        // Emit one finding per file that has the duplicate
        for file in locations {
            let also_in = locations
                .iter()
                .filter(|f| *f != file)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");

            findings.push(Finding {
                convention: "duplication".to_string(),
                severity: Severity::Warning,
                file: file.clone(),
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

    // ========================================================================
    // DuplicateGroup / canonical selection tests
    // ========================================================================

    #[test]
    fn group_picks_canonical_by_shortest_path() {
        let fp1 = make_fingerprint("src/core/deep/nested/helper.rs", &["foo"], &[("foo", "h1")]);
        let fp2 = make_fingerprint("src/utils.rs", &["foo"], &[("foo", "h1")]);

        let groups = detect_duplicate_groups(&[&fp1, &fp2]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_file, "src/utils.rs");
        assert_eq!(groups[0].remove_from, vec!["src/core/deep/nested/helper.rs"]);
    }

    #[test]
    fn group_prefers_utils_directory() {
        let fp1 = make_fingerprint("src/core/a.rs", &["shared"], &[("shared", "h1")]);
        let fp2 = make_fingerprint("src/utils/helpers.rs", &["shared"], &[("shared", "h1")]);

        let groups = detect_duplicate_groups(&[&fp1, &fp2]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_file, "src/utils/helpers.rs");
        assert_eq!(groups[0].remove_from, vec!["src/core/a.rs"]);
    }

    #[test]
    fn group_alphabetical_tiebreaker() {
        let fp1 = make_fingerprint("src/b.rs", &["dup"], &[("dup", "h1")]);
        let fp2 = make_fingerprint("src/a.rs", &["dup"], &[("dup", "h1")]);

        let groups = detect_duplicate_groups(&[&fp1, &fp2]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_file, "src/a.rs");
    }

    #[test]
    fn group_three_way_has_two_removals() {
        let fp1 = make_fingerprint("src/a.rs", &["f"], &[("f", "h")]);
        let fp2 = make_fingerprint("src/b.rs", &["f"], &[("f", "h")]);
        let fp3 = make_fingerprint("src/c.rs", &["f"], &[("f", "h")]);

        let groups = detect_duplicate_groups(&[&fp1, &fp2, &fp3]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].remove_from.len(), 2);
        assert!(!groups[0].remove_from.contains(&groups[0].canonical_file));
    }
}
