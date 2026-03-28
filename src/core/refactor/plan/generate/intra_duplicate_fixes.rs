//! Intra-method duplicate autofix — remove or flag duplicated code blocks within functions.
//!
//! When the same block of code appears twice within a function, the fix depends
//! on the structural relationship between the two occurrences:
//!
//! **Adjacent or same-indent duplicates**: The second block is a merge artifact
//! or copy-paste error. Automation-eligible removal — the first copy already does the work.
//!
//! **Cross-branch duplicates** (different indent levels): The same code appears
//! in multiple branches (if/else, match arms). These are structural repetition
//! where the fix requires human judgment about refactoring — manual-only with both
//! ranges identified.
//!
//! The algorithm is language-agnostic — it operates on line content, indentation
//! levels, and relative positions. No language parsing.

use std::path::Path;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::engine::local_files;
use crate::refactor::auto::{Fix, SkippedFile};

use super::{manual_blocked, range_removal};

/// Structural relationship between two duplicated blocks.
enum DupRelation {
    /// Blocks are adjacent — only blank/comment lines between them.
    /// Second block is a merge artifact. Automation-eligible to remove.
    Adjacent,
    /// Blocks are at the same indentation with a small gap of code between.
    /// Likely a copy-paste error — the gap is context that doesn't depend on
    /// the duplicated block. Automation-eligible to remove the second copy.
    SameIndentSmallGap,
    /// Blocks are at the same indentation but with a large gap of code.
    /// Could be intentional repetition at different stages. Manual-only.
    SameIndentLargeGap,
    /// Blocks are at different indentation levels — they're in different
    /// branches (if/else, match arms, nested blocks). Structural repetition
    /// that requires refactoring to consolidate. Manual-only.
    CrossBranch,
}

/// Generate fixes for intra-method duplicates.
///
/// Classifies the structural relationship between the two blocks and generates
/// either an automation-eligible removal (adjacent/same-indent) or a manual-only
/// flag (cross-branch).
pub(crate) fn generate_intra_duplicate_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    let line_re = regex::Regex::new(
        r"Duplicated block in `(\w+)` — (\d+) identical lines at line (\d+) and line (\d+)",
    )
    .expect("intra-duplicate regex should compile");

    for finding in &result.findings {
        if finding.kind != AuditFinding::IntraMethodDuplicate {
            continue;
        }

        let caps = match line_re.captures(&finding.description) {
            Some(c) => c,
            None => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: "Could not parse intra-method duplicate description".to_string(),
                });
                continue;
            }
        };

        let method_name = caps[1].to_string();
        let block_size: usize = match caps[2].parse() {
            Ok(n) if n > 0 => n,
            _ => continue,
        };
        let first_line: usize = match caps[3].parse() {
            Ok(n) if n > 0 => n,
            _ => continue,
        };
        let second_line: usize = match caps[4].parse() {
            Ok(n) if n > 0 => n,
            _ => continue,
        };

        if second_line <= first_line {
            continue;
        }

        // Read the source file.
        let file_path = root.join(&finding.file);
        let content = match local_files::read_file(&file_path, "read source for duplicate fix") {
            Ok(c) => c,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: "Could not read file".to_string(),
                });
                continue;
            }
        };

        let lines: Vec<&str> = content.lines().collect();

        // Validate line ranges.
        let first_end = first_line + block_size - 1;
        let second_end = second_line + block_size - 1;
        if first_end > lines.len() || second_end > lines.len() {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Line range out of bounds for duplicate in `{}`",
                    method_name,
                ),
            });
            continue;
        }

        // Classify the relationship.
        let relation = classify_relation(&lines, first_line, second_line, block_size);

        match relation {
            DupRelation::Adjacent | DupRelation::SameIndentSmallGap => {
                // Automation-eligible removal of the second block.
                // For adjacent: also remove gap lines (blank/comment between blocks).
                let removal_start = if matches!(relation, DupRelation::Adjacent) {
                    // Include gap lines.
                    first_line + block_size
                } else {
                    second_line
                };

                // Check that the lines being removed have balanced braces.
                // Duplicated blocks inside closures or match arms may contain
                // only half of a brace pair (e.g., the opening `{` is above
                // the block, the closing `}` is inside it). Removing such a
                // block corrupts the file's delimiter structure.
                let removal_lines =
                    &lines[removal_start.saturating_sub(1)..second_end.min(lines.len())];
                let mut brace_depth: i32 = 0;
                let mut paren_depth: i32 = 0;
                for line in removal_lines {
                    for ch in line.chars() {
                        match ch {
                            '{' => brace_depth += 1,
                            '}' => brace_depth -= 1,
                            '(' => paren_depth += 1,
                            ')' => paren_depth -= 1,
                            _ => {}
                        }
                    }
                }
                if brace_depth != 0 || paren_depth != 0 {
                    skipped.push(SkippedFile {
                        file: finding.file.clone(),
                        reason: format!(
                            "Duplicate block in `{}` (lines {}-{}) has unbalanced delimiters (braces: {}, parens: {}) — removal would corrupt file",
                            method_name, second_line, second_end, brace_depth, paren_depth,
                        ),
                    });
                    continue;
                }

                let ins = range_removal(
                    AuditFinding::IntraMethodDuplicate,
                    removal_start,
                    second_end,
                    format!(
                        "Remove duplicate block in `{}` (lines {}-{}) — identical to lines {}-{}",
                        method_name, second_line, second_end, first_line, first_end,
                    ),
                );

                fixes.push(Fix {
                    file: finding.file.clone(),
                    required_methods: vec![],
                    required_registrations: vec![],
                    insertions: vec![ins],
                    applied: false,
                });
            }
            DupRelation::SameIndentLargeGap | DupRelation::CrossBranch => {
                // Manual-only — flag both blocks for human review.
                let reason = match relation {
                    DupRelation::CrossBranch => format!(
                        "Cross-branch duplicate in `{}` — same block at different indent levels (branches). \
                         Consider hoisting shared logic above the branch or extracting a helper.",
                        method_name,
                    ),
                    _ => format!(
                        "Non-adjacent duplicate in `{}` — significant code between blocks. \
                         Verify the second copy is redundant before removing.",
                        method_name,
                    ),
                };

                let ins = manual_blocked(
                    range_removal(
                        AuditFinding::IntraMethodDuplicate,
                        second_line,
                        second_end,
                        format!(
                            "Duplicate block in `{}`: lines {}-{} and lines {}-{} are identical",
                            method_name, first_line, first_end, second_line, second_end,
                        ),
                    ),
                    reason,
                );

                fixes.push(Fix {
                    file: finding.file.clone(),
                    required_methods: vec![],
                    required_registrations: vec![],
                    insertions: vec![ins],
                    applied: false,
                });
            }
        }
    }
}

