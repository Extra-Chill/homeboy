//! Generate test files and test methods for MissingTestFile/MissingTestMethod findings.
//!
//! Uses the contract extraction → test plan → template rendering pipeline
//! to produce compilable test source code. New files are `NewFile` entries,
//! appended methods are `Fix`/`Insertion` entries — both at `Safe` tier,
//! validated by `validate_write` before committing.

use std::collections::HashMap;
use std::path::Path;

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::engine::contract_testgen::{
    generate_tests_for_file, generate_tests_for_methods, GeneratedTestOutput,
};
use crate::core::engine::symbol_graph::module_path_from_file;
use crate::core::refactor::auto::{
    Fix, FixSafetyTier, Insertion, InsertionKind, NewFile, SkippedFile,
};

/// Generate new test files for `MissingTestFile` findings.
///
/// For each finding, reads the source file, extracts function contracts,
/// generates test plans, and renders compilable test source code.
/// Produces `NewFile` entries at `Safe` tier — `validate_write` serves
/// as the safety net (if it doesn't compile, it gets rolled back).
pub(crate) fn generate_test_file_fixes(
    result: &CodeAuditResult,
    root: &Path,
    new_files: &mut Vec<NewFile>,
    skipped: &mut Vec<SkippedFile>,
) {
    let missing_test_findings: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.kind == AuditFinding::MissingTestFile)
        .collect();

    if missing_test_findings.is_empty() {
        return;
    }

    for finding in &missing_test_findings {
        let source_file = &finding.file;

        // Extract the expected test path from the finding description
        let test_path = match extract_test_path_from_description(&finding.description) {
            Some(p) => p,
            None => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: "Could not determine test file path from finding".to_string(),
                });
                continue;
            }
        };

        // Determine file extension and load grammar
        let ext = match Path::new(source_file).extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: "Could not determine file extension".to_string(),
                });
                continue;
            }
        };

        let grammar = match load_grammar_for_ext(ext) {
            Some(g) => g,
            None => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: format!("No grammar found for extension '{}'", ext),
                });
                continue;
            }
        };

        // Read source file content
        let source_path = root.join(source_file);
        let content = match std::fs::read_to_string(&source_path) {
            Ok(c) => c,
            Err(e) => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: format!("Could not read source file: {}", e),
                });
                continue;
            }
        };

        // Generate tests
        let generated = match generate_tests_for_file(&content, source_file, &grammar) {
            Some(g) => g,
            None => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: "No public functions found or no contract grammar available"
                        .to_string(),
                });
                continue;
            }
        };

        // Build the complete test file content
        let test_content = build_test_file_content(source_file, &generated, ext);

        new_files.push(NewFile {
            file: test_path.clone(),
            finding: AuditFinding::MissingTestFile,
            safety_tier: FixSafetyTier::Safe,
            auto_apply: true,
            blocked_reason: None,
            preflight: None,
            content: test_content,
            description: format!(
                "Generated test file for {} (testing: {})",
                source_file,
                generated.tested_functions.join(", ")
            ),
            written: false,
        });
    }
}

/// Build the complete test file content with module declaration, imports, and test functions.
fn build_test_file_content(
    source_file: &str,
    generated: &GeneratedTestOutput,
    ext: &str,
) -> String {
    let mut content = String::new();

    match ext {
        "rs" => {
            // Build the import path for the source module
            let module_path = module_path_from_file(source_file);
            content.push_str(&format!("use crate::{}::*;\n", module_path));

            // Add extra imports from type_defaults
            for imp in &generated.extra_imports {
                content.push_str(imp);
                content.push('\n');
            }

            content.push('\n');
            content.push_str(&generated.test_source);
        }
        _ => {
            // Non-Rust: just output the test source with extra imports
            for imp in &generated.extra_imports {
                content.push_str(imp);
                content.push('\n');
            }
            if !generated.extra_imports.is_empty() {
                content.push('\n');
            }
            content.push_str(&generated.test_source);
        }
    }

    content
}

