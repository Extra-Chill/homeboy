//! Duplication detection — find identical and near-identical functions across
//! source files.
//!
//! Uses method body hashes from fingerprinting to detect exact duplicates,
//! and structural hashes (identifiers/literals normalized to positional tokens)
//! to detect near-duplicates — functions with identical control flow that differ
//! only in variable names, constant references, or string values.
//!
//! Three outputs:
//! - `detect_duplicates()` → flat `Vec<Finding>` for exact duplicates
//! - `detect_duplicate_groups()` → structured `Vec<DuplicateGroup>` for the fixer
//! - `detect_near_duplicates()` → flat `Vec<Finding>` for structural near-duplicates

use std::collections::HashMap;

use super::conventions::DeviationKind;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

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
                kind: DeviationKind::DuplicateFunction,
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

// ============================================================================
// Near-Duplicate Detection (structural similarity)
// ============================================================================

/// Names that are too generic to flag as near-duplicates.
/// These appear in many files with completely unrelated implementations.
const GENERIC_NAMES: &[&str] = &[
    "run", "new", "default", "build", "list", "show", "set", "get", "delete", "remove", "clear",
    "create", "update", "status", "search", "find", "read", "write", "rename", "init", "test",
    "fmt", "from", "into", "clone", "drop", "display", "parse", "validate", "execute", "handle",
    "process", "merge", "resolve", "pin", "plan",
];

/// Minimum body line count — skip trivial functions (1-2 line bodies).
/// Functions like `fn default_true() -> bool { true }` are too small
/// to meaningfully refactor into shared code with a parameter.
const MIN_BODY_LINES: usize = 3;

