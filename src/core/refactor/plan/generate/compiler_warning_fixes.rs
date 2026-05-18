//! Auto-fix compiler warnings using machine-applicable suggestions from the compiler.
//!
//! Runs extension-owned compiler warning fix scripts, then converts their generic
//! fix envelopes to Fix objects that the refactor pipeline applies.

use std::path::Path;

use super::{tagged_line_replacement, tagged_range_removal};
use crate::core::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::extension::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract, ExtensionManifest,
};
use crate::core::refactor::auto::{Fix, RefactorPrimitive, SkippedFile};

/// A machine-applicable fix suggestion from the compiler.
#[derive(Debug, Clone, serde::Deserialize)]
struct CompilerSuggestion {
    /// Warning code (e.g., "unused_imports", "dead_code").
    #[serde(default)]
    code: String,
    /// Generic edit kind: line_removal or line_replacement.
    kind: String,
    /// Relative file path.
    file: String,
    /// 1-indexed start line of the span to replace.
    line_start: usize,
    /// 1-indexed end line of the span to replace.
    line_end: usize,
    /// The text on the original line(s) to match for replacement.
    original_text: String,
    /// The replacement text (empty string = delete).
    replacement: String,
    /// Human-readable description.
    message: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CompilerFixEnvelope {
    #[serde(default)]
    fixes: Vec<CompilerSuggestion>,
}

/// Generate fixes for compiler warnings by running extension-owned fix scripts.
pub(crate) fn generate_compiler_warning_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    // Only run if there are compiler warning findings.
    let warning_count = result
        .findings
        .iter()
        .filter(|f| f.kind == AuditFinding::CompilerWarning)
        .count();

    if warning_count == 0 {
        return;
    }

    let suggestions =
        extensions_for_compiler_warning_contract(root, CompilerWarningContract::Fixes)
            .into_iter()
            .flat_map(|extension| {
                run_compiler_warning_fixes_script(&extension, root, result, skipped)
            })
            .collect::<Vec<_>>();

    for suggestion in suggestions {
        let fix = match suggestion.kind.as_str() {
            "line_removal" => {
                if is_inside_test_module(root, &suggestion) {
                    continue;
                }
                build_line_removal_fix(&suggestion)
            }
            "line_replacement" => build_line_replacement_fix(&suggestion),
            other => {
                skipped.push(SkippedFile {
                    file: suggestion.file.clone(),
                    reason: format!(
                        "Unknown compiler warning fix kind '{}' at line {}",
                        other, suggestion.line_start
                    ),
                });
                continue;
            }
        };

        fixes.push(fix);
    }
}

fn run_compiler_warning_fixes_script(
    extension: &ExtensionManifest,
    root: &Path,
    result: &CodeAuditResult,
    skipped: &mut Vec<SkippedFile>,
) -> Vec<CompilerSuggestion> {
    let input = serde_json::json!({
        "root": root,
        "findings": result.findings,
    });

    let stdout = match run_compiler_warning_contract_script(
        extension,
        CompilerWarningContract::Fixes,
        root,
        &input,
    ) {
        Ok(Some(stdout)) => stdout,
        Ok(None) => return Vec::new(),
        Err(error) => {
            skipped.push(SkippedFile {
                file: String::new(),
                reason: error,
            });
            return Vec::new();
        }
    };

    serde_json::from_str::<CompilerFixEnvelope>(&stdout)
        .map(|envelope| envelope.fixes)
        .unwrap_or_else(|e| {
            skipped.push(SkippedFile {
                file: String::new(),
                reason: format!(
                    "Invalid compiler warning fix output for extension '{}': {}",
                    extension.id, e
                ),
            });
            Vec::new()
        })
}

/// Build a Fix that removes lines (for unused imports, dead code).
fn build_line_removal_fix(suggestion: &CompilerSuggestion) -> Fix {
    let ins = tagged_range_removal(
        RefactorPrimitive::RemoveCompilerDeadCode,
        AuditFinding::CompilerWarning,
        suggestion.line_start,
        suggestion.line_end,
        format!(
            "Remove {} (compiler: {})",
            suggestion.code, suggestion.message
        ),
    );

    Fix {
        file: suggestion.file.clone(),
        required_methods: vec![],
        required_registrations: vec![],
        insertions: vec![ins],
        applied: false,
    }
}

/// Build a Fix that replaces text on a single line (for unused_mut, etc.).
fn build_line_replacement_fix(suggestion: &CompilerSuggestion) -> Fix {
    let ins = tagged_line_replacement(
        RefactorPrimitive::ApplyCompilerReplacement,
        AuditFinding::CompilerWarning,
        suggestion.line_start,
        suggestion.original_text.clone(),
        suggestion.replacement.clone(),
        format!("Fix {} (compiler: {})", suggestion.code, suggestion.message),
    );

    Fix {
        file: suggestion.file.clone(),
        required_methods: vec![],
        required_registrations: vec![],
        insertions: vec![ins],
        applied: false,
    }
}

