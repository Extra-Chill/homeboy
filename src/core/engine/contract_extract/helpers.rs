//! helpers — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::super::contract::*;


/// Find the line range of a function's body (opening brace to closing brace).
///
/// Returns `(body_start_line, body_end_line)` as 1-indexed inclusive.
/// `body_start_line` is the line with the opening brace.
/// `body_end_line` is the line with the closing brace.
pub(crate) fn find_function_body_range(
    lines: &[grammar::ContextualLine],
    fn_line: usize,
    fn_depth: i32,
) -> Option<(usize, usize)> {
    let mut body_start = None;
    let mut found_open = false;

    for ctx_line in lines {
        if ctx_line.line_num < fn_line {
            continue;
        }

        // Look for the opening brace (depth increases past fn_depth)
        if !found_open {
            if ctx_line.text.contains('{') && ctx_line.line_num >= fn_line {
                body_start = Some(ctx_line.line_num);
                found_open = true;
            }
            continue;
        }

        // Look for the closing brace. walk_lines records depth_at_start (depth
        // BEFORE processing braces on this line), so the line with the closing `}`
        // has depth fn_depth + 1, not fn_depth. Check <= fn_depth + 1.
        if ctx_line.depth <= fn_depth + 1 && ctx_line.text.trim().starts_with('}') {
            return Some((body_start?, ctx_line.line_num));
        }
    }

    None
}

/// Join lines from the function declaration through the opening brace into a
/// single string. This captures multi-line signatures where params and/or
/// the return type span continuation lines.
///
/// Example:
/// ```ignore
/// pub fn complex_function(
///     root: &Path,
///     files: &[PathBuf],
///     config: &Config,
/// ) -> Result<(), Error> {
/// ```
/// Becomes: `pub fn complex_function( root: &Path, files: &[PathBuf], config: &Config, ) -> Result<(), Error> {`
pub(crate) fn join_declaration_lines(raw_lines: &[&str], fn_line: usize, body_start: usize) -> String {
    // fn_line is 1-indexed, body_start is the line with `{`
    let start_idx = fn_line.saturating_sub(1);
    let end_idx = body_start.min(raw_lines.len()); // inclusive

    raw_lines[start_idx..end_idx]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Count early returns (guard clauses) in the function body.
pub(crate) fn count_early_returns(body_lines: &[(usize, &str)], contract: &ContractGrammar) -> usize {
    let mut count = 0;

    for pattern in &contract.guard_patterns {
        if let Ok(re) = Regex::new(pattern) {
            for (_line_num, text) in body_lines {
                if re.is_match(text) {
                    count += 1;
                }
            }
        }
    }

    count
}