/// Build structural hash groups from fingerprints.
///
/// Groups functions by (name, structural_hash), returning only groups
/// where the exact body hashes differ (otherwise they'd already be caught
/// by the exact-duplicate detector).
fn build_structural_groups(
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
fn is_command_file(path: &str) -> bool {
    path.contains("/commands/") || path.starts_with("commands/")
}

/// Count the body lines of a function in a file's structural hash data.
///
/// Uses heuristic: count lines in the content between `fn <name>` and the
/// matching closing brace. Returns 0 if function not found or content empty.
fn count_body_lines(fp: &FileFingerprint, method_name: &str) -> usize {
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
                kind: DeviationKind::NearDuplicate,
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;

    fn make_fingerprint(path: &str, methods: &[&str], hashes: &[(&str, &str)]) -> FileFingerprint {
        make_fingerprint_with_structural(path, methods, hashes, &[])
    }

    fn make_fingerprint_with_structural(
        path: &str,
        methods: &[&str],
        hashes: &[(&str, &str)],
        structural: &[(&str, &str)],
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.iter().map(|s| s.to_string()).collect(),
            registrations: vec![],
            type_name: None,
            extends: None,
            implements: vec![],
            namespace: None,
            imports: vec![],
            content: String::new(),
            method_hashes: hashes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            structural_hashes: structural
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            visibility: std::collections::HashMap::new(),
            properties: vec![],
            hooks: vec![],
            unused_parameters: vec![],
            dead_code_markers: vec![],
            internal_calls: vec![],
            public_api: vec![],
        }
    }

    #[test]
    fn detects_exact_duplicate() {
        let fp1 = make_fingerprint("src/utils/io.rs", &["is_zero"], &[("is_zero", "abc123")]);
        let fp2 = make_fingerprint(
            "src/utils/validation.rs",
            &["is_zero"],
            &[("is_zero", "abc123")],
        );

        let findings = detect_duplicates(&[&fp1, &fp2]);

        assert_eq!(findings.len(), 2, "Should emit one finding per location");
        assert!(findings
            .iter()
            .all(|f| f.kind == DeviationKind::DuplicateFunction));
        assert!(findings.iter().any(|f| f.file == "src/utils/io.rs"));
        assert!(findings.iter().any(|f| f.file == "src/utils/validation.rs"));
        assert!(findings[0].description.contains("is_zero"));
    }

    #[test]
    fn no_duplicates_different_hashes() {
        let fp1 = make_fingerprint("src/a.rs", &["process"], &[("process", "hash_a")]);
        let fp2 = make_fingerprint("src/b.rs", &["process"], &[("process", "hash_b")]);

        let findings = detect_duplicates(&[&fp1, &fp2]);
        assert!(
            findings.is_empty(),
            "Different hashes should not flag duplicates"
        );
    }

    #[test]
    fn no_duplicates_single_location() {
        let fp = make_fingerprint("src/only.rs", &["unique_fn"], &[("unique_fn", "abc123")]);

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
        assert!(
            findings.is_empty(),
            "No hashes means no duplication findings"
        );
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
        assert_eq!(
            groups[0].remove_from,
            vec!["src/core/deep/nested/helper.rs"]
        );
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

    // ========================================================================
    // Near-duplicate detection tests
    // ========================================================================

    /// Helper to build a fingerprint with content for body-line counting.
    fn make_fp_with_content(
        path: &str,
        content: &str,
        hashes: &[(&str, &str)],
        structural: &[(&str, &str)],
    ) -> FileFingerprint {
        let mut fp = make_fingerprint_with_structural(path, &[], hashes, structural);
        fp.content = content.to_string();
        fp
    }

    #[test]
    fn near_duplicate_detected_when_structural_match_but_exact_differs() {
        // cache_path in two files: same structure, different constants
        let content_a = "fn cache_path() -> Option<PathBuf> {\n    paths::homeboy().ok().map(|p| p.join(CACHE_A))\n}\n";
        let content_b = "fn cache_path() -> Option<PathBuf> {\n    paths::homeboy().ok().map(|p| p.join(CACHE_B))\n}\n";

        let fp1 = make_fp_with_content(
            "src/core/update_check.rs",
            content_a,
            &[("cache_path", "hash_a")],
            &[("cache_path", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/ext_update_check.rs",
            content_b,
            &[("cache_path", "hash_b")],
            &[("cache_path", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);

        assert_eq!(findings.len(), 2, "Should flag both locations");
        assert!(findings
            .iter()
            .all(|f| f.kind == DeviationKind::NearDuplicate));
        assert!(findings[0].description.contains("cache_path"));
        assert_eq!(findings[0].severity, Severity::Info);
    }

    #[test]
    fn near_duplicate_skips_exact_duplicates() {
        // If exact hashes match, exact-duplicate detector already handles it
        let fp1 = make_fingerprint_with_structural(
            "src/a.rs",
            &["helper"],
            &[("helper", "SAME")],
            &[("helper", "SAME_STRUCT")],
        );
        let fp2 = make_fingerprint_with_structural(
            "src/b.rs",
            &["helper"],
            &[("helper", "SAME")],
            &[("helper", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Exact duplicates should be excluded");
    }

    #[test]
    fn near_duplicate_skips_generic_names() {
        let content = "fn run() {\n    do_something();\n    do_more();\n}\n";
        let fp1 = make_fp_with_content(
            "src/core/a.rs",
            content,
            &[("run", "hash_a")],
            &[("run", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/b.rs",
            content,
            &[("run", "hash_b")],
            &[("run", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(
            findings.is_empty(),
            "'run' is a generic name — should be skipped"
        );
    }

    #[test]
    fn near_duplicate_skips_command_core_pairs() {
        let content = "fn deploy_site() {\n    connect();\n    upload();\n    verify();\n}\n";
        let fp1 = make_fp_with_content(
            "src/commands/deploy.rs",
            content,
            &[("deploy_site", "hash_a")],
            &[("deploy_site", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/deploy.rs",
            content,
            &[("deploy_site", "hash_b")],
            &[("deploy_site", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Command↔core pair should be skipped");
    }

    #[test]
    fn near_duplicate_skips_trivial_functions() {
        // default_true is only 1 line — too trivial to refactor
        let content = "fn default_true() -> bool { true }\n";
        let fp1 = make_fp_with_content(
            "src/core/defaults.rs",
            content,
            &[("default_true", "hash_a")],
            &[("default_true", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/project.rs",
            content,
            &[("default_true", "hash_b")],
            &[("default_true", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "Trivial functions should be skipped");
    }

    #[test]
    fn near_duplicate_not_skipped_for_multi_line_core_functions() {
        // Non-trivial functions in core/ (not commands/) SHOULD be flagged
        let content = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy()?;\n    let file = base.join(FILENAME);\n    Some(file)\n}\n";
        let fp1 = make_fp_with_content(
            "src/core/update.rs",
            content,
            &[("cache_path", "hash_a")],
            &[("cache_path", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/core/ext_update.rs",
            content,
            &[("cache_path", "hash_b")],
            &[("cache_path", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert_eq!(
            findings.len(),
            2,
            "Non-trivial core↔core near-duplicates should be flagged"
        );
    }

    #[test]
    fn near_duplicate_skips_all_command_files() {
        // Multiple command files with same structural hash — normal pattern
        let content = "fn components() {\n    let list = config::list();\n    for item in list {\n        output::print(item);\n    }\n}\n";
        let fp1 = make_fp_with_content(
            "src/commands/fleet.rs",
            content,
            &[("components", "hash_a")],
            &[("components", "SAME_STRUCT")],
        );
        let fp2 = make_fp_with_content(
            "src/commands/project.rs",
            content,
            &[("components", "hash_b")],
            &[("components", "SAME_STRUCT")],
        );

        let findings = detect_near_duplicates(&[&fp1, &fp2]);
        assert!(findings.is_empty(), "All-commands group should be skipped");
    }
}
