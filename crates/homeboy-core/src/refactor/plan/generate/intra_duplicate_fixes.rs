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
                let DelimCounts {
                    brace: brace_depth,
                    paren: paren_depth,
                    bracket: bracket_depth,
                } = count_delimiters(removal_lines);
                if brace_depth != 0 || paren_depth != 0 || bracket_depth != 0 {
                    skipped.push(SkippedFile {
                        file: finding.file.clone(),
                        reason: format!(
                            "Duplicate block in `{}` (lines {}-{}) has unbalanced delimiters (braces: {}, parens: {}, brackets: {}) — removal would corrupt file",
                            method_name, second_line, second_end, brace_depth, paren_depth, bracket_depth,
                        ),
                    });
                    continue;
                }

                // Boundary-depth check: even when the removed slice is internally
                // balanced, we may be cutting into the middle of an open expression
                // (e.g. a multi-line function call, array literal, or match arm).
                // Walk cumulative paren/bracket depth from the top of the file to
                // the line immediately before `removal_start`. If either depth is
                // positive, we're mid-expression and removing the slice would
                // delete arguments/elements that belong to the enclosing call.
                //
                // Brace depth is intentionally ignored — method bodies always sit
                // inside a `{`, so brace depth is expected to be > 0 at the
                // boundary. Parens and brackets, by contrast, should be 0 at any
                // statement boundary.
                let prefix = &lines[..removal_start.saturating_sub(1).min(lines.len())];
                let DelimCounts {
                    paren: prefix_paren,
                    bracket: prefix_bracket,
                    ..
                } = count_delimiters(prefix);
                if prefix_paren > 0 || prefix_bracket > 0 {
                    skipped.push(SkippedFile {
                        file: finding.file.clone(),
                        reason: format!(
                            "Duplicate block in `{}` starts inside an open expression (paren depth: {}, bracket depth: {}) — auto-removal would corrupt the enclosing call/array. Resolve manually by extracting a helper.",
                            method_name, prefix_paren, prefix_bracket,
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

/// Net delimiter depths for a slice of source lines.
///
/// Computed by scanning each line character-by-character while skipping
/// `//` line comments and `"…"` / `'…'` string literals. This is a
/// language-agnostic approximation — it handles Rust, PHP, and JS well enough
/// for the safety checks in this file. It intentionally does NOT handle:
/// Rust raw strings (`r#"..."#`), block comments (`/* … */`), or nested
/// string interpolation. False positives from those cases downgrade an
/// autofix to manual-only, which is the safe direction.
struct DelimCounts {
    brace: i32,
    paren: i32,
    bracket: i32,
}

fn count_delimiters(lines: &[&str]) -> DelimCounts {
    let mut brace = 0i32;
    let mut paren = 0i32;
    let mut bracket = 0i32;

    for line in lines {
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                // Skip `//` line comments — everything else on this line is comment.
                '/' if chars.peek() == Some(&'/') => break,
                // Skip `#` line comments (shell/PHP hash comments).
                '#' => break,
                // Skip string literals — walk to the matching quote, honoring
                // backslash escapes. This keeps delimiters inside strings
                // from skewing the depth count (e.g. a `"("` literal).
                '"' | '\'' => {
                    let quote = ch;
                    let mut prev_was_backslash = false;
                    for inner in chars.by_ref() {
                        if prev_was_backslash {
                            prev_was_backslash = false;
                            continue;
                        }
                        if inner == '\\' {
                            prev_was_backslash = true;
                            continue;
                        }
                        if inner == quote {
                            break;
                        }
                    }
                }
                '{' => brace += 1,
                '}' => brace -= 1,
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => bracket += 1,
                ']' => bracket -= 1,
                _ => {}
            }
        }
    }

    DelimCounts {
        brace,
        paren,
        bracket,
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
    fn count_delimiters_balanced_block() {
        let lines = vec![
            "    let x = foo(1, 2);",
            "    let y = bar([3, 4]);",
            "    { let z = 5; }",
        ];
        let c = count_delimiters(&lines);
        assert_eq!(c.brace, 0);
        assert_eq!(c.paren, 0);
        assert_eq!(c.bracket, 0);
    }

    #[test]
    fn count_delimiters_ignores_delimiters_in_strings() {
        // A line containing "(" inside a string literal should not change
        // paren depth.
        let lines = vec![r#"    let s = "value is (bogus";"#, "    let t = 1;"];
        let c = count_delimiters(&lines);
        assert_eq!(c.paren, 0);
    }

    #[test]
    fn count_delimiters_ignores_line_comments() {
        // Delimiters after `//` are inside a comment — must not count.
        let lines = vec!["    let x = 1; // trailing ( paren", "    let y = 2;"];
        let c = count_delimiters(&lines);
        assert_eq!(c.paren, 0);
    }

    #[test]
    fn count_delimiters_handles_escaped_quotes_in_strings() {
        // The escaped quote must not close the string early.
        let lines = vec![r#"    let s = "with \"quoted\" ( paren";"#];
        let c = count_delimiters(&lines);
        assert_eq!(c.paren, 0);
    }

    #[test]
    fn count_delimiters_open_paren_increments() {
        // Multi-line open paren — one line with `(` and nothing to close it.
        let lines = vec!["    foo(", "        arg,"];
        let c = count_delimiters(&lines);
        assert_eq!(c.paren, 1);
    }

    #[test]
    fn count_delimiters_open_bracket_increments() {
        let lines = vec!["    let v = [", "        1,"];
        let c = count_delimiters(&lines);
        assert_eq!(c.bracket, 1);
    }

    // Regression test for issue #1164 — intra_method_duplicate Adjacent
    // autofix collapsed a multi-line make_fingerprint(…) call by removing
    // the duplicated prefix lines, which sat *inside* an open `(` boundary.
    // The boundary-depth check must reject this kind of removal so the fix
    // is demoted to manual_only instead of shipping compile-broken code.
    //
    // Shape (reduced from the real seed):
    //
    //   let base = make_fingerprint(
    //       "Foo.php",
    //       vec![], vec![], vec![],
    //       None, None,
    //       vec![("action", "a")],
    //   );
    //   let current = make_fingerprint(
    //       "Foo.php",
    //       vec![], vec![], vec![],
    //       None, None,
    //       vec![("action", "b")],
    //   );
    //
    // The 5-line window `"Foo.php", / vec![], vec![], vec![], / None, None,`
    // hashes identically in both calls. Without the boundary-depth check the
    // fixer would delete the second block, leaving `make_fingerprint( vec![…] )`
    // with the wrong argument count.
    #[test]
    fn boundary_depth_check_blocks_removal_inside_open_call() {
        // Lines up to and including the line *before* the removal point
        // (`removal_start - 1`). For the duplicated-second-call case, the
        // removal would start on the `"Foo.php",` line of the second call,
        // which sits inside an open `make_fingerprint(` paren opened above.
        let prefix = vec![
            "fn demo() {",
            "    let base = make_fingerprint(",
            "        \"Foo.php\",",
            "        vec![], vec![], vec![],",
            "        None, None,",
            "        vec![(\"action\", \"a\")],",
            "    );",
            "    let current = make_fingerprint(",
            // Removal would start on the next line — this prefix should
            // end here with paren depth = 1.
        ];
        let c = count_delimiters(&prefix);
        assert_eq!(
            c.paren, 1,
            "removal starts inside open make_fingerprint(…) — paren depth must be > 0",
        );
    }

    #[test]
    fn boundary_depth_check_allows_removal_at_statement_boundary() {
        // Same overall shape but the removal point is after the closing `);` —
        // the removal boundary is at statement level, paren depth should be 0.
        let prefix = vec![
            "fn demo() {",
            "    let base = some_call();",
            "    let first = compute();",
            // Removal would start below — paren depth must be 0.
        ];
        let c = count_delimiters(&prefix);
        assert_eq!(c.paren, 0);
        assert_eq!(c.bracket, 0);
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
