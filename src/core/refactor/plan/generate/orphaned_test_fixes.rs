use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::code_audit::fingerprint;
use crate::core::refactor::auto::{Fix, FixSafetyTier, InsertionKind, SkippedFile};
use crate::engine::text::levenshtein;
use std::path::Path;

use super::insertion;

/// Extract the correct test path from a misplaced test finding description.
///
/// Expected format: "Test file is misplaced — source moved to '...' (expected test at 'correct/path')"
fn extract_correct_test_path(description: &str) -> Option<String> {
    let needle = "expected test at '";
    let start = description.find(needle)? + needle.len();
    let rest = &description[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Extract the test method name from an orphaned-test finding description.
///
/// Expected format: "Test method 'test_foo' references 'foo' which no longer exists in the source"
fn extract_test_method_name(description: &str) -> Option<String> {
    let needle = "Test method '";
    let start = description.find(needle)? + needle.len();
    let rest = &description[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Extract the expected source method name from an orphaned-test finding description.
///
/// Expected format: "Test method 'test_foo' references 'foo' which no longer exists in the source"
fn extract_expected_source_name(description: &str) -> Option<String> {
    let needle = "references '";
    let start = description.find(needle)? + needle.len();
    let rest = &description[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Compute normalized similarity between two strings (0.0 = no match, 1.0 = identical).
/// Uses levenshtein distance normalized by the longer string's length.
fn normalized_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(a, b);
    1.0 - (dist as f64 / max_len as f64)
}

/// Try to find a renamed source method that matches the orphaned test's expected name.
///
/// Resolves the source file from the test file path using Rust naming conventions,
/// fingerprints it to get current methods, and fuzzy-matches.
///
/// Returns `Some(new_method_name)` if a high-confidence rename candidate is found.
fn find_rename_candidate(root: &Path, test_file: &str, expected_source: &str) -> Option<String> {
    // Derive source file path from test file path using Rust conventions:
    // tests/core/foo_test.rs → src/core/foo.rs
    let source_path = test_file_to_source_path(test_file)?;
    let abs_source = root.join(&source_path);

    if !abs_source.exists() {
        return None;
    }

    // Fingerprint the source file to get current methods
    let fp = fingerprint::fingerprint_file(&abs_source, root)?;

    // Find the best fuzzy match among current source methods
    let mut best_match: Option<(&str, f64)> = None;
    for method in &fp.methods {
        let sim = normalized_similarity(expected_source, method);
        if sim > best_match.map_or(0.0, |(_, s)| s) {
            best_match = Some((method, sim));
        }
    }

    // Require ≥0.5 similarity for a rename candidate.
    // This catches common renames like:
    //   build_report → generate_report (0.6+)
    //   parse → parse_url (0.5+)
    //   validate_write → validate_only (0.5+)
    // But rejects unrelated names like:
    //   parse → deploy (0.2)
    if let Some((name, sim)) = best_match {
        if (0.5..1.0).contains(&sim) {
            return Some(name.to_string());
        }
    }

    None
}

/// Convert a test file path to its expected source file path using Rust conventions.
///
/// `tests/core/foo_test.rs` → `src/core/foo.rs`
/// `tests/core/code_audit/bar_test.rs` → `src/core/code_audit/bar.rs`
fn test_file_to_source_path(test_path: &str) -> Option<String> {
    let path = test_path.strip_prefix("tests/")?;
    let path = path.strip_suffix("_test.rs")?;
    Some(format!("src/{}.rs", path))
}

/// Returns true if this is a method-level orphaned test (not a file-level orphan).
///
/// Method-level: "Test method 'X' references 'Y' which no longer exists in the source"
/// File-level:   "Test file has no corresponding source file (expected 'path')"
fn is_method_level_orphan(description: &str) -> bool {
    description.contains("no longer exists")
}

/// Find a function's line range by name within source content.
///
/// `parse_items_for_dedup` excludes items inside `#[cfg(test)]` modules,
/// so we need our own search that works for inline test functions.
///
/// Returns `(start_line, end_line)` as 1-indexed inclusive line numbers,
/// where `start_line` includes any `#[test]` or `#[ignore]` attributes
/// and doc comments above the function.
fn find_test_function_range(content: &str, fn_name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();

    // Try the full name first, then without the test_ prefix.
    // Rust inline tests often omit the test_ prefix (relying on #[test] attribute),
    // but the audit detector reports them with the prefix added back.
    let candidates: Vec<&str> = if let Some(stripped) = fn_name.strip_prefix("test_") {
        vec![fn_name, stripped]
    } else {
        vec![fn_name]
    };

    let decl_idx = candidates.iter().find_map(|name| {
        lines.iter().position(|line| {
            let trimmed = line.trim();
            trimmed.contains(&format!("fn {}(", name))
                || trimmed.contains(&format!("fn {} (", name))
        })
    })?;

    // Walk backwards to include #[test], #[ignore], doc comments, and attributes
    let mut start_idx = decl_idx;
    while start_idx > 0 {
        let prev = lines[start_idx - 1].trim();
        if prev.starts_with("#[")
            || prev.starts_with("///")
            || prev.starts_with("//!")
            || prev.is_empty()
        {
            // Don't include blank lines that aren't between attributes/comments
            if prev.is_empty() {
                if start_idx >= 2 {
                    let above = lines[start_idx - 2].trim();
                    if above.starts_with("#[") || above.starts_with("///") {
                        start_idx -= 1;
                        continue;
                    }
                }
                break;
            }
            start_idx -= 1;
        } else {
            break;
        }
    }

    // Walk forward to find the matching closing brace using string-aware brace
    // counting. We must skip braces inside string literals (e.g., regex patterns
    // in `r"...\{..."`) to avoid miscounting and producing broken removals.
    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut end_idx = decl_idx;

    for i in decl_idx..lines.len() {
        let mut in_string: Option<char> = None;
        let mut prev_char = '\0';

        for ch in lines[i].chars() {
            if let Some(quote) = in_string {
                // Inside a string — look for the closing quote (unescaped).
                if ch == quote && prev_char != '\\' {
                    in_string = None;
                }
            } else {
                match ch {
                    '"' | '\'' => {
                        in_string = Some(ch);
                    }
                    '{' => {
                        depth += 1;
                        found_open = true;
                    }
                    '}' => {
                        depth -= 1;
                        if found_open && depth == 0 {
                            end_idx = i;
                            return Some((start_idx + 1, end_idx + 1)); // 1-indexed
                        }
                    }
                    _ => {}
                }
            }
            prev_char = ch;
        }
    }

    // Fallback: if we found the open but not the close, something is off
    if found_open {
        None
    } else {
        // Function with no body (shouldn't happen for tests, but be safe)
        None
    }
}

pub(crate) fn generate_orphaned_test_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    for finding in &result.findings {
        if finding.kind != AuditFinding::OrphanedTest {
            continue;
        }

        // Handle file-level misplaced tests — generate FileMove fixes.
        // Description format: "Test file is misplaced — source moved to '...' (expected test at '...')"
        if finding.description.contains("is misplaced") {
            if let Some(correct_path) = extract_correct_test_path(&finding.description) {
                let mut ins = insertion(
                    InsertionKind::FileMove {
                        from: finding.file.clone(),
                        to: correct_path.clone(),
                    },
                    AuditFinding::OrphanedTest,
                    String::new(),
                    format!(
                        "Move misplaced test '{}' → '{}'",
                        finding.file, correct_path
                    ),
                );
                ins.safety_tier = FixSafetyTier::Safe;

                fixes.push(Fix {
                    file: finding.file.clone(),
                    required_methods: vec![],
                    required_registrations: vec![],
                    insertions: vec![ins],
                    applied: false,
                });
            }
            continue;
        }

        // Only handle method-level orphans — skip file-level orphans.
        if !is_method_level_orphan(&finding.description) {
            continue;
        }

        let Some(test_method) = extract_test_method_name(&finding.description) else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Cannot extract test method name from description: {}",
                    finding.description
                ),
            });
            continue;
        };

        let abs_path = root.join(&finding.file);

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(content) => content,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: format!(
                        "Cannot read test file to remove orphaned test `{}`",
                        test_method
                    ),
                });
                continue;
            }
        };

        let Some((start_line, end_line)) = find_test_function_range(&content, &test_method) else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Test function `{}` not found in {}",
                    test_method, finding.file
                ),
            });
            continue;
        };

        // Try to find a renamed source method before falling back to deletion.
        // If the source method was renamed (e.g., build_report → generate_report),
        // rename the test to match instead of deleting it.
        let expected_source = extract_expected_source_name(&finding.description);
        let rename_candidate = expected_source
            .as_deref()
            .and_then(|src| find_rename_candidate(root, &finding.file, src));

        if let Some(new_source_name) = rename_candidate {
            // Generate a test rename fix instead of deletion.
            // The test method prefix convention: test_<source_name>...
            // We replace the old source name with the new one in the test function name.
            let old_test_name = &test_method;
            let _new_test_name = if let Some(src) = expected_source.as_deref() {
                old_test_name.replacen(src, &new_source_name, 1)
            } else {
                continue;
            };

            // Find the declaration line and build a text replacement
            let lines: Vec<&str> = content.lines().collect();
            let decl_line_idx = (start_line - 1..end_line).find(|&i| {
                let trimmed = lines.get(i).map_or("", |l| l.trim());
                trimmed.contains(&format!("fn {}(", old_test_name))
                    || trimmed.contains(&format!("fn {} (", old_test_name))
                    || {
                        // Also check for the stripped prefix variant
                        if let Some(stripped) = old_test_name.strip_prefix("test_") {
                            trimmed.contains(&format!("fn {}(", stripped))
                                || trimmed.contains(&format!("fn {} (", stripped))
                        } else {
                            false
                        }
                    }
            });

            if let Some(decl_idx) = decl_line_idx {
                let old_line = lines[decl_idx];
                // Determine the actual function name in the file (may not have test_ prefix)
                let actual_old_name = if old_line.contains(&format!("fn {}(", old_test_name))
                    || old_line.contains(&format!("fn {} (", old_test_name))
                {
                    old_test_name.clone()
                } else if let Some(stripped) = old_test_name.strip_prefix("test_") {
                    stripped.to_string()
                } else {
                    old_test_name.clone()
                };

                let actual_new_name = if let Some(src) = expected_source.as_deref() {
                    actual_old_name.replacen(src, &new_source_name, 1)
                } else {
                    continue;
                };

                let new_line = old_line.replacen(&actual_old_name, &actual_new_name, 1);

                let mut ins = insertion(
                    InsertionKind::LineReplacement {
                        line: decl_idx + 1, // 1-indexed
                        old_text: old_line.to_string(),
                        new_text: new_line,
                    },
                    AuditFinding::OrphanedTest,
                    String::new(),
                    format!(
                        "Rename orphaned test `{}` → `{}` (source method renamed: `{}` → `{}`)",
                        actual_old_name,
                        actual_new_name,
                        expected_source.as_deref().unwrap_or("?"),
                        new_source_name
                    ),
                );
                ins.safety_tier = FixSafetyTier::Safe;

                fixes.push(Fix {
                    file: finding.file.clone(),
                    required_methods: vec![],
                    required_registrations: vec![],
                    insertions: vec![ins],
                    applied: false,
                });
                continue;
            }
        }

        // Fallback: delete the orphaned test if no rename candidate found
        let mut ins = insertion(
            InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            },
            AuditFinding::OrphanedTest,
            String::new(),
            format!(
                "Remove orphaned test `{}` — referenced source method no longer exists",
                test_method
            ),
        );
        ins.safety_tier = FixSafetyTier::Safe;

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![ins],
            applied: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_test_method_name_valid() {
        let desc =
            "Test method 'test_foo_bar' references 'foo_bar' which no longer exists in the source";
        assert_eq!(
            extract_test_method_name(desc),
            Some("test_foo_bar".to_string())
        );
    }

    #[test]
    fn test_extract_test_method_name_no_match() {
        let desc = "Test file has no corresponding source file (expected 'src/foo.rs')";
        assert_eq!(extract_test_method_name(desc), None);
    }

    #[test]
    fn test_is_method_level_orphan_true() {
        let desc = "Test method 'test_foo' references 'foo' which no longer exists in the source";
        assert!(is_method_level_orphan(desc));
    }

    #[test]
    fn test_is_method_level_orphan_false_for_file_level() {
        let desc = "Test file has no corresponding source file (expected 'src/foo.rs')";
        assert!(!is_method_level_orphan(desc));
    }

    #[test]
    fn test_find_test_function_range_simple() {
        let content = r#"
fn some_function() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        assert_eq!(1, 1);
    }

    #[test]
    fn test_other() {
        assert_eq!(2, 2);
    }
}
"#;
        let range = find_test_function_range(content, "test_something");
        assert!(range.is_some());
        let (start, end) = range.unwrap();
        // #[test] is on line 8, fn test_something on line 9, closing } on line 11
        assert_eq!(start, 8);
        assert_eq!(end, 11);
    }

    #[test]
    fn test_find_test_function_range_with_doc_comment() {
        let content = r#"
#[cfg(test)]
mod tests {
    /// This is a doc comment
    #[test]
    fn test_documented() {
        assert!(true);
    }
}
"#;
        let range = find_test_function_range(content, "test_documented");
        assert!(range.is_some());
        let (start, _end) = range.unwrap();
        // Doc comment starts at line 4
        assert_eq!(start, 4);
    }

    #[test]
    fn test_find_test_function_range_not_found() {
        let content = "fn main() {}\n";
        let range = find_test_function_range(content, "test_nonexistent");
        assert!(range.is_none());
    }

    #[test]
    fn test_find_test_function_range_prefix_stripped() {
        // Rust inline tests often omit the test_ prefix. The detector reports
        // "test_foo" but the actual function is "fn foo()".
        let content = r#"
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_metadata_roundtrips() {
        assert!(true);
    }
}
"#;
        // Searching for "test_audit_metadata_roundtrips" should find "audit_metadata_roundtrips"
        let range = find_test_function_range(content, "test_audit_metadata_roundtrips");
        assert!(range.is_some());
        let (start, end) = range.unwrap();
        // #[test] is on line 6, fn on line 7, closing } on line 9
        assert_eq!(start, 6);
        assert_eq!(end, 9);
    }

    #[test]
    fn test_find_test_function_range_multiline_body() {
        let content = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_complex() {
        let x = {
            let y = 1;
            y + 1
        };
        assert_eq!(x, 2);
    }
}
"#;
        let range = find_test_function_range(content, "test_complex");
        assert!(range.is_some());
        let (_start, end) = range.unwrap();
        // The closing } of test_complex is on line 11
        assert_eq!(end, 11);
    }

    #[test]
    fn test_extract_expected_source_name() {
        let desc = "Test method 'test_build_report' references 'build_report' which no longer exists in the source";
        assert_eq!(
            extract_expected_source_name(desc),
            Some("build_report".to_string())
        );
    }

    #[test]
    fn test_extract_expected_source_name_no_match() {
        let desc = "Test file has no corresponding source file";
        assert_eq!(extract_expected_source_name(desc), None);
    }

    #[test]
    fn test_normalized_similarity_identical() {
        assert_eq!(normalized_similarity("foo", "foo"), 1.0);
    }

    #[test]
    fn test_normalized_similarity_similar() {
        // Typical renames: validate_write → validate_only, save_baseline → save_baseline_scoped
        let sim = normalized_similarity("validate_write", "validate_only");
        assert!(sim > 0.5, "Expected >0.5, got {}", sim);
        let sim2 = normalized_similarity("save_baseline", "save_baseline_scoped");
        assert!(sim2 > 0.5, "Expected >0.5, got {}", sim2);
    }

    #[test]
    fn test_normalized_similarity_unrelated() {
        let sim = normalized_similarity("parse", "deploy");
        assert!(sim < 0.5, "Expected <0.5, got {}", sim);
    }

    #[test]
    fn test_find_test_function_range_with_braces_in_strings() {
        // Regression test: braces inside string literals (e.g., regex patterns)
        // should not affect brace depth counting. Previously, the naive counter
        // would miscount and produce wrong function boundaries.
        let content = r#"
#[cfg(test)]
mod tests {
    use super::*;

    fn helper_with_regex_strings() -> Grammar {
        Grammar {
            regex: r"use\s+([\w:]+(?:::\{[^}]+\})?);".to_string(),
            other: "{nested}".to_string(),
        }
    }

    #[test]
    fn test_actual() {
        assert!(true);
    }
}
"#;
        // The helper function spans lines 6-10
        let range = find_test_function_range(content, "test_helper_with_regex_strings");
        assert!(range.is_some(), "Should find the helper function");
        let (start, end) = range.unwrap();
        // The function body has braces in strings — make sure we find the right closing brace
        assert_eq!(start, 6);
        assert_eq!(end, 11);

        // The test function at line 14 should also be findable
        let range2 = find_test_function_range(content, "test_actual");
        assert!(range2.is_some(), "Should find test_actual");
        let (start2, end2) = range2.unwrap();
        assert_eq!(start2, 13); // #[test] attribute
        assert_eq!(end2, 16);
    }

    #[test]
    fn test_find_test_function_range_unbalanced_braces_in_string() {
        // Regression: raw strings can have unbalanced braces like r"\{[^}]+\}"
        // which has 1 open and 2 close braces as chars. A naive counter would
        // exit the function too early (depth drops below zero).
        let content = r#"
#[cfg(test)]
mod tests {
    fn build_grammar() -> Grammar {
        Grammar {
            regex: r"(?:::\{[^}]+\})?".to_string(),
        }
    }

    #[test]
    fn test_something() {
        assert!(true);
    }
}
"#;
        let range = find_test_function_range(content, "test_build_grammar");
        assert!(
            range.is_some(),
            "Should find build_grammar via test_ prefix strip"
        );
        let (start, end) = range.unwrap();
        assert_eq!(start, 4);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_test_file_to_source_path() {
        assert_eq!(
            test_file_to_source_path("tests/core/foo_test.rs"),
            Some("src/core/foo.rs".to_string())
        );
        assert_eq!(
            test_file_to_source_path("tests/core/code_audit/bar_test.rs"),
            Some("src/core/code_audit/bar.rs".to_string())
        );
        assert_eq!(test_file_to_source_path("src/foo.rs"), None);
    }
}
