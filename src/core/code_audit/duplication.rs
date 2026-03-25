//! Duplication detection — find identical and near-identical functions across
//! source files, and duplicated code blocks within a single method.
//!
//! Uses method body hashes from fingerprinting to detect exact duplicates,
//! and structural hashes (identifiers/literals normalized to positional tokens)
//! to detect near-duplicates — functions with identical control flow that differ
//! only in variable names, constant references, or string values.
//!
//! Four outputs:
//! - `detect_duplicates()` → flat `Vec<Finding>` for exact duplicates
//! - `detect_duplicate_groups()` → structured `Vec<DuplicateGroup>` for the fixer
//! - `detect_near_duplicates()` → flat `Vec<Finding>` for structural near-duplicates
//! - `detect_intra_method_duplicates()` → duplicated blocks within a single method

mod build_groups;
mod constants;
mod intra_method_duplication;
mod near_duplicate_detection;

pub use build_groups::*;
pub use constants::*;
pub use intra_method_duplication::*;
pub use near_duplicate_detection::*;


use std::collections::HashMap;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

// ============================================================================
// Near-Duplicate Detection (structural similarity)
// ============================================================================

// ============================================================================
// Intra-Method Duplication Detection
// ============================================================================

/// Check if a line is comment-only (PHP, Rust, or shell style).
fn is_comment_only(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('#')
}

/// Normalize a line for hashing: collapse whitespace, lowercase.
fn normalize_line(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// ============================================================================
// Parallel Implementation Detection (call-sequence similarity)
// ============================================================================

/// Minimum number of function calls in a method body to consider it for
/// parallel implementation detection. Trivial methods (< 4 calls) are
/// too simple to meaningfully abstract.
const MIN_CALL_COUNT: usize = 4;

/// Minimum Jaccard similarity (|intersection| / |union|) between two
/// call sets to flag as a parallel implementation.
const MIN_JACCARD_SIMILARITY: f64 = 0.5;

/// Minimum longest-common-subsequence ratio to flag as parallel.
/// This captures sequential ordering — two methods that call helpers
/// in the same order score higher than ones that share calls but in
/// a different order.
const MIN_LCS_RATIO: f64 = 0.5;

/// Minimum number of shared (intersecting) calls between two methods
/// to flag as a parallel implementation. This prevents false positives
/// from methods that share only 1-2 trivial calls like `to_string`.
const MIN_SHARED_CALLS: usize = 3;

/// Ubiquitous stdlib/trait method calls that appear in almost every function
/// and carry no signal for parallel implementation detection. Two functions
/// both calling `.to_string()` does not mean they implement the same workflow.
const TRIVIAL_CALLS: &[&str] = &[
    "to_string",
    "to_owned",
    "to_lowercase",
    "to_uppercase",
    "clone",
    "default",
    "new",
    "len",
    "is_empty",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "expect",
    "as_str",
    "as_ref",
    "as_deref",
    "into",
    "from",
    "iter",
    "into_iter",
    "collect",
    "map",
    "filter",
    "any",
    "all",
    "find",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "extend",
    "join",
    "split",
    "trim",
    "starts_with",
    "ends_with",
    "strip_prefix",
    "strip_suffix",
    "replace",
    "display",
    "write",
    "read",
    "flush",
    "ok",
    "err",
    "map_err",
    "and_then",
    "or_else",
    "flatten",
    "take",
    "skip",
    "chain",
    "zip",
    "enumerate",
    "cloned",
    "copied",
    "rev",
    "sort",
    "sort_by",
    "dedup",
    "retain",
    "get",
    "set",
    "entry",
    "or_insert",
    "or_insert_with",
    "keys",
    "values",
    "exists",
    "parent",
    "file_name",
    "extension",
    "with_extension",
];

/// Per-method call sequence extracted from file content.
#[derive(Debug)]
struct MethodCallSequence {
    file: String,
    method: String,
    /// Ordered list of function/method calls made in the body.
    calls: Vec<String>,
}

/// Extract function call names from a code block.
///
/// Matches patterns like `function_name(`, `self.method(`, `Type::method(`.
/// Returns the called name (without receiver/namespace prefix).
fn extract_calls_from_body(body: &str) -> Vec<String> {
    let mut calls = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        // Skip comments
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        // Find all `identifier(` patterns
        let chars: Vec<char> = trimmed.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // Look for `(`
            if chars[i] == '(' && i > 0 {
                // Walk backwards to find the identifier
                let end = i;
                let mut start = i;
                while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
                    start -= 1;
                }
                if start < end {
                    let name: String = chars[start..end].iter().collect();
                    // Skip language keywords, control flow, and trivial stdlib calls
                    if !is_keyword(&name)
                        && !name.is_empty()
                        && !TRIVIAL_CALLS.contains(&name.as_str())
                    {
                        calls.push(name);
                    }
                }
            }
            i += 1;
        }
    }

    calls
}

