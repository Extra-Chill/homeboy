//! find_branch_condition — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::super::contract::*;


/// Detect return branches within function body lines.
pub(crate) fn detect_branches(
    body_lines: &[(usize, &str)],
    return_type: &ReturnShape,
    contract: &ContractGrammar,
) -> Vec<Branch> {
    let mut branches = Vec::new();

    // Use grammar-defined return patterns
    for (variant, patterns) in &contract.return_patterns {
        for pattern in patterns {
            let re = match Regex::new(pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for &(line_num, text) in body_lines {
                if re.is_match(text) {
                    let trimmed = text.trim();

                    // Try to extract a value description from the capture
                    let value = re
                        .captures(text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().trim().to_string())
                        .filter(|v| !v.is_empty());

                    // Determine the condition — look at preceding lines for if/match
                    let condition = find_branch_condition(body_lines, line_num);

                    branches.push(Branch {
                        condition: condition.unwrap_or_else(|| {
                            if trimmed.starts_with("return ") || trimmed.ends_with(';') {
                                "default path".to_string()
                            } else {
                                trimmed.to_string()
                            }
                        }),
                        returns: ReturnValue {
                            variant: variant.clone(),
                            value,
                        },
                        effects: vec![],
                        line: Some(line_num),
                    });
                }
            }
        }
    }

    // Detect error propagation branches (e.g., `?` in Rust).
    // Each `?` is an implicit "if this fails, return Err" branch.
    // Rather than generating one branch per `?` (noisy), we generate
    // one branch for the first `?` site with a description of all
    // propagation points. This produces a test that verifies the
    // error path exists. (#818)
    if matches!(return_type, ReturnShape::ResultType { .. }) {
        detect_error_propagation(body_lines, contract, &mut branches);
    }

    // Deduplicate branches by line number
    branches.sort_by_key(|b| b.line);
    branches.dedup_by_key(|b| b.line);

    // If no return patterns matched but we know the return type, add a default branch
    if branches.is_empty() && !matches!(return_type, ReturnShape::Unit) {
        branches.push(Branch {
            condition: "default path".to_string(),
            returns: ReturnValue {
                variant: "value".to_string(),
                value: None,
            },
            effects: vec![],
            line: None,
        });
    }

    branches
}

/// Detect error propagation branches from `?` operator usage.
///
/// Scans body lines for patterns matching `error_propagation` in the grammar
/// (e.g., `?;` or `?` at end of line in Rust). Generates a single `Err` branch
/// describing the propagation, rather than one branch per `?` site.
///
/// The generated branch uses a descriptive condition like:
///   "error propagation via ? (3 sites: read_to_string, from_str, validate)"
/// and has variant "err" so the test pipeline generates an error-path test.
pub(crate) fn detect_error_propagation(
    body_lines: &[(usize, &str)],
    contract: &ContractGrammar,
    branches: &mut Vec<Branch>,
) {
    if contract.error_propagation.is_empty() {
        return;
    }

    let prop_regexes: Vec<Regex> = contract
        .error_propagation
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    if prop_regexes.is_empty() {
        return;
    }

    let mut prop_sites: Vec<(usize, String)> = Vec::new();

    for &(line_num, text) in body_lines {
        if prop_regexes.iter().any(|re| re.is_match(text)) {
            // Extract a short description of what's being called before the `?`
            let call_desc = extract_propagation_call(text);
            prop_sites.push((line_num, call_desc));
        }
    }

    if prop_sites.is_empty() {
        return;
    }

    // Check if we already have an explicit Err branch — if so, propagation
    // is secondary and we just note the count.
    let has_explicit_err = branches.iter().any(|b| b.returns.variant == "err");

    let first_line = prop_sites[0].0;
    let call_names: Vec<&str> = prop_sites.iter().map(|(_, name)| name.as_str()).collect();
    let condition = if prop_sites.len() == 1 {
        format!("error propagation via ? ({})", call_names[0])
    } else {
        format!(
            "error propagation via ? ({} sites: {})",
            prop_sites.len(),
            call_names.join(", ")
        )
    };

    // Only add the branch if there's no explicit Err return already,
    // or if we want to ensure propagation paths are tested too.
    if !has_explicit_err {
        branches.push(Branch {
            condition,
            returns: ReturnValue {
                variant: "err".to_string(),
                value: None,
            },
            effects: vec![],
            line: Some(first_line),
        });
    }
}

/// Extract a short description of the function call before the `?` operator.
///
/// From `let content = fs::read_to_string(path)?;` extracts `read_to_string`.
/// From `serde_json::from_str(&content)?` extracts `from_str`.
/// Falls back to "operation" for unrecognized patterns.
pub(crate) fn extract_propagation_call(line: &str) -> String {
    let trimmed = line.trim();

    // Find the `?` and work backwards to find the call
    if let Some(q_pos) = trimmed.rfind('?') {
        let before_q = &trimmed[..q_pos];
        // Look for the last function call: name(...)
        if let Some(paren_pos) = before_q.rfind('(') {
            let before_paren = &before_q[..paren_pos];
            // Extract the function/method name (last identifier before the paren)
            let name = before_paren
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("operation");
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }

    "operation".to_string()
}

/// Look backwards from a return statement to find the enclosing condition.
pub(crate) fn find_branch_condition(body_lines: &[(usize, &str)], return_line: usize) -> Option<String> {
    // Search backwards for an if/match/else statement
    for &(line_num, text) in body_lines.iter().rev() {
        if line_num >= return_line {
            continue;
        }
        // Stop searching if we go too far back
        if return_line - line_num > 5 {
            break;
        }

        let trimmed = text.trim();
        if trimmed.starts_with("if ")
            || trimmed.starts_with("} else if ")
            || trimmed.starts_with("else if ")
        {
            // Extract the condition
            let cond = trimmed
                .trim_start_matches("} ")
                .trim_start_matches("else ")
                .trim_start_matches("if ")
                .trim_end_matches('{')
                .trim();
            return Some(cond.to_string());
        }
        if trimmed.starts_with("} else") || trimmed.starts_with("else") {
            return Some("else".to_string());
        }
        if trimmed.starts_with("match ") {
            return Some(trimmed.trim_end_matches('{').trim().to_string());
        }
    }

    None
}
