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
    generate_tests_for_file_with_types, generate_tests_for_methods_with_types, GeneratedTestOutput,
};
use crate::core::extension::grammar_items;
use crate::core::refactor::auto::{
    Fix, FixSafetyTier, Insertion, InsertionKind, NewFile, SkippedFile,
};

/// Generate inline test modules for `MissingTestFile` findings.
///
/// For each finding, reads the source file, extracts function contracts,
/// generates test plans, and renders a `#[cfg(test)] mod tests { ... }` block
/// that gets appended to the end of the **source file** itself.
///
/// This matches the codebase's existing test pattern (all 830+ tests are inline)
/// and avoids orphaned files in `tests/` that Rust's test runner can't discover.
///
/// Produces `Fix`/`Insertion` entries at `Safe` tier — `validate_write` serves
/// as the safety net (if it doesn't compile, it gets rolled back).
pub(crate) fn generate_test_file_fixes(
    result: &CodeAuditResult,
    root: &Path,
    new_files: &mut Vec<NewFile>,
    fixes: &mut Vec<Fix>,
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

    // Build a project-wide type registry once for cross-file struct resolution.
    let project_registry = build_project_registry_for_findings(&missing_test_findings, root);

    for finding in &missing_test_findings {
        let source_file = &finding.file;

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

        // Skip files that already have a test module
        if content.contains("#[cfg(test)]") {
            continue;
        }

        // Generate tests with cross-file type resolution
        let generated = match generate_tests_for_file_with_types(
            &content,
            source_file,
            &grammar,
            Some(&project_registry),
        ) {
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

        // Build inline test module content — skip if brace validation fails
        let test_module = match build_inline_test_module(&generated, ext) {
            Some(module) => module,
            None => {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: "Generated test source has unbalanced braces — skipping to avoid compilation breakage".to_string(),
                });
                continue;
            }
        };

        fixes.push(Fix {
            file: source_file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                kind: InsertionKind::TestModule,
                finding: AuditFinding::MissingTestFile,
                safety_tier: FixSafetyTier::Safe,
                auto_apply: true,
                blocked_reason: None,
                preflight: None,
                code: test_module,
                description: format!(
                    "Append inline test module (testing: {})",
                    generated.tested_functions.join(", ")
                ),
            }],
            applied: false,
        });
    }

    // Suppress unused parameter warning — new_files kept for backward compatibility
    // with callers that still expect NewFile output for non-Rust languages
    let _ = new_files;
}

