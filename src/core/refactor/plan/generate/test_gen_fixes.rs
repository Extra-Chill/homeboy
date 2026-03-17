//! Generate test files for MissingTestFile audit findings.
//!
//! Uses the contract extraction → test plan → template rendering pipeline
//! to produce compilable test source code. The generated file is a `NewFile`
//! entry with `Safe` tier — validated by `validate_write` before committing.

use std::path::Path;

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::engine::contract_testgen::{generate_tests_for_file, GeneratedTestOutput};
use crate::core::engine::symbol_graph::module_path_from_file;
use crate::core::refactor::auto::{FixSafetyTier, NewFile, SkippedFile};

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
                    reason: "No public functions found or no contract grammar available".to_string(),
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
