//! helpers — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::extract_generic_inner;
use super::extract_result_types;
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

/// Detect the return type shape from the function declaration line.
pub(crate) fn detect_return_shape(decl_line: &str, contract: &ContractGrammar) -> ReturnShape {
    // Extract the return type portion after the language-specific separator.
    // For multi-char separators like "->" (Rust), split on the separator.
    // For single-char separators like ":" (PHP), find the separator that
    // follows the closing ")" of the parameter list to avoid matching
    // namespace separators or ternary operators.
    let separator = &contract.return_type_separator;
    let return_part = if separator.len() == 1 {
        // Single-char separator: find `)` then look for separator after it
        let sep_char = separator.chars().next().unwrap();
        let after_paren = match decl_line.rfind(')') {
            Some(pos) => &decl_line[pos + 1..],
            None => return ReturnShape::Unit,
        };
        match after_paren.find(sep_char) {
            Some(pos) => after_paren[pos + 1..].trim().trim_end_matches('{').trim(),
            None => return ReturnShape::Unit,
        }
    } else {
        // Multi-char separator like "->": simple split
        match decl_line.split(separator.as_str()).nth(1) {
            Some(part) => part.trim().trim_end_matches('{').trim(),
            None => return ReturnShape::Unit,
        }
    };

    if return_part.is_empty() {
        return ReturnShape::Unit;
    }

    // Check grammar-defined return shape patterns
    for (shape_name, patterns) in &contract.return_shapes {
        for pattern in patterns {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(return_part) {
                    return match shape_name.as_str() {
                        "result" => {
                            let (ok_t, err_t) = extract_result_types(return_part);
                            ReturnShape::ResultType {
                                ok_type: ok_t,
                                err_type: err_t,
                            }
                        }
                        "option" => {
                            let inner = extract_generic_inner(return_part);
                            ReturnShape::OptionType { some_type: inner }
                        }
                        "bool" => ReturnShape::Bool,
                        "collection" => {
                            let inner = extract_generic_inner(return_part);
                            ReturnShape::Collection {
                                element_type: inner,
                            }
                        }
                        _ => ReturnShape::Value {
                            value_type: return_part.to_string(),
                        },
                    };
                }
            }
        }
    }

    // Fallback: raw type
    ReturnShape::Value {
        value_type: return_part.to_string(),
    }
}
