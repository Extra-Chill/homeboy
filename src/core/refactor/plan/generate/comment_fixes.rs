//! Auto-fix legacy/fallback code blocks flagged by comment hygiene analysis.
//!
//! The audit detects comments containing markers like "temporary", "workaround",
//! "remove after", "legacy:", and "outdated". These comments signal code that
//! should not exist — compatibility shims, temporary hacks, legacy fallbacks.
//!
//! The correct fix is NOT to remove the comment — it's to remove the entire
//! legacy code block the comment annotates. This fixer:
//!
//! 1. Reads the source file to find the comment line
//! 2. Classifies the code structure the comment annotates (function, if/else
//!    branch, match arm, guard clause, code section)
//! 3. Computes the full line range of the legacy block
//! 4. Emits a `FunctionRemoval` that removes the comment + the entire block
//!
//! When the enclosing block can't be determined, emits a PlanOnly fix for
//! manual review instead of silently removing just the comment.

use std::path::Path;

use regex::Regex;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::refactor::auto::{Fix, FixSafetyTier, Insertion, InsertionKind, SkippedFile};

/// Classification of the code block a legacy comment annotates.
#[derive(Debug, PartialEq)]
enum BlockKind {
    /// Comment precedes a function/method definition.
    Function,
    /// Comment is inside or precedes an `else` branch.
    ElseBranch,
    /// Comment precedes a standalone `if` guard (no else).
    GuardClause,
    /// Comment precedes a Rust match arm.
    MatchArm,
    /// Comment precedes a contiguous code section (no structural boundary).
    CodeSection,
    /// Could not determine what the comment annotates.
    Unknown,
}

/// Generate fixes that remove legacy/fallback code blocks.
///
/// For `LegacyComment` findings: identifies the enclosing code block and
/// removes the entire thing (comment + code). Safe when the block boundaries
/// are clear. PlanOnly when they're ambiguous.
///
/// For `TodoMarker` findings: always PlanOnly since TODOs describe work to
/// be done, not code to be removed.
pub(crate) fn generate_comment_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    let legacy_re =
        Regex::new(r"Potential legacy/stale comment on line (\d+)").expect("regex should compile");
    let todo_re =
        Regex::new(r"Comment marker '[^']+' found on line (\d+)").expect("regex should compile");

    for finding in &result.findings {
        let (line_num, finding_kind) = match finding.kind {
            AuditFinding::LegacyComment => {
                let caps = match legacy_re.captures(&finding.description) {
                    Some(c) => c,
                    None => {
                        skipped.push(SkippedFile {
                            file: finding.file.clone(),
                            reason: format!(
                                "Could not parse line number from legacy comment: {}",
                                finding.description
                            ),
                        });
                        continue;
                    }
                };
                let line: usize = caps[1].parse().unwrap_or(0);
                if line == 0 {
                    continue;
                }
                (line, AuditFinding::LegacyComment)
            }
            AuditFinding::TodoMarker => {
                let caps = match todo_re.captures(&finding.description) {
                    Some(c) => c,
                    None => {
                        skipped.push(SkippedFile {
                            file: finding.file.clone(),
                            reason: format!(
                                "Could not parse line number from TODO marker: {}",
                                finding.description
                            ),
                        });
                        continue;
                    }
                };
                let line: usize = caps[1].parse().unwrap_or(0);
                if line == 0 {
                    continue;
                }
                (line, AuditFinding::TodoMarker)
            }
            _ => continue,
        };

        // TODO markers describe work to be done — always needs human review.
        if finding_kind == AuditFinding::TodoMarker {
            fixes.push(Fix {
                file: finding.file.clone(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![Insertion {
                    kind: InsertionKind::DocLineRemoval { line: line_num },
                    finding: AuditFinding::TodoMarker,
                    safety_tier: FixSafetyTier::PlanOnly,
                    auto_apply: false,
                    blocked_reason: Some(
                        "TODO markers require human judgment — resolve the TODO, then remove"
                            .to_string(),
                    ),
                    preflight: None,
                    code: String::new(),
                    description: format!(
                        "TODO marker on line {} in {} — resolve before removing",
                        line_num, finding.file
                    ),
                }],
                applied: false,
            });
            continue;
        }

        // LegacyComment — read the file and analyze the code block.
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

        let lines: Vec<&str> = content.lines().collect();
        let comment_idx = line_num - 1; // 0-indexed

        if comment_idx >= lines.len() {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!("Line {} out of range in {}", line_num, finding.file),
            });
            continue;
        }

        let (block_kind, start_line, end_line) = classify_and_bound(&lines, comment_idx);

        let (safety, blocked_reason) = match block_kind {
            BlockKind::Function | BlockKind::GuardClause | BlockKind::MatchArm => {
                (FixSafetyTier::Safe, None)
            }
            BlockKind::ElseBranch => {
                // Removing an else branch changes control flow — needs review.
                (
                    FixSafetyTier::PlanOnly,
                    Some("Removing else branch changes control flow — verify the if-branch is the canonical path".to_string()),
                )
            }
            BlockKind::CodeSection => (
                FixSafetyTier::PlanOnly,
                Some(
                    "Legacy code section boundaries detected heuristically — verify removal range"
                        .to_string(),
                ),
            ),
            BlockKind::Unknown => (
                FixSafetyTier::PlanOnly,
                Some(
                    "Could not determine legacy code block boundaries — manual review required"
                        .to_string(),
                ),
            ),
        };

        let description = format!(
            "Remove legacy {} (lines {}-{}) in {} — {:?}",
            match block_kind {
                BlockKind::Function => "function",
                BlockKind::ElseBranch => "else branch",
                BlockKind::GuardClause => "guard clause",
                BlockKind::MatchArm => "match arm",
                BlockKind::CodeSection => "code section",
                BlockKind::Unknown => "code block",
            },
            start_line,
            end_line,
            finding.file,
            &finding.description
        );

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line,
                    end_line,
                },
                finding: AuditFinding::LegacyComment,
                safety_tier: safety,
                auto_apply: false,
                blocked_reason,
                preflight: None,
                code: String::new(),
                description,
            }],
            applied: false,
        });
    }
}

