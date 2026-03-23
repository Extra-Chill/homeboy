//! Auto-fix compiler warnings using machine-applicable suggestions from the compiler.
//!
//! Runs `cargo check --message-format=json` to get structured warnings with fix
//! suggestions, then converts them to Fix objects that the refactor pipeline applies.
//!
//! Supported warnings:
//! - `unused_imports`: remove the import line
//! - `unused_mut`: remove the `mut` keyword
//! - `unused_assignments`: remove the assignment
//! - `dead_code`: remove the function/method (line-range removal)

use std::path::Path;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{Fix, FixSafetyTier, Insertion, InsertionKind, SkippedFile};

/// A machine-applicable fix suggestion from the compiler.
#[derive(Debug, Clone)]
struct CompilerSuggestion {
    /// Warning code (e.g., "unused_imports", "dead_code").
    code: String,
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

/// Generate fixes for compiler warnings by running `cargo check` and parsing suggestions.
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

    // Don't run cargo check if not a Rust project.
    if !root.join("Cargo.toml").exists() {
        return;
    }

    let suggestions = match run_cargo_check_for_suggestions(root) {
        Ok(s) => s,
        Err(e) => {
            skipped.push(SkippedFile {
                file: String::new(),
                reason: format!("Failed to run cargo check for fix suggestions: {}", e),
            });
            return;
        }
    };

    for suggestion in suggestions {
        let fix = match suggestion.code.as_str() {
            // For unused_imports: the compiler suggests removing the full line(s).
            // Use FunctionRemoval for line-range deletion.
            "unused_imports" => build_line_removal_fix(&suggestion),

            // For unused_mut, unused_assignments: use LineReplacement.
            "unused_mut" | "unused_variables" | "unused_assignments" => {
                build_line_replacement_fix(&suggestion)
            }

            // dead_code: use FunctionRemoval for the span range.
            // Skip functions inside #[cfg(test)] modules — these are test helpers
            // (e.g., make_fingerprint, make_rule) that may appear unused to the
            // compiler in isolation but are called by test functions. Deleting them
            // breaks the tests that depend on them.
            "dead_code" => {
                if is_inside_test_module(root, &suggestion) {
                    continue;
                }
                build_line_removal_fix(&suggestion)
            }

            // Other warnings with suggestions: use LineReplacement if single-line.
            _ => {
                if suggestion.line_start == suggestion.line_end {
                    build_line_replacement_fix(&suggestion)
                } else {
                    // Multi-line replacement without specific handler — skip.
                    skipped.push(SkippedFile {
                        file: suggestion.file.clone(),
                        reason: format!(
                            "No fixer for multi-line {} warning at line {}",
                            suggestion.code, suggestion.line_start
                        ),
                    });
                    continue;
                }
            }
        };

        fixes.push(fix);
    }
}

