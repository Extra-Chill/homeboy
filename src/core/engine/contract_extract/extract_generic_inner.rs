//! extract_generic_inner — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::super::contract::*;


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

/// Extract Ok and Err types from a Result<T, E> string.
pub(crate) fn extract_result_types(s: &str) -> (String, String) {
    // Simple extraction: Result<OkType, ErrType>
    let inner = extract_generic_inner(s);
    if let Some(comma_pos) = find_top_level_comma(&inner) {
        let ok_t = inner[..comma_pos].trim().to_string();
        let err_t = inner[comma_pos + 1..].trim().to_string();
        (ok_t, err_t)
    } else {
        (inner, "Error".to_string())
    }
}

/// Find the position of a comma at the top level of generics (not inside nested <>).
pub(crate) fn find_top_level_comma(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Extract the inner type from a generic type like `Option<T>` or `Vec<T>`.
pub(crate) fn extract_generic_inner(s: &str) -> String {
    if let Some(start) = s.find('<') {
        if let Some(end) = s.rfind('>') {
            return s[start + 1..end].trim().to_string();
        }
    }
    s.to_string()
}