/// Classify the code block annotated by a legacy comment and return its line range.
///
/// Returns `(BlockKind, start_line, end_line)` where lines are 1-indexed inclusive.
fn classify_and_bound(lines: &[&str], comment_idx: usize) -> (BlockKind, usize, usize) {
    // Find the first non-blank, non-comment line after the comment.
    let next_code_idx = find_next_code_line(lines, comment_idx + 1);

    let Some(next_idx) = next_code_idx else {
        // Comment at end of file — just the comment line.
        return (BlockKind::Unknown, comment_idx + 1, comment_idx + 1);
    };

    let next_line = lines[next_idx].trim();

    // Check if comment is INSIDE an else branch (look backwards for `} else {` or `else {`).
    if is_inside_else_branch(lines, comment_idx) {
        if let Some(end) = find_enclosing_else_end(lines, comment_idx) {
            // Include the comment and everything down to the else's closing brace.
            // But the start should be the `else {` line, not the comment.
            let else_start = find_else_start(lines, comment_idx);
            return (BlockKind::ElseBranch, else_start + 1, end + 1);
        }
    }

    // Pattern: function definition
    if is_function_start(next_line) {
        if let Some(end) = find_brace_block_end(lines, next_idx) {
            return (BlockKind::Function, comment_idx + 1, end + 1);
        }
    }

    // Pattern: standalone guard clause (if without matching else)
    if is_if_start(next_line) && !has_else_after_block(lines, next_idx) {
        if let Some(end) = find_brace_block_end(lines, next_idx) {
            return (BlockKind::GuardClause, comment_idx + 1, end + 1);
        }
    }

    // Pattern: if/else — comment precedes the if, but there's an else
    // The legacy code might be the entire if/else construct
    if is_if_start(next_line) {
        if let Some(end) = find_full_if_else_end(lines, next_idx) {
            return (BlockKind::GuardClause, comment_idx + 1, end + 1);
        }
    }

    // Pattern: match arm (line contains `=>`)
    if next_line.contains("=>") {
        if let Some(end) = find_match_arm_end(lines, next_idx) {
            return (BlockKind::MatchArm, comment_idx + 1, end + 1);
        }
    }

    // Pattern: contiguous code section — runs until next blank line or de-indent
    let section_end = find_code_section_end(lines, comment_idx);
    if section_end > comment_idx {
        return (BlockKind::CodeSection, comment_idx + 1, section_end + 1);
    }

    // Fallback: just the comment line
    (BlockKind::Unknown, comment_idx + 1, comment_idx + 1)
}

/// Find the next non-blank, non-comment line starting from `start_idx`.
fn find_next_code_line(lines: &[&str], start_idx: usize) -> Option<usize> {
    for i in start_idx..lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        return Some(i);
    }
    None
}