/// Build a Fix that removes lines (for unused imports, dead code).
fn build_line_removal_fix(suggestion: &CompilerSuggestion) -> Fix {
    let mut ins = Insertion {
        kind: InsertionKind::FunctionRemoval {
            start_line: suggestion.line_start,
            end_line: suggestion.line_end,
        },
        finding: AuditFinding::CompilerWarning,
        // Compiler-suggested removals are safe — the compiler itself says this code is unused.
        safety_tier: FixSafetyTier::Safe,
        auto_apply: true,
        blocked_reason: None,
        preflight: None,
        code: String::new(),
        description: format!(
            "Remove {} (compiler: {})",
            suggestion.code, suggestion.message
        ),
    };

    // Override the default PlanOnly tier from FunctionRemoval.
    ins.safety_tier = FixSafetyTier::Safe;

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
    let ins = Insertion {
        kind: InsertionKind::LineReplacement {
            line: suggestion.line_start,
            old_text: suggestion.original_text.clone(),
            new_text: suggestion.replacement.clone(),
        },
        finding: AuditFinding::CompilerWarning,
        safety_tier: FixSafetyTier::Safe,
        auto_apply: true,
        blocked_reason: None,
        preflight: None,
        code: String::new(),
        description: format!("Fix {} (compiler: {})", suggestion.code, suggestion.message),
    };

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

/// Run `cargo check --message-format=json` and extract machine-applicable suggestions.
fn run_cargo_check_for_suggestions(root: &Path) -> Result<Vec<CompilerSuggestion>, String> {
    let output = std::process::Command::new("cargo")
        .args(["check", "--message-format=json"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("Failed to run cargo check: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_suggestions(&stdout, root))
}

/// Parse cargo JSON output for machine-applicable suggestions.
fn parse_suggestions(stdout: &str, root: &Path) -> Vec<CompilerSuggestion> {
    let root_str = root.to_string_lossy();
    let mut suggestions = Vec::new();

    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if msg.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
            continue;
        }

        let Some(message) = msg.get("message") else {
            continue;
        };

        if message.get("level").and_then(|v| v.as_str()) != Some("warning") {
            continue;
        }

        let code = message
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str())
            .unwrap_or("unknown")
            .to_string();

        let text = message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        // Look for machine-applicable suggestions in children.
        for child in message
            .get("children")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
        {
            for span in child
                .get("spans")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
            {
                let Some(replacement) = span.get("suggested_replacement").and_then(|r| r.as_str())
                else {
                    continue;
                };

                let file_name = span
                    .get("file_name")
                    .and_then(|f| f.as_str())
                    .unwrap_or("")
                    .to_string();

                // Make path relative to root.
                let relative = file_name
                    .strip_prefix(&*root_str)
                    .map(|s| s.trim_start_matches('/').to_string())
                    .unwrap_or(file_name);

                // Skip external files.
                if relative.is_empty() || relative.starts_with('/') || relative.contains("/.cargo/")
                {
                    continue;
                }

                let line_start =
                    span.get("line_start").and_then(|l| l.as_u64()).unwrap_or(1) as usize;
                let line_end = span
                    .get("line_end")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(line_start as u64) as usize;

                // Extract the original text from the span for LineReplacement matching.
                let original_text = span
                    .get("text")
                    .and_then(|t| t.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|t| t.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();

                // For single-line replacements, extract just the portion being replaced.
                let col_start = span
                    .get("column_start")
                    .and_then(|c| c.as_u64())
                    .unwrap_or(1) as usize;
                let col_end = span
                    .get("column_end")
                    .and_then(|c| c.as_u64())
                    .unwrap_or(col_start as u64) as usize;

                let old_text = if line_start == line_end && !original_text.is_empty() {
                    // Extract just the replaced portion from the line.
                    let start = col_start.saturating_sub(1);
                    let end = col_end.saturating_sub(1);
                    if start < original_text.len() && end <= original_text.len() {
                        original_text[start..end].to_string()
                    } else {
                        original_text.clone()
                    }
                } else {
                    original_text
                };

                suggestions.push(CompilerSuggestion {
                    code: code.clone(),
                    file: relative,
                    line_start,
                    line_end,
                    original_text: old_text,
                    replacement: replacement.to_string(),
                    message: text.clone(),
                });
            }
        }
    }

    // Deduplicate.
    suggestions
        .sort_by(|a, b| (&a.file, a.line_start, &a.code).cmp(&(&b.file, b.line_start, &b.code)));
    suggestions
        .dedup_by(|a, b| a.file == b.file && a.line_start == b.line_start && a.code == b.code);

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_suggestions_extracts_unused_import() {
        let json_line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","message":{"rendered":"warning: unused import","level":"warning","code":{"code":"unused_imports","explanation":null},"message":"unused import: `std::collections::HashMap`","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":31,"line_start":1,"line_end":2,"column_start":1,"column_end":1,"is_primary":true,"text":[{"text":"use std::collections::HashMap;","highlight_start":1,"highlight_end":31}]}],"children":[{"message":"remove the whole `use` item","code":null,"level":"help","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":31,"line_start":1,"line_end":2,"column_start":1,"column_end":1,"is_primary":true,"text":[{"text":"use std::collections::HashMap;","highlight_start":1,"highlight_end":31}],"suggested_replacement":""}],"children":[],"rendered":null}]}}"#;

        let root = Path::new("/project");
        let suggestions = parse_suggestions(json_line, root);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].code, "unused_imports");
        assert_eq!(suggestions[0].file, "src/lib.rs");
        assert_eq!(suggestions[0].line_start, 1);
        assert_eq!(suggestions[0].replacement, "");
    }

    #[test]
    fn parse_suggestions_extracts_unused_mut() {
        let json_line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","message":{"rendered":"warning: unused mut","level":"warning","code":{"code":"unused_mut","explanation":null},"message":"variable does not need to be mutable","spans":[{"file_name":"src/lib.rs","byte_start":90,"byte_end":94,"line_start":6,"line_end":6,"column_start":9,"column_end":13,"is_primary":true,"text":[{"text":"    let mut x = 5;","highlight_start":9,"highlight_end":13}]}],"children":[{"message":"remove this `mut`","code":null,"level":"help","spans":[{"file_name":"src/lib.rs","byte_start":90,"byte_end":94,"line_start":6,"line_end":6,"column_start":9,"column_end":13,"is_primary":true,"text":[{"text":"    let mut x = 5;","highlight_start":9,"highlight_end":13}],"suggested_replacement":""}],"children":[],"rendered":null}]}}"#;

        let root = Path::new("/project");
        let suggestions = parse_suggestions(json_line, root);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].code, "unused_mut");
        assert_eq!(suggestions[0].file, "src/lib.rs");
        assert_eq!(suggestions[0].line_start, 6);
        assert_eq!(suggestions[0].original_text, "mut ");
        assert_eq!(suggestions[0].replacement, "");
    }

    #[test]
    fn build_line_removal_fix_creates_function_removal() {
        let suggestion = CompilerSuggestion {
            code: "unused_imports".to_string(),
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
        assert_eq!(fix.insertions[0].safety_tier, FixSafetyTier::Safe);
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
        assert_eq!(fix.insertions[0].safety_tier, FixSafetyTier::Safe);
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
