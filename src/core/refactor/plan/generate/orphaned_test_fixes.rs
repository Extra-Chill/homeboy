use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{Fix, FixSafetyTier, InsertionKind, SkippedFile};
use std::path::Path;

use super::insertion;

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

    // Walk forward to find the matching closing brace using simple brace counting.
    // This is sufficient for Rust test functions which have straightforward bodies.
    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut end_idx = decl_idx;

    for i in decl_idx..lines.len() {
        for ch in lines[i].chars() {
            match ch {
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
        ins.safety_tier = FixSafetyTier::SafeWithChecks;

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
}