/// Check if a line starts a function definition.
fn is_function_start(line: &str) -> bool {
    // Rust: fn, pub fn, pub(crate) fn, async fn, etc.
    let re = Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+\w+").unwrap();
    if re.is_match(line) {
        return true;
    }
    // PHP: function, public function, private function, etc.
    let php_re = Regex::new(r"^(?:public|private|protected|static|\s)*function\s+\w+").unwrap();
    php_re.is_match(line)
}

/// Check if a line starts an `if` statement.
fn is_if_start(line: &str) -> bool {
    line.starts_with("if ") || line.starts_with("if(")
}

/// Check if the comment at `comment_idx` is inside an else branch.
/// Looks backwards for `} else {` or `else {` at the same or lower indentation.
fn is_inside_else_branch(lines: &[&str], comment_idx: usize) -> bool {
    let comment_indent = leading_indent(lines[comment_idx]);

    for i in (0..comment_idx).rev() {
        let trimmed = lines[i].trim();
        let indent = leading_indent(lines[i]);

        // Stop if we hit a line at lower indent (we've left the block).
        if indent < comment_indent && !trimmed.is_empty() {
            // Check if this line IS the else.
            if trimmed.contains("else") && trimmed.contains('{') {
                return true;
            }
            if trimmed == "} else {" || trimmed.starts_with("} else {") {
                return true;
            }
            break;
        }

        // Check for else at same indent.
        if trimmed == "} else {" || trimmed.starts_with("} else {") || trimmed == "else {" {
            return true;
        }
    }

    false
}

/// Find the line index of the `else {` that starts the branch containing `comment_idx`.
fn find_else_start(lines: &[&str], comment_idx: usize) -> usize {
    let comment_indent = leading_indent(lines[comment_idx]);

    for i in (0..comment_idx).rev() {
        let trimmed = lines[i].trim();
        let indent = leading_indent(lines[i]);

        if indent < comment_indent && !trimmed.is_empty() {
            if trimmed.contains("else") {
                return i;
            }
            break;
        }

        if trimmed == "} else {" || trimmed.starts_with("} else {") || trimmed == "else {" {
            return i;
        }
    }

    // Fallback — couldn't find else, return the comment itself.
    comment_idx
}

/// Find the closing brace of the else branch that encloses `comment_idx`.
fn find_enclosing_else_end(lines: &[&str], comment_idx: usize) -> Option<usize> {
    let else_start = find_else_start(lines, comment_idx);
    find_brace_block_end(lines, else_start)
}