/// Check whether a compiler suggestion points to code inside a `#[cfg(test)]` module.
///
/// Functions inside test modules (like `make_fingerprint`, `make_rule`) are test
/// helpers that get called by `#[test]` functions. The compiler may flag them as
/// `dead_code` when the test module has compilation errors elsewhere (preventing
/// call-graph analysis), or when the helpers are only used transitively.
///
/// Deleting these helpers breaks the test functions that depend on them, so we
/// skip `dead_code` removals inside test modules entirely.
fn is_inside_test_module(root: &Path, suggestion: &CompilerSuggestion) -> bool {
    let abs_path = root.join(&suggestion.file);
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let lines: Vec<&str> = content.lines().collect();
    let target_line = suggestion.line_start.saturating_sub(1); // 0-indexed

    // Walk backwards from the target line looking for `mod tests {` preceded by
    // `#[cfg(test)]`. Track brace depth to ensure the target is actually inside
    // the module (not after its closing brace).
    let mut depth: i32 = 0;
    for i in (0..=target_line.min(lines.len().saturating_sub(1))).rev() {
        let trimmed = lines[i].trim();

        // Count braces on this line (simplified — sufficient for module boundaries)
        for ch in trimmed.chars() {
            match ch {
                '}' => depth += 1,
                '{' => depth -= 1,
                _ => {}
            }
        }

        // If we see `mod tests` and depth is negative (we're inside the opening brace),
        // check whether it's preceded by `#[cfg(test)]`.
        if depth < 0 && (trimmed.starts_with("mod tests") || trimmed.starts_with("mod test ")) {
            // Look for #[cfg(test)] on the line above (skipping blank lines)
            for j in (0..i).rev() {
                let above = lines[j].trim();
                if above.is_empty() {
                    continue;
                }
                return above == "#[cfg(test)]";
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::refactor::InsertionKind;

    #[test]
    fn build_line_removal_fix_creates_function_removal() {
        let suggestion = CompilerSuggestion {
            code: "unused_imports".to_string(),
            kind: "line_removal".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 1,
            line_end: 1,
            original_text: "use std::collections::HashMap;".to_string(),
            replacement: String::new(),
            message: "unused import: `std::collections::HashMap`".to_string(),
        };

        let fix = build_line_removal_fix(&suggestion);
        assert_eq!(fix.file, "src/lib.rs");
        assert_eq!(fix.insertions.len(), 1);
        assert!(!fix.insertions[0].manual_only);
        assert!(matches!(
            fix.insertions[0].kind,
            InsertionKind::FunctionRemoval {
                start_line: 1,
                end_line: 1
            }
        ));
    }

    #[test]
    fn build_line_replacement_fix_creates_replacement() {
        let suggestion = CompilerSuggestion {
            code: "unused_mut".to_string(),
            kind: "line_replacement".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 6,
            line_end: 6,
            original_text: "mut ".to_string(),
            replacement: String::new(),
            message: "variable does not need to be mutable".to_string(),
        };

        let fix = build_line_replacement_fix(&suggestion);
        assert_eq!(fix.file, "src/lib.rs");
        assert_eq!(fix.insertions.len(), 1);
        assert!(!fix.insertions[0].manual_only);
        assert!(matches!(
            fix.insertions[0].kind,
            InsertionKind::LineReplacement { .. }
        ));
    }

    #[test]
    fn is_inside_test_module_detects_test_helpers() {
        let dir = std::env::temp_dir().join("homeboy_test_inside_test_module");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let content = r#"
pub fn public_function() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fingerprint(path: &str) -> String {
        path.to_string()
    }

    #[test]
    fn test_something() {
        let fp = make_fingerprint("test");
        assert!(!fp.is_empty());
    }
}
"#;
        std::fs::write(dir.join("src/lib.rs"), content).unwrap();

        // Line 8 is inside the test module (make_fingerprint)
        let suggestion_inside = CompilerSuggestion {
            code: "dead_code".to_string(),
            kind: "line_removal".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 8,
            line_end: 10,
            original_text: String::new(),
            replacement: String::new(),
            message: "function `make_fingerprint` is never used".to_string(),
        };
        assert!(
            is_inside_test_module(&dir, &suggestion_inside),
            "make_fingerprint at line 8 should be detected as inside test module"
        );

        // Line 2 is outside the test module (public_function)
        let suggestion_outside = CompilerSuggestion {
            code: "dead_code".to_string(),
            kind: "line_removal".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 2,
            line_end: 2,
            original_text: String::new(),
            replacement: String::new(),
            message: "function `public_function` is never used".to_string(),
        };
        assert!(
            !is_inside_test_module(&dir, &suggestion_outside),
            "public_function at line 2 should NOT be detected as inside test module"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_inside_test_module_false_for_non_test_mod() {
        let dir = std::env::temp_dir().join("homeboy_test_inside_non_test_module");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let content = r#"
mod helpers {
    fn make_something() -> String {
        String::new()
    }
}
"#;
        std::fs::write(dir.join("src/lib.rs"), content).unwrap();

        let suggestion = CompilerSuggestion {
            code: "dead_code".to_string(),
            kind: "line_removal".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 3,
            line_end: 5,
            original_text: String::new(),
            replacement: String::new(),
            message: "function `make_something` is never used".to_string(),
        };
        assert!(
            !is_inside_test_module(&dir, &suggestion),
            "function in non-test module should not be skipped"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