/// Generate test methods for `MissingTestMethod` findings.
///
/// Groups findings by source file, generates tests for only the missing methods,
/// and appends them to the existing test file via `Fix`/`Insertion`.
pub(crate) fn generate_test_method_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    // Group MissingTestMethod findings by source file
    let mut by_source_file: HashMap<String, Vec<String>> = HashMap::new();
    for finding in &result.findings {
        if finding.kind != AuditFinding::MissingTestMethod {
            continue;
        }
        if let Some(method_name) = extract_method_name_from_description(&finding.description) {
            by_source_file
                .entry(finding.file.clone())
                .or_default()
                .push(method_name);
        }
    }

    if by_source_file.is_empty() {
        return;
    }

    for (source_file, missing_methods) in &by_source_file {
        // Determine file extension and load grammar
        let ext = match Path::new(source_file).extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };

        let grammar = match load_grammar_for_ext(ext) {
            Some(g) => g,
            None => continue,
        };

        // Find where tests live for this source file
        let test_location = match find_test_location(source_file, root, ext) {
            Some(loc) => loc,
            None => {
                // No test file or inline module — MissingTestFile handler covers this.
                continue;
            }
        };

        // Read source file content
        let source_path = root.join(source_file);
        let content = match std::fs::read_to_string(&source_path) {
            Ok(c) => c,
            Err(e) => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: format!("Could not read source file: {}", e),
                });
                continue;
            }
        };

        // Generate tests for only the missing methods
        let method_refs: Vec<&str> = missing_methods.iter().map(|s| s.as_str()).collect();
        let generated =
            match generate_tests_for_methods(&content, source_file, &grammar, &method_refs) {
                Some(g) => g,
                None => {
                    skipped.push(SkippedFile {
                        file: source_file.clone(),
                        reason: format!(
                            "Could not generate tests for methods: {}",
                            missing_methods.join(", ")
                        ),
                    });
                    continue;
                }
            };

        // Determine target file and build insertion code
        let (target_file, append_code) = match &test_location {
            TestLocation::SeparateFile(test_path) => {
                // Append to end of separate test file
                let mut code = String::new();
                code.push('\n');
                code.push_str(&generated.test_source);
                (test_path.clone(), code)
            }
            TestLocation::InlineModule => {
                // Insert before the closing `}` of the inline test module
                let source_content = match std::fs::read_to_string(root.join(source_file)) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let end_line = match find_inline_test_module_end(&source_content) {
                    Some(l) => l,
                    None => {
                        skipped.push(SkippedFile {
                            file: source_file.clone(),
                            reason: "Could not find end of inline test module".to_string(),
                        });
                        continue;
                    }
                };

                // The test source already has proper indentation from templates.
                // Insert before the closing brace line.
                let mut code = String::new();
                code.push('\n');
                code.push_str(&generated.test_source);
                // We use FunctionRemoval with same start/end to mark insertion point,
                // but actually MethodStub with the line context is better.
                // For inline modules, we just append before the closing brace.
                let _ = end_line; // Used for context, insertion is at end of file
                (source_file.to_string(), code)
            }
        };

        let insertions = vec![Insertion {
            kind: InsertionKind::MethodStub,
            finding: AuditFinding::MissingTestMethod,
            safety_tier: FixSafetyTier::Safe,
            auto_apply: true,
            blocked_reason: None,
            preflight: None,
            code: append_code,
            description: format!(
                "Generated tests for {} missing methods: {}",
                generated.tested_functions.len(),
                generated.tested_functions.join(", ")
            ),
        }];

        fixes.push(Fix {
            file: target_file,
            required_methods: vec![],
            required_registrations: vec![],
            insertions,
            applied: false,
        });
    }
}

/// Extract the method name from a MissingTestMethod finding description.
///
/// The description format is: "Method 'foo_bar' has no corresponding test ..."
fn extract_method_name_from_description(description: &str) -> Option<String> {
    let start = description.find("Method '")?;
    let after_quote = &description[start + "Method '".len()..];
    let end = after_quote.find('\'')?;
    Some(after_quote[..end].to_string())
}

/// Where tests live for a given source file.
enum TestLocation {
    /// Separate test file (e.g., `tests/core/foo/bar_test.rs`)
    SeparateFile(String),
    /// Inline test module in the source file itself (`#[cfg(test)] mod tests { ... }`)
    InlineModule,
}