/// Find the end of a brace-delimited block starting at `start_idx`.
/// The line at `start_idx` should contain the opening `{`.
/// Returns the 0-indexed line of the matching `}`.
fn find_brace_block_end(lines: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut found_opening = false;

    for i in start_idx..lines.len() {
        for ch in lines[i].chars() {
            match ch {
                '{' => {
                    depth += 1;
                    found_opening = true;
                }
                '}' => {
                    depth -= 1;
                    if found_opening && depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }

    None
}

/// Check if there's an `else` after the brace block starting at `start_idx`.
fn has_else_after_block(lines: &[&str], start_idx: usize) -> bool {
    let Some(block_end) = find_brace_block_end(lines, start_idx) else {
        return false;
    };

    // Check the closing brace line itself (Rust: `} else {`)
    let end_line = lines[block_end].trim();
    if end_line.contains("else") {
        return true;
    }

    // Check the next non-blank line after the block.
    if let Some(next) = find_next_code_line(lines, block_end + 1) {
        let trimmed = lines[next].trim();
        if trimmed.starts_with("else") || trimmed == "} else {" {
            return true;
        }
    }

    false
}

/// Find the end of an entire if/else/else-if chain.
fn find_full_if_else_end(lines: &[&str], if_start: usize) -> Option<usize> {
    let mut current = if_start;

    loop {
        let block_end = find_brace_block_end(lines, current)?;

        // Check if there's an else continuation.
        let end_trimmed = lines[block_end].trim();
        if end_trimmed.contains("else") && end_trimmed.contains('{') {
            // `} else {` or `} else if ... {` on the same line.
            current = block_end;
            continue;
        }

        // Check next line for else.
        if let Some(next) = find_next_code_line(lines, block_end + 1) {
            let next_trimmed = lines[next].trim();
            if next_trimmed.starts_with("else") {
                current = next;
                continue;
            }
        }

        return Some(block_end);
    }
}

/// Find the end of a match arm starting at `arm_idx`.
/// A match arm ends at the next `=>` line, the closing `}` of the match, or a blank line.
fn find_match_arm_end(lines: &[&str], arm_idx: usize) -> Option<usize> {
    // If the arm has a block `{ ... }`, find its end.
    let arm_line = lines[arm_idx].trim();
    if arm_line.contains('{') {
        return find_brace_block_end(lines, arm_idx);
    }

    // Single-line arm: ends at the comma or the line itself.
    // But could span multiple lines if no trailing comma.
    let arm_indent = leading_indent(lines[arm_idx]);
    let mut end = arm_idx;

    for i in (arm_idx + 1)..lines.len() {
        let trimmed = lines[i].trim();

        // Next arm or end of match.
        if trimmed.contains("=>") || trimmed == "}" {
            break;
        }

        // Blank line breaks the arm.
        if trimmed.is_empty() {
            break;
        }

        // Lines at same or lower indent are outside this arm.
        if leading_indent(lines[i]) <= arm_indent && !trimmed.is_empty() {
            break;
        }

        end = i;
    }

    Some(end)
}

/// Find the end of a contiguous code section starting from a comment.
/// A section runs until the next blank line or a line with less indentation.
fn find_code_section_end(lines: &[&str], comment_idx: usize) -> usize {
    let base_indent = leading_indent(lines[comment_idx]);
    let mut end = comment_idx;

    for i in (comment_idx + 1)..lines.len() {
        let trimmed = lines[i].trim();

        // Blank line ends the section.
        if trimmed.is_empty() {
            break;
        }

        let indent = leading_indent(lines[i]);

        // De-indent means we've left the section.
        if indent < base_indent {
            break;
        }

        end = i;
    }

    end
}

/// Count the number of leading whitespace characters.
fn leading_indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::test_helpers::empty_result;
    use crate::code_audit::{Finding, Severity};

    // ── classify_and_bound tests ──────────────────────────────────────

    #[test]
    fn classifies_function_after_comment() {
        let src = vec![
            "// some code above",
            "// legacy: old API compatibility shim",
            "fn old_handler() {",
            "    do_old_thing();",
            "}",
            "fn main() {}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 1);
        assert_eq!(kind, BlockKind::Function);
        assert_eq!(start, 2); // comment line (1-indexed)
        assert_eq!(end, 5); // closing brace of old_handler
    }

    #[test]
    fn classifies_pub_function_after_comment() {
        let src = vec![
            "// workaround for broken upstream API",
            "pub fn compat_shim(input: &str) -> String {",
            "    input.to_uppercase()",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 0);
        assert_eq!(kind, BlockKind::Function);
        assert_eq!(start, 1);
        assert_eq!(end, 4);
    }

    #[test]
    fn classifies_guard_clause() {
        let src = vec![
            "fn process() {",
            "    // temporary: handle old format",
            "    if is_legacy(data) {",
            "        convert(data);",
            "    }",
            "    do_real_work();",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 1);
        assert_eq!(kind, BlockKind::GuardClause);
        assert_eq!(start, 2); // comment
        assert_eq!(end, 5); // closing brace of if
    }

    #[test]
    fn classifies_else_branch() {
        let src = vec![
            "if new_way() {",
            "    do_new();",
            "} else {",
            "    // legacy: fallback for v1",
            "    do_old();",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 3);
        assert_eq!(kind, BlockKind::ElseBranch);
        assert_eq!(start, 3); // `} else {` line (1-indexed)
        assert_eq!(end, 6); // closing brace
    }

    #[test]
    fn classifies_match_arm() {
        let src = vec![
            "match version {",
            "    2 => new_handler(),",
            "    // workaround for legacy v1 format",
            "    1 => old_handler(),",
            "    _ => default(),",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 2);
        assert_eq!(kind, BlockKind::MatchArm);
        assert_eq!(start, 3); // comment
        assert_eq!(end, 4); // match arm line
    }

    #[test]
    fn classifies_code_section() {
        let src = vec![
            "fn process() {",
            "    let input = get();",
            "",
            "    // temporary: bridge old format",
            "    let converted = old_to_new(input);",
            "    let result = process_old(converted);",
            "",
            "    finalize();",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&src, 3);
        assert_eq!(kind, BlockKind::CodeSection);
        assert_eq!(start, 4); // comment
        assert_eq!(end, 6); // last line before blank
    }

    #[test]
    fn classifies_unknown_at_end_of_file() {
        let src = vec!["fn main() {}", "// outdated: leftover note"];
        let (kind, start, end) = classify_and_bound(&src, 1);
        // No code after the comment — Unknown.
        assert_eq!(kind, BlockKind::Unknown);
        assert_eq!(start, 2);
        assert_eq!(end, 2);
    }

    // ── Integration tests ─────────────────────────────────────────────

    #[test]
    fn generates_safe_fix_for_legacy_function() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src/old.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "// workaround for broken upstream\nfn compat_shim() {\n    hack();\n}\n",
        )
        .unwrap();

        let mut result = empty_result();
        result.source_path = dir.path().to_string_lossy().to_string();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/old.rs".to_string(),
            description: "Potential legacy/stale comment on line 1: workaround for broken upstream"
                .to_string(),
            suggestion: "Validate".to_string(),
            kind: AuditFinding::LegacyComment,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, dir.path(), &mut fixes, &mut skipped);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].insertions.len(), 1);
        assert_eq!(fixes[0].insertions[0].safety_tier, FixSafetyTier::Safe);
        match &fixes[0].insertions[0].kind {
            InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            } => {
                assert_eq!(*start_line, 1);
                assert_eq!(*end_line, 4);
            }
            other => panic!("Expected FunctionRemoval, got {:?}", other),
        }
    }

    #[test]
    fn generates_plan_only_for_else_branch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src/branch.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "if new_way() {\n    new();\n} else {\n    // legacy: old path\n    old();\n}\n",
        )
        .unwrap();

        let mut result = empty_result();
        result.source_path = dir.path().to_string_lossy().to_string();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/branch.rs".to_string(),
            description: "Potential legacy/stale comment on line 4: legacy: old path".to_string(),
            suggestion: "Validate".to_string(),
            kind: AuditFinding::LegacyComment,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, dir.path(), &mut fixes, &mut skipped);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].insertions[0].safety_tier, FixSafetyTier::PlanOnly);
        assert!(fixes[0].insertions[0].blocked_reason.is_some());
    }

    #[test]
    fn todo_marker_is_always_plan_only() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/lib.rs".to_string(),
            description: "Comment marker 'TODO' found on line 42: implement caching".to_string(),
            suggestion: "Resolve".to_string(),
            kind: AuditFinding::TodoMarker,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].insertions[0].safety_tier, FixSafetyTier::PlanOnly);
        assert_eq!(fixes[0].insertions[0].finding, AuditFinding::TodoMarker);
    }

    #[test]
    fn ignores_other_finding_kinds() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/lib.rs".to_string(),
            description: "Something on line 10".to_string(),
            suggestion: "".to_string(),
            kind: AuditFinding::MissingMethod,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert!(fixes.is_empty());
        assert!(skipped.is_empty());
    }

    // ── Helper function tests ─────────────────────────────────────────

    #[test]
    fn find_brace_block_end_simple() {
        let lines = vec!["fn foo() {", "    bar();", "}"];
        assert_eq!(find_brace_block_end(&lines, 0), Some(2));
    }

    #[test]
    fn find_brace_block_end_nested() {
        let lines = vec!["if a {", "    if b {", "        c();", "    }", "}"];
        assert_eq!(find_brace_block_end(&lines, 0), Some(4));
    }

    #[test]
    fn is_inside_else_branch_true() {
        let lines = vec![
            "if a {",
            "    x();",
            "} else {",
            "    // legacy: fallback",
            "    y();",
            "}",
        ];
        assert!(is_inside_else_branch(&lines, 3));
    }

    #[test]
    fn is_inside_else_branch_false_in_if() {
        let lines = vec!["if a {", "    // workaround for bug", "    x();", "}"];
        assert!(!is_inside_else_branch(&lines, 1));
    }

    #[test]
    fn match_arm_single_line() {
        let lines = vec![
            "match v {",
            "    // legacy: old format",
            "    1 => old(),",
            "    2 => new(),",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&lines, 1);
        assert_eq!(kind, BlockKind::MatchArm);
        assert_eq!(start, 2);
        assert_eq!(end, 3);
    }

    #[test]
    fn match_arm_with_block() {
        let lines = vec![
            "match v {",
            "    // temporary: compat handler",
            "    1 => {",
            "        convert();",
            "        process();",
            "    }",
            "    _ => default(),",
            "}",
        ];
        let (kind, start, end) = classify_and_bound(&lines, 1);
        assert_eq!(kind, BlockKind::MatchArm);
        assert_eq!(start, 2);
        assert_eq!(end, 6);
    }
}