/// Classify the structural relationship between two duplicated blocks.
///
/// Uses indentation and gap analysis — no language parsing.
fn classify_relation(
    lines: &[&str],
    first_line: usize,
    second_line: usize,
    block_size: usize,
) -> DupRelation {
    let gap_start = first_line + block_size; // 1-indexed, first line after block 1
    let gap_end = second_line; // 1-indexed, first line of block 2

    // Check if blocks are at different indentation levels (cross-branch).
    let first_indent = median_indent(lines, first_line, block_size);
    let second_indent = median_indent(lines, second_line, block_size);

    if first_indent != second_indent {
        return DupRelation::CrossBranch;
    }

    // Same indent — check the gap.
    if gap_start >= gap_end {
        // No gap at all — immediately adjacent.
        return DupRelation::Adjacent;
    }

    // Count meaningful (non-blank, non-comment) lines in the gap.
    let mut code_lines_in_gap = 0usize;
    for line_num in gap_start..gap_end {
        if line_num == 0 || line_num > lines.len() {
            continue;
        }
        let line = lines[line_num - 1].trim();
        if !line.is_empty() && !line.starts_with("//") && !line.starts_with('#') {
            code_lines_in_gap += 1;
        }
    }

    if code_lines_in_gap == 0 {
        return DupRelation::Adjacent;
    }

    // Same indent with code in the gap.
    // Small gap (≤ 8 lines of real code): likely copy-paste with minor context between.
    // The brace-balance check in the caller prevents corrupted output even for
    // larger gap removals, so we can safely promote more cases to automation.
    // Large gap (> 8): different enough to need human review.
    if code_lines_in_gap <= 8 {
        DupRelation::SameIndentSmallGap
    } else {
        DupRelation::SameIndentLargeGap
    }
}