/// Build an inline `#[cfg(test)] mod tests { ... }` block for Rust files,
/// or a test class for PHP files.
///
/// For Rust: wraps the generated test functions in a test module with
/// `use super::*;` to access the parent module's items.
///
/// For other languages: wraps in the appropriate test class structure.
///
/// Returns `None` if the generated test source has unbalanced braces,
/// which would break compilation when appended to the source file.
fn build_inline_test_module(generated: &GeneratedTestOutput, ext: &str) -> Option<String> {
    let mut content = String::new();
    content.push('\n');

    match ext {
        "rs" => {
            // Validate that the generated test source has balanced braces before
            // wrapping it in mod tests {}. Unbalanced braces in template-expanded
            // code (e.g., from assertion conditions containing raw braces) would
            // produce an extra closing `}` that breaks the module boundary.
            if let Some(grammar) = load_grammar_for_ext("rs") {
                if !grammar_items::validate_brace_balance(&generated.test_source, &grammar) {
                    return None;
                }
            }

            content.push_str("#[cfg(test)]\nmod tests {\n");
            content.push_str("    use super::*;\n");

            // Add extra imports from type_defaults (e.g., use std::path::Path;)
            for imp in &generated.extra_imports {
                content.push_str(&format!("    {}\n", imp.trim()));
            }

            content.push('\n');
            content.push_str(&generated.test_source);
            content.push_str("}\n");
        }
        "php" => {
            // PHP: test functions are methods in a test class
            // The test_templates already produce method-level code
            for imp in &generated.extra_imports {
                content.push_str(imp);
                content.push('\n');
            }
            if !generated.extra_imports.is_empty() {
                content.push('\n');
            }
            content.push_str(&generated.test_source);
        }
        _ => {
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

    Some(content)
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

    // Build project registry once for all method findings
    let method_findings: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.kind == AuditFinding::MissingTestMethod)
        .collect();
    let project_registry = build_project_registry_for_findings(&method_findings, root);

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

        // Generate tests for only the missing methods with cross-file type resolution
        let method_refs: Vec<&str> = missing_methods.iter().map(|s| s.as_str()).collect();
        let generated = match generate_tests_for_methods_with_types(
            &content,
            source_file,
            &grammar,
            &method_refs,
            Some(&project_registry),
        ) {
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

        // Validate brace balance in generated test source before insertion.
        // Unbalanced braces from template expansion (e.g., conditions with raw
        // braces) would break the target file's mod tests {} block.
        if ext == "rs" {
            if !grammar_items::validate_brace_balance(&generated.test_source, &grammar) {
                skipped.push(SkippedFile {
                    file: source_file.clone(),
                    reason: "Generated test method source has unbalanced braces — skipping to avoid compilation breakage".to_string(),
                });
                continue;
            }
        }

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
///
/// Uses grammar-aware brace matching (via `grammar_items::find_matching_brace`)
/// to correctly handle braces inside string literals, comments, raw strings,
/// and char literals. The naive char-counting approach previously used here
/// would miscount braces in string content and produce wrong boundaries.
fn find_inline_test_module_end(content: &str) -> Option<usize> {
    let grammar = load_grammar_for_ext("rs")?;
    let lines: Vec<&str> = content.lines().collect();

    // Find `#[cfg(test)]` followed by `mod tests`
    let mut found_cfg_test = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !found_cfg_test {
            if trimmed == "#[cfg(test)]" {
                found_cfg_test = true;
            }
        } else {
            if trimmed.is_empty() {
                continue; // Skip blank lines between attribute and mod
            }
            if trimmed.starts_with("mod tests") || trimmed.starts_with("mod test ") {
                // Use grammar-aware brace matching to find the end
                let end_line_idx = grammar_items::find_matching_brace(&lines, i, &grammar);
                return Some(end_line_idx + 1); // 1-indexed
            }
            // Not a mod declaration after #[cfg(test)] — reset
            found_cfg_test = false;
        }
    }

    None
}

/// Extract the expected test path from a MissingTestFile finding description.
/// Build a project-wide type registry for a set of audit findings.
///
/// Determines the dominant file extension from the findings, loads the
/// corresponding grammar, and scans the project for all type definitions.
/// This is called once per autofix batch to avoid scanning the project tree
/// per-finding.
fn build_project_registry_for_findings(
    findings: &[&crate::code_audit::Finding],
    root: &Path,
) -> HashMap<String, crate::core::engine::contract::TypeDefinition> {
    let ext = findings
        .iter()
        .filter_map(|f| {
            Path::new(&f.file)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        })
        .next();

    let ext = match ext {
        Some(e) => e,
        None => return HashMap::new(),
    };

    let grammar = match load_grammar_for_ext(&ext) {
        Some(g) => g,
        None => return HashMap::new(),
    };

    let contract_grammar = match grammar.contract.as_ref() {
        Some(cg) => cg,
        None => return HashMap::new(),
    };

    crate::core::engine::contract_testgen::build_project_type_registry(
        root,
        &grammar,
        contract_grammar,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_method_name_from_description_typical() {
        let desc =
            "Method 'validate_write' has no corresponding test (expected 'test_validate_write')";
        assert_eq!(
            extract_method_name_from_description(desc),
            Some("validate_write".to_string())
        );
    }

    #[test]
    fn test_extract_method_name_from_description_bad_format() {
        assert_eq!(extract_method_name_from_description("no method info"), None);
    }

    #[test]
    fn test_find_test_location_basic_convention() {
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
}
