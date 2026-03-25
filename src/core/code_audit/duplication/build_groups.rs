//! build_groups — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::DuplicateGroup;
use super::MIN_DUPLICATE_LOCATIONS;


/// Build grouped duplication data from fingerprints.
///
/// For each group of identical functions, picks a canonical file (shortest
/// path, then alphabetical) and lists the rest as removal targets.
pub(crate) fn build_groups(fingerprints: &[&FileFingerprint]) -> HashMap<(String, String), Vec<String>> {
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
pub(crate) fn pick_canonical(locations: &[String]) -> String {
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
        let mut remove_from: Vec<String> = locations
            .iter()
            .filter(|f| **f != canonical)
            .cloned()
            .collect();
        remove_from.sort();

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
/// Detect exact function body duplicates across files.
///
/// `convention_methods` are excluded — identical implementations across convention-
/// following files are expected behavior (e.g. `__construct`, `checkPermission`,
/// interface methods with identical bodies).
pub fn detect_duplicates(
    fingerprints: &[&FileFingerprint],
    convention_methods: &std::collections::HashSet<String>,
) -> Vec<Finding> {
    let hash_groups = build_groups(fingerprints);
    let mut findings = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        // Skip convention-expected methods — identical implementations are by design.
        if convention_methods.contains(method_name) {
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
            let mut also_in_vec: Vec<_> =
                locations.iter().filter(|f| *f != file).cloned().collect();
            also_in_vec.sort();
            let also_in = also_in_vec.join(", ");

            findings.push(Finding {
                convention: "duplication".to_string(),
                severity: Severity::Warning,
                file: file.clone(),
                description: format!("Duplicate function `{}` — also in {}", method_name, also_in),
                suggestion: suggestion.clone(),
                kind: AuditFinding::DuplicateFunction,
            });
        }
    }

    // Sort by file path then description for deterministic output
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}
