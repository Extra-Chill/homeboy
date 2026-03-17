//! Parameter removal autofix — remove truly unused parameters from function signatures.
//!
//! Phase 2 of #824. Only handles `UnusedParameter` findings where the description
//! indicates "truly dead" (no caller passes a value for that position). These are
//! safe to remove from the signature without updating any call sites.
//!
//! `IgnoredParameter` findings (callers DO pass values) are never auto-fixed —
//! they require human judgment about whether the function should use the parameter
//! or callers should stop passing it.

use std::path::Path;

use regex::Regex;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::refactor::auto::{Fix, Insertion, InsertionKind, SkippedFile};

/// Generate parameter removal fixes for truly unused parameters.
///
/// Only processes `UnusedParameter` findings that contain "truly dead" in the
/// description (indicating no callers reach that parameter position). These
/// are safe to remove from the signature without call site updates.
pub(crate) fn generate_parameter_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    let param_re = Regex::new(
        r"(?:Unused parameter|Parameter) '(\w+)' in (?:function )?'(\w+)'.*truly dead.*position (\d+)",
    )
    .expect("parameter regex should compile");

    for finding in &result.findings {
        if finding.kind != AuditFinding::UnusedParameter {
            continue;
        }

        // Only autofix "truly dead" params — callers don't reach this position
        if !finding.description.contains("truly dead") {
            continue;
        }

        let caps = match param_re.captures(&finding.description) {
            Some(c) => c,
            None => continue,
        };

        let param_name = &caps[1];
        let fn_name = &caps[2];
        let position: usize = caps[3].parse().unwrap_or(0);

        let file_path = root.join(&finding.file);
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: format!("Could not read file: {}", finding.file),
                });
                continue;
            }
        };

        if let Some(insertion) =
            build_param_removal(&content, &finding.file, fn_name, param_name, position)
        {
            fixes.push(Fix {
                file: finding.file.clone(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![insertion],
                applied: false,
            });
        } else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Could not generate parameter removal for '{}' in '{}'",
                    param_name, fn_name
                ),
            });
        }
    }
}

/// Build a LineReplacement insertion that removes a parameter from a function signature.
///
/// Finds the function declaration line, parses the parameter list, removes the
/// parameter at the given position, and generates a replacement.
fn build_param_removal(
    content: &str,
    _file: &str,
    fn_name: &str,
    param_name: &str,
    position: usize,
) -> Option<Insertion> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the line containing the function declaration
    // PHP: function foo($a, $b, $c)
    // Rust: fn foo(a: Type, b: Type, c: Type)
    let fn_pattern = Regex::new(&format!(r"function\s+{}\s*\(", regex::escape(fn_name))).ok()?;
    let fn_pattern_rust = Regex::new(&format!(r"fn\s+{}\s*\(", regex::escape(fn_name))).ok()?;

    let (line_num, line_text) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| fn_pattern.is_match(line) || fn_pattern_rust.is_match(line))?;

    // Extract the parameter list from the line (handle single-line signatures only)
    let paren_start = line_text.find('(')?;
    // Find matching close paren by tracking depth (handles nested parens in return types)
    let paren_end = {
        let mut depth = 0;
        let mut end = None;
        for (i, ch) in line_text[paren_start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(paren_start + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        end?
    };
    if paren_start >= paren_end {
        return None;
    }

    let params_str = &line_text[paren_start + 1..paren_end];
    let params: Vec<&str> = params_str.split(',').map(|p| p.trim()).collect();

    if position >= params.len() {
        return None;
    }

    // Verify the parameter at this position contains the expected name
    if !params[position].contains(param_name) {
        return None;
    }

    // Build the new parameter list without the removed parameter
    let new_params: Vec<&str> = params
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != position)
        .map(|(_, p)| *p)
        .collect();

    let new_params_str = new_params.join(", ");
    let old_sig = &line_text[paren_start..=paren_end];
    let new_sig = format!("({})", new_params_str);

    Some(Insertion {
        kind: InsertionKind::LineReplacement {
            line: line_num + 1, // 1-indexed
            old_text: old_sig.to_string(),
            new_text: new_sig,
        },
        finding: crate::code_audit::AuditFinding::UnusedParameter,
        safety_tier: crate::refactor::auto::FixSafetyTier::Safe,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        code: String::new(),
        description: format!(
            "Remove unused parameter '{}' from '{}' (position {}, no callers pass it)",
            param_name, fn_name, position
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_param_removal_php_middle_param() {
        let content = "<?php\nclass Foo {\n    public function bar($a, $b, $c) {\n        return $a + $c;\n    }\n}\n";
        let insertion = build_param_removal(content, "test.php", "bar", "b", 1);
        assert!(insertion.is_some(), "Should generate a removal");
        let ins = insertion.unwrap();
        match &ins.kind {
            InsertionKind::LineReplacement {
                line,
                old_text,
                new_text,
            } => {
                assert_eq!(*line, 3);
                assert_eq!(old_text, "($a, $b, $c)");
                assert_eq!(new_text, "($a, $c)");
            }
            _ => panic!("Expected LineReplacement"),
        }
    }

    #[test]
    fn build_param_removal_php_last_param() {
        let content = "<?php\nfunction process($input, $opts) {\n    return $input;\n}\n";
        let insertion = build_param_removal(content, "test.php", "process", "opts", 1);
        assert!(insertion.is_some());
        let ins = insertion.unwrap();
        match &ins.kind {
            InsertionKind::LineReplacement { new_text, .. } => {
                assert_eq!(new_text, "($input)");
            }
            _ => panic!("Expected LineReplacement"),
        }
    }

    #[test]
    fn build_param_removal_php_first_param() {
        let content = "<?php\nfunction foo($unused, $used) {\n    return $used;\n}\n";
        let insertion = build_param_removal(content, "test.php", "foo", "unused", 0);
        assert!(insertion.is_some());
        let ins = insertion.unwrap();
        match &ins.kind {
            InsertionKind::LineReplacement { new_text, .. } => {
                assert_eq!(new_text, "($used)");
            }
            _ => panic!("Expected LineReplacement"),
        }
    }

    #[test]
    fn build_param_removal_rust_with_types() {
        let content =
            "fn process(input: &str, opts: Options, ctx: Context) -> Result<()> {\n    Ok(())\n}\n";
        let insertion = build_param_removal(content, "test.rs", "process", "opts", 1);
        assert!(insertion.is_some());
        let ins = insertion.unwrap();
        match &ins.kind {
            InsertionKind::LineReplacement { new_text, .. } => {
                assert_eq!(new_text, "(input: &str, ctx: Context)");
            }
            _ => panic!("Expected LineReplacement"),
        }
    }

    #[test]
    fn build_param_removal_wrong_position_returns_none() {
        let content = "<?php\nfunction foo($a, $b) {\n}\n";
        // Position 0 should be $a, not $b
        let insertion = build_param_removal(content, "test.php", "foo", "b", 0);
        assert!(
            insertion.is_none(),
            "Should return None when param name doesn't match position"
        );
    }

    #[test]
    fn build_param_removal_only_param() {
        let content = "<?php\nfunction lonely($unused) {\n}\n";
        let insertion = build_param_removal(content, "test.php", "lonely", "unused", 0);
        assert!(insertion.is_some());
        let ins = insertion.unwrap();
        match &ins.kind {
            InsertionKind::LineReplacement { new_text, .. } => {
                assert_eq!(new_text, "()");
            }
            _ => panic!("Expected LineReplacement"),
        }
    }
}