/// Compute the median indentation level of a block of lines.
/// Uses the median to be robust against blank lines or unusual indentation on a single line.
fn median_indent(lines: &[&str], start_line: usize, block_size: usize) -> usize {
    let mut indents: Vec<usize> = Vec::with_capacity(block_size);

    for i in 0..block_size {
        let line_idx = start_line - 1 + i; // 0-indexed
        if line_idx < lines.len() {
            let line = lines[line_idx];
            if !line.trim().is_empty() {
                indents.push(line.len() - line.trim_start().len());
            }
        }
    }

    if indents.is_empty() {
        return 0;
    }

    indents.sort_unstable();
    indents[indents.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_adjacent_no_gap() {
        let lines = vec![
            "    let x = 1;", // line 1
            "    let y = 2;", // line 2
            "    let x = 1;", // line 3
            "    let y = 2;", // line 4
        ];
        let rel = classify_relation(&lines, 1, 3, 2);
        assert!(matches!(rel, DupRelation::Adjacent));
    }

    #[test]
    fn classify_adjacent_blank_gap() {
        let lines = vec![
            "    let x = 1;", // line 1
            "    let y = 2;", // line 2
            "",               // line 3
            "    let x = 1;", // line 4
            "    let y = 2;", // line 5
        ];
        let rel = classify_relation(&lines, 1, 4, 2);
        assert!(matches!(rel, DupRelation::Adjacent));
    }

    #[test]
    fn classify_same_indent_small_gap() {
        let lines = vec![
            "    let x = 1;",     // line 1
            "    let y = 2;",     // line 2
            "    let z = x + y;", // line 3 — 1 line of code
            "    let x = 1;",     // line 4
            "    let y = 2;",     // line 5
        ];
        let rel = classify_relation(&lines, 1, 4, 2);
        assert!(matches!(rel, DupRelation::SameIndentSmallGap));
    }

    #[test]
    fn classify_same_indent_medium_gap_now_promoted() {
        // 4 lines of code in gap — was SameIndentLargeGap at threshold 3,
        // now SameIndentSmallGap at threshold 8 (automation-eligible).
        let lines = vec![
            "    let x = 1;", // line 1
            "    let y = 2;", // line 2
            "    a();",       // line 3
            "    b();",       // line 4
            "    c();",       // line 5
            "    d();",       // line 6  — 4 lines of code in gap
            "    let x = 1;", // line 7
            "    let y = 2;", // line 8
        ];
        let rel = classify_relation(&lines, 1, 7, 2);
        assert!(matches!(rel, DupRelation::SameIndentSmallGap));
    }

    #[test]
    fn classify_same_indent_large_gap() {
        // 9 lines of code in gap — exceeds threshold of 8, stays manual.
        let lines = vec![
            "    let x = 1;", // line 1
            "    let y = 2;", // line 2
            "    a();",       // line 3
            "    b();",       // line 4
            "    c();",       // line 5
            "    d();",       // line 6
            "    e();",       // line 7
            "    f();",       // line 8
            "    g();",       // line 9
            "    h();",       // line 10
            "    i();",       // line 11  — 9 lines of code in gap
            "    let x = 1;", // line 12
            "    let y = 2;", // line 13
        ];
        let rel = classify_relation(&lines, 1, 12, 2);
        assert!(matches!(rel, DupRelation::SameIndentLargeGap));
    }

    #[test]
    fn classify_cross_branch_different_indent() {
        let lines = vec![
            "    if cond {",          // line 1
            "        let x = 1;",     // line 2 — indent 8
            "        let y = 2;",     // line 3
            "    } else {",           // line 4
            "            let x = 1;", // line 5 — indent 12
            "            let y = 2;", // line 6
            "    }",                  // line 7
        ];
        let rel = classify_relation(&lines, 2, 5, 2);
        assert!(matches!(rel, DupRelation::CrossBranch));
    }

    #[test]
    fn median_indent_skips_blank() {
        let lines = vec![
            "        code();", // indent 8
            "",                // blank — skipped
            "        more();", // indent 8
        ];
        assert_eq!(median_indent(&lines, 1, 3), 8);
    }

    #[test]
    fn median_indent_handles_outlier() {
        let lines = vec![
            "        code();",  // indent 8
            "    oddly();",     // indent 4
            "        more();",  // indent 8
            "        again();", // indent 8
            "        last();",  // indent 8
        ];
        // Sorted: [4, 8, 8, 8, 8] → median at index 2 = 8
        assert_eq!(median_indent(&lines, 1, 5), 8);
    }
}
