//! Intra-method duplicate autofix — remove adjacent duplicated code blocks.
//!
//! When the same block of code appears twice within a function, and the two
//! occurrences are adjacent (no intervening logic), the second occurrence is
//! a merge artifact or copy-paste error and can be safely removed.
//!
//! Non-adjacent duplicates (e.g., same setup in different if/else branches)
//! require extract-to-helper refactoring and are skipped — those need human
//! judgment about parameter extraction, return values, and naming.

use std::path::Path;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::engine::local_files;
use crate::refactor::auto::{Fix, FixSafetyTier, Insertion, InsertionKind, SkippedFile};

/// Generate fixes for intra-method duplicates where blocks are adjacent.
///
/// Parses the finding description to extract method name, line numbers, and
/// block size. Reads the source file to verify the blocks are truly adjacent
/// (no meaningful code between them), then generates a deletion fix using
/// `FunctionRemoval` to remove the second block.
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
            Ok(n) => n,
            Err(_) => continue,
        };
        let first_line: usize = match caps[3].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let second_line: usize = match caps[4].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Only fix adjacent duplicates — where the second block starts right
        // after the first block ends (allowing blank/comment lines between).
        if second_line <= first_line {
            continue;
        }

        let gap = second_line.saturating_sub(first_line + block_size);

        // Read the file to check if the gap contains only blank/comment lines
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

        // Verify the gap between the two blocks is empty (blank/comment only)
        let gap_is_empty = if gap > 0 {
            let gap_start = first_line + block_size; // 1-indexed line after block 1
            let gap_end = second_line; // 1-indexed line where block 2 starts
            (gap_start..gap_end).all(|line_num| {
                if line_num == 0 || line_num > lines.len() {
                    return false;
                }
                let line = lines[line_num - 1].trim();
                line.is_empty() || line.starts_with("//") || line.starts_with('#')
            })
        } else {
            true // Blocks are immediately adjacent
        };

        if !gap_is_empty {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Non-adjacent duplicate in `{}` — logic between blocks requires extract-to-helper",
                    method_name,
                ),
            });
            continue;
        }

        // Validate line ranges
        let remove_end = second_line + block_size - 1; // 1-indexed inclusive
        if second_line == 0 || remove_end > lines.len() {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Line range out of bounds for duplicate in `{}`",
                    method_name,
                ),
            });
            continue;
        }

        // Include gap lines in the removal range (blank lines between blocks)
        let removal_start = if gap > 0 {
            first_line + block_size // Start removing from gap
        } else {
            second_line // No gap, start from second block
        };

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: removal_start,
                    end_line: remove_end,
                },
                finding: AuditFinding::IntraMethodDuplicate,
                safety_tier: FixSafetyTier::Safe,
                auto_apply: false,
                blocked_reason: None,
                preflight: None,
                code: String::new(),
                description: format!(
                    "Remove duplicate block in `{}` (lines {}–{}) — identical to lines {}–{}",
                    method_name,
                    removal_start,
                    remove_end,
                    first_line,
                    first_line + block_size - 1,
                ),
            }],
            applied: false,
        });
    }
}
