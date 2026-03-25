//! parallel_implementation_detection — extracted from duplication.rs.

use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use std::collections::HashMap;
use super::MethodCallSequence;


/// Extract function call names from a code block.
///
/// Matches patterns like `function_name(`, `self.method(`, `Type::method(`.
/// Returns the called name (without receiver/namespace prefix).
pub(crate) fn extract_calls_from_body(body: &str) -> Vec<String> {
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

/// Check if a name is a language keyword (not a function call).
pub(crate) fn is_keyword(name: &str) -> bool {
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
pub(crate) fn extract_call_sequences(fingerprints: &[&FileFingerprint]) -> Vec<MethodCallSequence> {
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
pub(crate) fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
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
pub(crate) fn lcs_length(a: &[String], b: &[String]) -> usize {
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
pub(crate) fn lcs_ratio(a: &[String], b: &[String]) -> f64 {
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
pub fn detect_parallel_implementations(
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