/// Check if a name is a language keyword (not a function call).
fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "let"
            | "mut"
            | "const"
            | "fn"
            | "pub"
            | "use"
            | "mod"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "type"
            | "where"
            | "self"
            | "Self"
            | "super"
            | "crate"
            | "as"
            | "in"
            | "ref"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "true"
            | "false"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "println"
            | "eprintln"
            | "format"
            | "vec"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "panic"
            | "dbg"
    )
}

/// Extract per-method call sequences from all fingerprints.
fn extract_call_sequences(fingerprints: &[&FileFingerprint]) -> Vec<MethodCallSequence> {
    let mut sequences = Vec::new();

    for fp in fingerprints {
        if fp.content.is_empty() {
            continue;
        }

        // Skip test files entirely — test code is expected to mirror production
        // call patterns and flagging it as "parallel implementation" is noise.
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }

        let lines: Vec<&str> = fp.content.lines().collect();

        for method_name in &fp.methods {
            // Skip generic names — they're expected to have similar call patterns
            if GENERIC_NAMES.contains(&method_name.as_str()) {
                continue;
            }

            // Skip test methods (inline #[cfg(test)] modules)
            if method_name.starts_with("test_") {
                continue;
            }

            let Some((body_start, body_end)) = find_method_body(&lines, method_name) else {
                continue;
            };

            if body_start + 1 >= body_end {
                continue;
            }

            let body: String = lines[body_start + 1..body_end].join("\n");
            let calls = extract_calls_from_body(&body);

            if calls.len() >= MIN_CALL_COUNT {
                sequences.push(MethodCallSequence {
                    file: fp.relative_path.clone(),
                    method: method_name.clone(),
                    calls,
                });
            }
        }
    }

    sequences
}

/// Compute Jaccard similarity between two sets.
fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    let set_a: std::collections::HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(|s| s.as_str()).collect();

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Compute longest common subsequence length between two sequences.
fn lcs_length(a: &[String], b: &[String]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    dp[m][n]
}

/// Compute LCS ratio: 2 * LCS / (len(a) + len(b)).
fn lcs_ratio(a: &[String], b: &[String]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 0.0;
    }
    2.0 * lcs_length(a, b) as f64 / total as f64
}