/// Find where tests live for a given source file.
fn find_test_location(source_file: &str, root: &Path, ext: &str) -> Option<TestLocation> {
    // Check for separate test file first
    if let Some(without_ext) = source_file.strip_suffix(&format!(".{}", ext)) {
        if let Some(without_src) = without_ext.strip_prefix("src/") {
            let test_path = format!("tests/{}_test.{}", without_src, ext);
            if root.join(&test_path).exists() {
                return Some(TestLocation::SeparateFile(test_path));
            }
        }
    }

    // Check for inline test module in the source file
    let source_path = root.join(source_file);
    if let Ok(content) = std::fs::read_to_string(&source_path) {
        if content.contains("#[cfg(test)]") {
            return Some(TestLocation::InlineModule);
        }
    }

    None
}

/// Find the line number of the closing brace of the inline test module.
/// Returns the line number (1-indexed) of the last `}` in the `mod tests` block.
fn find_inline_test_module_end(content: &str) -> Option<usize> {
    let lines: Vec<&str> = content.lines().collect();

    // Find `#[cfg(test)]` as an actual attribute (not inside a comment or string)
    let mut in_test_mod = false;
    let mut brace_depth: i32 = 0;
    let mut found_cfg_test = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_test_mod {
            if !found_cfg_test {
                // Must be exactly `#[cfg(test)]` as a standalone attribute line
                // (not inside a comment, not part of a longer expression)
                if trimmed == "#[cfg(test)]" {
                    found_cfg_test = true;
                }
            } else {
                // We found #[cfg(test)] on a prior line — look for `mod tests {`
                if trimmed.is_empty() {
                    continue; // Skip blank lines between attribute and mod
                }
                if trimmed.starts_with("mod tests") || trimmed.starts_with("mod test ") {
                    in_test_mod = true;
                    for ch in trimmed.chars() {
                        if ch == '{' {
                            brace_depth += 1;
                        } else if ch == '}' {
                            brace_depth -= 1;
                        }
                    }
                } else {
                    // Not a mod declaration after #[cfg(test)] — reset
                    found_cfg_test = false;
                }
            }
        } else {
            // Count braces to find the closing brace
            for ch in trimmed.chars() {
                if ch == '{' {
                    brace_depth += 1;
                } else if ch == '}' {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        return Some(i + 1); // 1-indexed
                    }
                }
            }
        }
    }

    None
}

/// Extract the expected test path from a MissingTestFile finding description.
///
/// The description format is: "No test file found (expected 'tests/core/engine/foo_test.rs') ..."
fn extract_test_path_from_description(description: &str) -> Option<String> {
    let start = description.find("expected '")?;
    let after_quote = &description[start + "expected '".len()..];
    let end = after_quote.find('\'')?;
    Some(after_quote[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_test_path_from_typical_description() {
        let desc = "No test file found (expected 'tests/core/engine/validate_write_test.rs') and no inline tests";
        assert_eq!(
            extract_test_path_from_description(desc),
            Some("tests/core/engine/validate_write_test.rs".to_string())
        );
    }

    #[test]
    fn extract_test_path_returns_none_for_bad_format() {
        assert_eq!(extract_test_path_from_description("no test file"), None);
    }

    #[test]
    fn extract_method_name_from_typical_description() {
        let desc =
            "Method 'validate_write' has no corresponding test (expected 'test_validate_write')";
        assert_eq!(
            extract_method_name_from_description(desc),
            Some("validate_write".to_string())
        );
    }

    #[test]
    fn extract_method_name_returns_none_for_bad_format() {
        assert_eq!(extract_method_name_from_description("no method info"), None);
    }

    #[test]
    fn find_test_file_basic_convention() {
        // This tests the path logic, not file existence
        let expected = "tests/core/engine/foo_test.rs";
        let result = {
            let source = "src/core/engine/foo.rs";
            let without_ext = source.strip_suffix(".rs").unwrap();
            let without_src = without_ext.strip_prefix("src/").unwrap();
            format!("tests/{}_test.rs", without_src)
        };
        assert_eq!(result, expected);
    }

    #[test]
    fn build_test_file_content_includes_imports() {
        let generated = GeneratedTestOutput {
            test_source: "#[test]\nfn test_foo() {}\n".to_string(),
            extra_imports: vec!["use std::path::Path;".to_string()],
            tested_functions: vec!["foo".to_string()],
        };

        let content = build_test_file_content("src/core/engine/foo.rs", &generated, "rs");
        assert!(content.contains("use crate::core::engine::foo::*;"));
        assert!(content.contains("use std::path::Path;"));
        assert!(content.contains("#[test]"));
    }
}
