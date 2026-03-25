//! near_duplicate_detection — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// Build structural hash groups from fingerprints.
///
/// Groups functions by (name, structural_hash), returning only groups
/// where the exact body hashes differ (otherwise they'd already be caught
/// by the exact-duplicate detector).
pub(crate) fn build_structural_groups(
    fingerprints: &[&FileFingerprint],
) -> HashMap<(String, String), Vec<(String, String)>> {
    // Collect: (fn_name, structural_hash) → [(file, body_hash), ...]
    let mut groups: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();

    for fp in fingerprints {
        for (method_name, struct_hash) in &fp.structural_hashes {
            groups
                .entry((method_name.clone(), struct_hash.clone()))
                .or_default()
                .push((
                    fp.relative_path.clone(),
                    fp.method_hashes
                        .get(method_name)
                        .cloned()
                        .unwrap_or_default(),
                ));
        }
    }

    groups
}

/// Check if a file path looks like a CLI command module.
///
/// Command modules (`src/commands/*.rs`) are expected to have identically-
/// named functions (`run`, `list`, etc.) with completely different bodies.
pub(crate) fn is_command_file(path: &str) -> bool {
    path.contains("/commands/") || path.starts_with("commands/")
}

/// Count the body lines of a function in a file's structural hash data.
///
/// Uses heuristic: count lines in the content between `fn <name>` and the
/// matching closing brace. Returns 0 if function not found or content empty.
pub(crate) fn count_body_lines(fp: &FileFingerprint, method_name: &str) -> usize {
    let pattern = format!("fn {}", method_name);
    let lines: Vec<&str> = fp.content.lines().collect();
    let mut start = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&pattern) {
            start = Some(i);
            break;
        }
    }

    let Some(start_idx) = start else { return 0 };

    let mut brace_depth = 0i32;
    let mut found_open = false;
    for (offset, line) in lines[start_idx..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }
        if found_open && brace_depth == 0 {
            return offset + 1;
        }
    }

    0
}

/// Detect structural near-duplicates across all fingerprinted files.
///
/// Groups functions by (name, structural_hash). When two or more files
/// contain a function with the same name and the same structural hash
/// but *different* exact body hashes, it means the functions have
/// identical control flow but differ in identifiers/constants.
///
/// Filters out:
/// - Functions already caught by exact-duplicate detection
/// - Generic names (`run`, `list`, `show`, etc.)
/// - Command/core delegation pairs (command module ↔ core module)
/// - Trivial functions (< 3 body lines)
pub fn detect_near_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let structural_groups = build_structural_groups(fingerprints);
    let exact_groups = build_groups(fingerprints);

    // Collect exact-duplicate (name, hash) pairs for exclusion
    let exact_duplicate_names: std::collections::HashSet<String> = exact_groups
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_DUPLICATE_LOCATIONS)
        .map(|((name, _), _)| name.clone())
        .collect();

    let mut findings = Vec::new();

    for ((method_name, _struct_hash), file_hashes) in &structural_groups {
        // Need at least 2 locations
        if file_hashes.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        // Skip if already an exact duplicate
        if exact_duplicate_names.contains(method_name) {
            continue;
        }

        // Skip generic names
        if GENERIC_NAMES.contains(&method_name.as_str()) {
            continue;
        }

        // Check that exact hashes actually differ (otherwise exact detection covers it)
        let unique_body_hashes: std::collections::HashSet<&str> =
            file_hashes.iter().map(|(_, h)| h.as_str()).collect();
        if unique_body_hashes.len() < 2 {
            continue;
        }

        let files: Vec<&str> = file_hashes.iter().map(|(f, _)| f.as_str()).collect();

        // Filter: skip if all files are command modules (delegation pattern)
        if files.iter().all(|f| is_command_file(f)) {
            continue;
        }

        // Filter: skip command↔core pairs where one is in commands/ and another in core/
        // These are the delegation pattern — the command calls the core function.
        let has_command = files.iter().any(|f| is_command_file(f));
        let has_non_command = files.iter().any(|f| !is_command_file(f));
        if has_command && has_non_command && files.len() == 2 {
            continue;
        }

        // Filter: skip trivial functions (< MIN_BODY_LINES)
        let body_lines: Vec<usize> = files
            .iter()
            .filter_map(|file_path| {
                fingerprints
                    .iter()
                    .find(|fp| fp.relative_path == *file_path)
                    .map(|fp| count_body_lines(fp, method_name))
            })
            .collect();
        if body_lines.iter().all(|&l| l < MIN_BODY_LINES) {
            continue;
        }

        let suggestion = format!(
            "Function `{}` has identical structure in {} files but different \
             identifiers/constants. Consider extracting shared logic into a \
             parameterized function.",
            method_name,
            files.len()
        );

        for (file, _body_hash) in file_hashes {
            let mut also_in_vec: Vec<&str> = file_hashes
                .iter()
                .filter(|(f, _)| f != file)
                .map(|(f, _)| f.as_str())
                .collect();
            also_in_vec.sort();
            let also_in = also_in_vec.join(", ");

            findings.push(Finding {
                convention: "near-duplication".to_string(),
                severity: Severity::Info,
                file: file.clone(),
                description: format!(
                    "Near-duplicate `{}` — structurally identical to {}",
                    method_name, also_in
                ),
                suggestion: suggestion.clone(),
                kind: AuditFinding::NearDuplicate,
            });
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}