/// Detect parallel implementations across files.
///
/// Compares all method pairs (in different files) by their call sequences.
/// When two methods make a similar set of calls in a similar order — but
/// have different names and different exact implementations — they're
/// likely parallel implementations of the same workflow that should be
/// abstracted into a shared parameterized function.
///
/// Filters out:
/// - Methods in the same file
/// - Generic names (run, new, build, etc.)
/// - Methods with fewer than MIN_CALL_COUNT calls
/// - Pairs already caught by exact or near-duplicate detection
/// - Pairs below both similarity thresholds
/// Detect parallel implementations — methods with similar call patterns across files.
///
/// `convention_methods` contains method names that are expected by discovered conventions.
/// When both methods in a pair are convention-expected, the pair is skipped — similar call
/// patterns are the expected behavior for convention-following code, not a finding.
pub(crate) fn detect_parallel_implementations(
    fingerprints: &[&FileFingerprint],
    convention_methods: &std::collections::HashSet<String>,
) -> Vec<Finding> {
    let sequences = extract_call_sequences(fingerprints);

    // Build sets of already-flagged pairs (exact + near duplicates) to avoid double-flagging
    let exact_groups = build_groups(fingerprints);
    let exact_dup_fns: std::collections::HashSet<String> = exact_groups
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_DUPLICATE_LOCATIONS)
        .map(|((name, _), _)| name.clone())
        .collect();

    let mut findings = Vec::new();
    let mut reported_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();

    for i in 0..sequences.len() {
        for j in (i + 1)..sequences.len() {
            let a = &sequences[i];
            let b = &sequences[j];

            // Skip same file
            if a.file == b.file {
                continue;
            }

            // Skip if same function name (already caught by other detectors)
            if a.method == b.method {
                continue;
            }

            // Skip if either function is an exact duplicate
            if exact_dup_fns.contains(&a.method) || exact_dup_fns.contains(&b.method) {
                continue;
            }

            // Skip if either method is convention-expected — its call pattern is shaped
            // by the convention, so similar patterns with other methods are expected.
            if convention_methods.contains(&a.method) || convention_methods.contains(&b.method) {
                continue;
            }

            // Skip already-reported pairs (both directions)
            let pair_key = if a.file < b.file || (a.file == b.file && a.method < b.method) {
                (
                    format!("{}::{}", a.file, a.method),
                    format!("{}::{}", b.file, b.method),
                )
            } else {
                (
                    format!("{}::{}", b.file, b.method),
                    format!("{}::{}", a.file, a.method),
                )
            };
            if reported_pairs.contains(&pair_key) {
                continue;
            }

            let jaccard = jaccard_similarity(&a.calls, &b.calls);
            let lcs = lcs_ratio(&a.calls, &b.calls);

            if jaccard >= MIN_JACCARD_SIMILARITY && lcs >= MIN_LCS_RATIO {
                // Find the shared calls for the description
                let set_a: std::collections::HashSet<&str> =
                    a.calls.iter().map(|s| s.as_str()).collect();
                let set_b: std::collections::HashSet<&str> =
                    b.calls.iter().map(|s| s.as_str()).collect();
                let mut shared: Vec<&&str> = set_a.intersection(&set_b).collect();

                // Require a minimum absolute number of shared calls.
                // Jaccard/LCS alone can trigger on tiny overlaps (2 shared out of 4 total).
                if shared.len() < MIN_SHARED_CALLS {
                    continue;
                }

                reported_pairs.insert(pair_key);
                shared.sort();
                let shared_preview: String = shared
                    .iter()
                    .take(5)
                    .map(|s| format!("`{}`", s))
                    .collect::<Vec<_>>()
                    .join(", ");
                let extra = if shared.len() > 5 {
                    format!(" (+{} more)", shared.len() - 5)
                } else {
                    String::new()
                };

                let suggestion = format!(
                    "`{}` and `{}` follow the same call pattern (Jaccard: {:.0}%, sequence: {:.0}%). \
                     Consider extracting the shared workflow into a parameterized function.",
                    a.method,
                    b.method,
                    jaccard * 100.0,
                    lcs * 100.0
                );

                // Emit finding for file A
                findings.push(Finding {
                    convention: "parallel-implementation".to_string(),
                    severity: Severity::Info,
                    file: a.file.clone(),
                    description: format!(
                        "Parallel implementation: `{}` has similar call pattern to `{}` in {} — shared calls: {}{}",
                        a.method, b.method, b.file, shared_preview, extra
                    ),
                    suggestion: suggestion.clone(),
                    kind: AuditFinding::ParallelImplementation,
                });

                // Emit finding for file B
                findings.push(Finding {
                    convention: "parallel-implementation".to_string(),
                    severity: Severity::Info,
                    file: b.file.clone(),
                    description: format!(
                        "Parallel implementation: `{}` has similar call pattern to `{}` in {} — shared calls: {}{}",
                        b.method, a.method, a.file, shared_preview, extra
                    ),
                    suggestion,
                    kind: AuditFinding::ParallelImplementation,
                });
            }
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
            method_hashes: hashes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            structural_hashes: structural
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
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

        let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());

        assert_eq!(findings.len(), 2, "Should emit one finding per location");
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::DuplicateFunction));
        assert!(findings.iter().any(|f| f.file == "src/utils/io.rs"));
        assert!(findings.iter().any(|f| f.file == "src/utils/validation.rs"));
        assert!(findings[0].description.contains("is_zero"));
    }

    #[test]
    fn no_duplicates_different_hashes() {
        let fp1 = make_fingerprint("src/a.rs", &["process"], &[("process", "hash_a")]);
        let fp2 = make_fingerprint("src/b.rs", &["process"], &[("process", "hash_b")]);

        let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());
        assert!(
            findings.is_empty(),
            "Different hashes should not flag duplicates"
        );
    }

    #[test]
    fn no_duplicates_single_location() {
        let fp = make_fingerprint("src/only.rs", &["unique_fn"], &[("unique_fn", "abc123")]);

        let findings = detect_duplicates(&[&fp], &std::collections::HashSet::new());
        assert!(findings.is_empty(), "Single location is not a duplicate");
    }

    #[test]
    fn three_way_duplicate() {
        let fp1 = make_fingerprint("src/a.rs", &["helper"], &[("helper", "same_hash")]);
        let fp2 = make_fingerprint("src/b.rs", &["helper"], &[("helper", "same_hash")]);
        let fp3 = make_fingerprint("src/c.rs", &["helper"], &[("helper", "same_hash")]);

        let findings = detect_duplicates(&[&fp1, &fp2, &fp3], &std::collections::HashSet::new());

        assert_eq!(findings.len(), 3, "Should flag all 3 locations");
        assert!(findings[0].suggestion.contains("3 files"));
    }

    #[test]
    fn empty_method_hashes_no_findings() {
        let fp1 = make_fingerprint("src/a.rs", &["foo", "bar"], &[]);
        let fp2 = make_fingerprint("src/b.rs", &["foo", "bar"], &[]);

        let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());
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

        let findings = detect_duplicates(&[&fp1, &fp2], &std::collections::HashSet::new());

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
            .all(|f| f.kind == AuditFinding::NearDuplicate));
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

    // ========================================================================
    // Intra-method duplication tests
    // ========================================================================

    #[test]
    fn intra_method_detects_duplicated_block() {
        // Simulate a merge artifact: same 5-line block appears twice
        let content = "<?php\nclass PipelineSteps {\n    public function handle_update( $request ) {\n        $config = array();\n        $has_provider = $request->has_param( 'provider' );\n        $has_model = $request->has_param( 'model' );\n        $has_prompt = $request->has_param( 'system_prompt' );\n        $has_disabled = $request->has_param( 'disabled_tools' );\n        $has_key = $request->has_param( 'ai_api_key' );\n\n        if ( $has_provider ) {\n            $config['provider'] = sanitize_text_field( $request->get_param( 'provider' ) );\n        }\n\n        $has_provider = $request->has_param( 'provider' );\n        $has_model = $request->has_param( 'model' );\n        $has_prompt = $request->has_param( 'system_prompt' );\n        $has_disabled = $request->has_param( 'disabled_tools' );\n        $has_key = $request->has_param( 'ai_api_key' );\n\n        if ( $has_provider ) {\n            $config['provider'] = sanitize_text_field( $request->get_param( 'provider' ) );\n        }\n\n        return $config;\n    }\n}\n";

        let mut fp = make_fingerprint(
            "inc/Api/Pipelines/PipelineSteps.php",
            &["handle_update"],
            &[],
        );
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);

        assert!(
            !findings.is_empty(),
            "Should detect duplicated block within handle_update"
        );
        assert!(findings[0].kind == AuditFinding::IntraMethodDuplicate);
        assert!(findings[0].description.contains("handle_update"));
    }

    #[test]
    fn intra_method_no_false_positive_on_unique_code() {
        let content = "<?php\nclass Handler {\n    public function process( $data ) {\n        $name = sanitize_text_field( $data['name'] );\n        $email = sanitize_email( $data['email'] );\n        $phone = sanitize_text_field( $data['phone'] );\n        $address = sanitize_text_field( $data['address'] );\n        $city = sanitize_text_field( $data['city'] );\n\n        $result = $this->save( $name, $email );\n        $this->notify( $result );\n        $this->log_action( $result );\n        $this->update_cache( $result );\n        $this->send_confirmation( $email );\n\n        return $result;\n    }\n}\n";

        let mut fp = make_fingerprint("inc/Handler.php", &["process"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            findings.is_empty(),
            "Unique code should not trigger intra-method duplication"
        );
    }

    #[test]
    fn intra_method_skips_short_methods() {
        let content = "fn short() {\n    let a = 1;\n    let b = 2;\n    let c = a + b;\n    println!(\"{}\", c);\n}\n";

        let mut fp = make_fingerprint("src/short.rs", &["short"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(findings.is_empty(), "Short methods should be skipped");
    }

    #[test]
    fn intra_method_rust_function_duplicated_block() {
        let content = "fn process_items(items: &[Item]) -> Vec<Result> {\n    let mut results = Vec::new();\n    let config = load_config();\n    let validator = Validator::new(&config);\n    let processor = Processor::new(&config);\n    let output = processor.run(&items[0]);\n\n    results.push(output);\n\n    let config = load_config();\n    let validator = Validator::new(&config);\n    let processor = Processor::new(&config);\n    let output = processor.run(&items[0]);\n\n    results.push(output);\n\n    results\n}\n";

        let mut fp = make_fingerprint("src/core/pipeline.rs", &["process_items"], &[]);
        fp.content = content.to_string();

        let findings = detect_intra_method_duplicates(&[&fp]);
        assert!(
            !findings.is_empty(),
            "Should detect duplicated block in Rust function"
        );
    }

    #[test]
    fn find_method_body_php() {
        let content =
            "<?php\nclass Foo {\n    public function bar() {\n        return 1;\n    }\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "bar");
        assert!(result.is_some());
        let (open, close) = result.unwrap();
        assert!(lines[open].contains('{'));
        assert!(lines[close].contains('}'));
    }

    #[test]
    fn find_method_body_rust() {
        let content = "fn hello() {\n    println!(\"hi\");\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "hello");
        assert!(result.is_some());
    }

    #[test]
    fn find_method_body_missing() {
        let content = "fn other() {\n    println!(\"hi\");\n}\n";
        let lines: Vec<&str> = content.lines().collect();
        let result = find_method_body(&lines, "nonexistent");
        assert!(result.is_none());
    }

    // ========================================================================
    // Parallel Implementation Detection tests
    // ========================================================================

    fn make_fingerprint_with_content(
        path: &str,
        methods: &[&str],
        content: &str,
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.iter().map(|s| s.to_string()).collect(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_parallel_implementation() {
        let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["deploy_to_server"],
            "fn deploy_to_server() {\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    notify_complete();\n}",
        );
        let fp2 = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["upgrade_on_server"],
            "fn upgrade_on_server() {\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    send_notification();\n}",
        );

        let findings =
            detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new());

        assert_eq!(findings.len(), 2, "Should emit one finding per file");
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::ParallelImplementation));
        assert!(findings.iter().any(|f| f.file == "src/deploy.rs"));
        assert!(findings.iter().any(|f| f.file == "src/upgrade.rs"));
    }

    #[test]
    fn no_parallel_for_unrelated_functions() {
        let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["deploy_to_server"],
            "fn deploy_to_server() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
        );
        let fp2 = make_fingerprint_with_content(
            "src/parser.rs",
            &["parse_config"],
            "fn parse_config() {\n    read_file();\n    tokenize();\n    parse_ast();\n    validate_schema();\n}",
        );

        let findings =
            detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new());
        assert!(
            findings.is_empty(),
            "Completely different call sets should not flag"
        );
    }

    #[test]
    fn no_parallel_for_same_file() {
        let fp = make_fingerprint_with_content(
            "src/ops.rs",
            &["deploy_op", "upgrade_op"],
            "fn deploy_op() {\n    validate();\n    build();\n    upload();\n    notify();\n}\nfn upgrade_op() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
        );

        let findings = detect_parallel_implementations(&[&fp], &std::collections::HashSet::new());
        assert!(
            findings.is_empty(),
            "Same-file methods should not be flagged as parallel"
        );
    }

    #[test]
    fn no_parallel_for_trivial_methods() {
        let fp1 = make_fingerprint_with_content(
            "src/a.rs",
            &["small_a"],
            "fn small_a() {\n    foo();\n    bar();\n}",
        );
        let fp2 = make_fingerprint_with_content(
            "src/b.rs",
            &["small_b"],
            "fn small_b() {\n    foo();\n    bar();\n}",
        );

        let findings =
            detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new());
        assert!(
            findings.is_empty(),
            "Methods with < MIN_CALL_COUNT calls should be skipped"
        );
    }

    #[test]
    fn no_parallel_for_generic_names() {
        // "run" is in GENERIC_NAMES
        let fp1 = make_fingerprint_with_content(
            "src/a.rs",
            &["run"],
            "fn run() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
        );
        let fp2 = make_fingerprint_with_content(
            "src/b.rs",
            &["execute"],
            "fn execute() {\n    validate();\n    build();\n    upload();\n    notify();\n}",
        );

        // "run" is skipped, so only one method in the pool — no pair to compare
        let findings =
            detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new());
        // Only fp2's "execute" has a valid call sequence; fp1's "run" is filtered
        // So there's only 1 candidate, no pair → no findings
        assert!(findings.is_empty(), "Generic names should be filtered out");
    }

    #[test]
    fn extract_calls_skips_keywords() {
        let body = "if something() {\n    let x = process();\n    for item in list() {\n        handle(item);\n    }\n}";
        let calls = extract_calls_from_body(body);
        assert!(calls.contains(&"something".to_string()));
        assert!(calls.contains(&"process".to_string()));
        assert!(calls.contains(&"list".to_string()));
        assert!(calls.contains(&"handle".to_string()));
        assert!(!calls.contains(&"if".to_string()));
        assert!(!calls.contains(&"for".to_string()));
        assert!(!calls.contains(&"let".to_string()));
    }

    #[test]
    fn jaccard_identical_sets() {
        let a = vec!["foo".to_string(), "bar".to_string()];
        assert!((jaccard_similarity(&a, &a) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_sets() {
        let a = vec!["foo".to_string()];
        let b = vec!["bar".to_string()];
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn lcs_identical_sequences() {
        let a = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(lcs_length(&a, &a), 3);
        assert!((lcs_ratio(&a, &a) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn lcs_partial_overlap() {
        let a = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let b = vec!["a".to_string(), "x".to_string(), "c".to_string()];
        assert_eq!(lcs_length(&a, &b), 2); // a, c
    }

    #[test]
    fn convention_methods_skip_parallel_detection() {
        // Two methods with identical call patterns — would normally flag
        let fp1 = make_fingerprint_with_content(
            "src/deploy.rs",
            &["registerAbilities"],
            "fn registerAbilities() {\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    notify_complete();\n}",
        );
        let fp2 = make_fingerprint_with_content(
            "src/upgrade.rs",
            &["registerAbility"],
            "fn registerAbility() {\n    validate_component();\n    build_artifact();\n    upload_to_host();\n    run_post_hooks();\n    send_notification();\n}",
        );

        // Without convention methods: flagged
        let findings =
            detect_parallel_implementations(&[&fp1, &fp2], &std::collections::HashSet::new());
        assert_eq!(findings.len(), 2, "Should flag without convention context");

        // With EITHER method as convention-expected: NOT flagged
        let conv_methods: std::collections::HashSet<String> = ["registerAbilities"] // only one of the two
            .iter()
            .map(|s| s.to_string())
            .collect();
        let findings = detect_parallel_implementations(&[&fp1, &fp2], &conv_methods);
        assert!(
            findings.is_empty(),
            "Pairs involving convention methods should not be flagged, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_detect_duplicate_groups_default_path() {

        let result = detect_duplicate_groups();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_detect_duplicate_groups_has_expected_effects() {
        // Expected effects: mutation

        let _ = detect_duplicate_groups();
    }

    #[test]
    fn test_detect_duplicates_default_path() {

        let result = detect_duplicates();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_detect_duplicates_has_expected_effects() {
        // Expected effects: mutation

        let _ = detect_duplicates();
    }

    #[test]
    fn test_detect_near_duplicates_default_path() {

        let result = detect_near_duplicates();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_detect_near_duplicates_has_expected_effects() {
        // Expected effects: mutation

        let _ = detect_near_duplicates();
    }

    #[test]
    fn test_detect_intra_method_duplicates_let_some_body_start_body_end_find_method_body_file_lines_met() {

        let result = detect_intra_method_duplicates();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some((body_start, body_end)) = find_method_body(&file_lines, method_name) else {{");
    }

    #[test]
    fn test_detect_intra_method_duplicates_trimmed_is_empty_is_comment_only_trimmed() {

        let result = detect_intra_method_duplicates();
        assert!(!result.is_empty(), "expected non-empty collection for: trimmed.is_empty() || is_comment_only(trimmed)");
    }

    #[test]
    fn test_detect_intra_method_duplicates_else() {

        let result = detect_intra_method_duplicates();
        assert!(!result.is_empty(), "expected non-empty collection for: else");
    }

    #[test]
    fn test_detect_intra_method_duplicates_has_expected_effects() {
        // Expected effects: mutation

        let _ = detect_intra_method_duplicates();
    }

    #[test]
    fn test_detect_parallel_implementations_default_path() {

        let result = detect_parallel_implementations();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_detect_parallel_implementations_has_expected_effects() {
        // Expected effects: mutation

        let _ = detect_parallel_implementations();
    }

}
