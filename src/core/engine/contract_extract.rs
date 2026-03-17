//! Grammar-driven function contract extraction.
//!
//! Analyzes function bodies using patterns defined in `grammar.toml [contract]`
//! to produce `FunctionContract` structs. No language-specific logic — all
//! pattern knowledge comes from the grammar.
//!
//! This is the primary extraction path. The `scripts/contract.sh` extension
//! hook exists as a fallback for languages that need full AST parsing.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

use super::contract::*;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};

/// Extract function contracts from a source file using grammar-driven analysis.
///
/// Returns `None` if the grammar has no `[contract]` section.
pub fn extract_contracts_from_grammar(
    content: &str,
    file_path: &str,
    grammar: &Grammar,
) -> Option<Vec<FunctionContract>> {
    let contract_grammar = grammar.contract.as_ref()?;

    // Step 1: Walk the file to get context-aware lines
    let lines = grammar::walk_lines(content, grammar);
    let raw_lines: Vec<&str> = content.lines().collect();

    // Step 2: Extract function symbols to find function boundaries
    let function_symbols = grammar::extract(content, grammar)
        .into_iter()
        .filter(|s| s.concept == "function")
        .collect::<Vec<_>>();

    let mut contracts = Vec::new();

    for sym in &function_symbols {
        let fn_name = match sym.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let fn_line = sym.line; // 1-indexed
        let fn_depth = sym.depth;

        // Find the function body range: from the opening brace to the matching close
        let (body_start, body_end) = match find_function_body_range(&lines, fn_line, fn_depth) {
            Some(range) => range,
            None => continue,
        };

        // Extract signature info
        let params_str = sym.get("params").unwrap_or("");
        let visibility = sym.get("visibility").map(|v| v.trim());
        let is_public = visibility.map_or(false, |v| v.starts_with("pub"));

        // Detect return type from the declaration line(s)
        let decl_text = raw_lines
            .get(fn_line.saturating_sub(1))
            .copied()
            .unwrap_or("");
        let return_type = detect_return_shape(decl_text, contract_grammar);

        // Parse params
        let params = parse_params(params_str);

        // Detect receiver
        let receiver = detect_receiver(params_str);

        // Filter body lines (only code lines within the function body)
        let body_lines: Vec<(usize, &str)> = lines
            .iter()
            .filter(|l| {
                l.line_num > body_start
                    && l.line_num < body_end
                    && l.region == Region::Code
                    && l.depth > fn_depth
            })
            .map(|l| (l.line_num, l.text))
            .collect();

        // Step 3: Analyze function body for effects, branches, calls
        let effects = detect_effects(&body_lines, contract_grammar);
        let branches = detect_branches(&body_lines, &return_type, contract_grammar);
        let early_returns = count_early_returns(&body_lines, contract_grammar);
        let calls = detect_calls(&body_lines, &params);

        // Detect async
        let is_async = decl_text.contains("async ");

        contracts.push(FunctionContract {
            name: fn_name,
            file: file_path.to_string(),
            line: fn_line,
            signature: Signature {
                params,
                return_type,
                receiver,
                is_public,
                is_async,
                generics: vec![], // TODO: extract from grammar
            },
            branches,
            early_returns,
            effects,
            calls,
        });
    }

    Some(contracts)
}

/// Find the line range of a function's body (opening brace to closing brace).
///
/// Returns `(body_start_line, body_end_line)` as 1-indexed inclusive.
/// `body_start_line` is the line with the opening brace.
/// `body_end_line` is the line with the closing brace.
fn find_function_body_range(
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

        // Look for the closing brace (depth returns to fn_depth)
        if ctx_line.depth <= fn_depth && ctx_line.text.trim().starts_with('}') {
            return Some((body_start?, ctx_line.line_num));
        }
    }

    None
}

/// Detect the return type shape from the function declaration line.
fn detect_return_shape(decl_line: &str, contract: &ContractGrammar) -> ReturnShape {
    // Extract the return type portion (after "->")
    let return_part = match decl_line.split("->").nth(1) {
        Some(part) => part.trim().trim_end_matches('{').trim(),
        None => return ReturnShape::Unit,
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
fn extract_result_types(s: &str) -> (String, String) {
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
fn find_top_level_comma(s: &str) -> Option<usize> {
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
fn extract_generic_inner(s: &str) -> String {
    if let Some(start) = s.find('<') {
        if let Some(end) = s.rfind('>') {
            return s[start + 1..end].trim().to_string();
        }
    }
    s.to_string()
}

/// Parse function parameters from the params string.
fn parse_params(params_str: &str) -> Vec<Param> {
    let params_str = params_str.trim();
    if params_str.is_empty() {
        return vec![];
    }

    let mut params = Vec::new();

    for part in split_params(params_str) {
        let part = part.trim();
        // Skip self/receiver params
        if part == "self"
            || part == "&self"
            || part == "&mut self"
            || part == "mut self"
            || part.is_empty()
        {
            continue;
        }

        // Parse "name: Type" pattern
        if let Some(colon_pos) = part.find(':') {
            let name = part[..colon_pos]
                .trim()
                .trim_start_matches("mut ")
                .to_string();
            let param_type = part[colon_pos + 1..].trim().to_string();
            let mutable = part.starts_with("mut ") || param_type.starts_with("&mut ");
            params.push(Param {
                name,
                param_type,
                mutable,
                has_default: false,
            });
        }
    }

    params
}

/// Split parameter string by commas, respecting generic angle brackets.
fn split_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in s.chars() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Detect the receiver type from the params string.
fn detect_receiver(params_str: &str) -> Option<Receiver> {
    let first = params_str.split(',').next()?.trim();
    if first == "&mut self" {
        Some(Receiver::MutRef)
    } else if first == "&self" {
        Some(Receiver::Ref)
    } else if first == "self" || first == "mut self" {
        Some(Receiver::OwnedSelf)
    } else {
        None
    }
}

/// Detect side effects within function body lines using grammar patterns.
fn detect_effects(body_lines: &[(usize, &str)], contract: &ContractGrammar) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let mut seen_kinds: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (effect_kind, patterns) in &contract.effects {
        for pattern in patterns {
            let re = match Regex::new(pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for (_line_num, text) in body_lines {
                if re.is_match(text) && seen_kinds.insert(effect_kind.clone()) {
                    let effect = match effect_kind.as_str() {
                        "file_read" => Effect::FileRead,
                        "file_write" => Effect::FileWrite,
                        "file_delete" => Effect::FileDelete,
                        "process_spawn" => {
                            // Try to extract the command name
                            let cmd = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string());
                            Effect::ProcessSpawn { command: cmd }
                        }
                        "mutation" => {
                            let target = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            Effect::Mutation { target }
                        }
                        "panic" => {
                            let msg = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string());
                            Effect::Panic { message: msg }
                        }
                        "network" => Effect::Network,
                        "resource_alloc" => Effect::ResourceAlloc { resource: None },
                        "logging" => Effect::Logging,
                        _ => continue,
                    };
                    effects.push(effect);
                    break; // Only add each effect kind once per function
                }
            }
        }
    }

    // Also detect panics from panic_patterns
    for pattern in &contract.panic_patterns {
        if let Ok(re) = Regex::new(pattern) {
            for (_line_num, text) in body_lines {
                if re.is_match(text) && seen_kinds.insert("panic".to_string()) {
                    let msg = re
                        .captures(text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string());
                    effects.push(Effect::Panic { message: msg });
                    break;
                }
            }
        }
    }

    effects
}

/// Detect return branches within function body lines.
fn detect_branches(
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

/// Look backwards from a return statement to find the enclosing condition.
fn find_branch_condition(body_lines: &[(usize, &str)], return_line: usize) -> Option<String> {
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

/// Count early returns (guard clauses) in the function body.
fn count_early_returns(body_lines: &[(usize, &str)], contract: &ContractGrammar) -> usize {
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

/// Detect function calls within the body and track parameter forwarding.
fn detect_calls(body_lines: &[(usize, &str)], params: &[Param]) -> Vec<FunctionCall> {
    let mut calls: Vec<FunctionCall> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Simple call detection: word followed by (
    let call_re = Regex::new(r"(\w+(?:::\w+)*)\s*\(").unwrap();

    let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();

    for (_line_num, text) in body_lines {
        for caps in call_re.captures_iter(text) {
            let fn_name = caps[1].to_string();

            // Skip common non-function keywords
            if matches!(
                fn_name.as_str(),
                "if" | "while"
                    | "for"
                    | "match"
                    | "return"
                    | "let"
                    | "Some"
                    | "None"
                    | "Ok"
                    | "Err"
                    | "vec"
                    | "format"
                    | "println"
                    | "eprintln"
                    | "write"
                    | "writeln"
            ) {
                continue;
            }

            if !seen.insert(fn_name.clone()) {
                continue;
            }

            // Check which params are forwarded to this call
            let call_text = text.trim();
            let forwards: Vec<String> = param_names
                .iter()
                .filter(|&&p| {
                    // Check if the param name appears in the same line as the call
                    call_text.contains(p)
                })
                .map(|&p| p.to_string())
                .collect();

            calls.push(FunctionCall {
                function: fn_name,
                forwards,
            });
        }
    }

    calls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_contract_grammar() -> ContractGrammar {
        let mut effects = HashMap::new();
        effects.insert(
            "file_read".to_string(),
            vec![r"std::fs::read|fs::read_to_string|File::open".to_string()],
        );
        effects.insert(
            "file_write".to_string(),
            vec![r"std::fs::write|fs::write".to_string()],
        );
        effects.insert(
            "process_spawn".to_string(),
            vec![r"Command::new\((.+?)\)".to_string()],
        );

        let mut return_patterns = HashMap::new();
        return_patterns.insert("ok".to_string(), vec![r"Ok\((.+?)\)".to_string()]);
        return_patterns.insert("err".to_string(), vec![r"Err\((.+?)\)".to_string()]);
        return_patterns.insert("some".to_string(), vec![r"Some\((.+?)\)".to_string()]);
        return_patterns.insert("none".to_string(), vec![r"\breturn\s+None\b".to_string()]);

        let mut return_shapes = HashMap::new();
        return_shapes.insert("result".to_string(), vec![r"Result\s*<".to_string()]);
        return_shapes.insert("option".to_string(), vec![r"Option\s*<".to_string()]);
        return_shapes.insert("bool".to_string(), vec![r"^\s*bool\s*$".to_string()]);
        return_shapes.insert("collection".to_string(), vec![r"Vec\s*<".to_string()]);

        ContractGrammar {
            effects,
            guard_patterns: vec![
                r"if\s+.*\{\s*return\s+".to_string(),
                r"if\s+.*\.is_empty\(\)".to_string(),
            ],
            return_patterns,
            error_propagation: vec![r"\?\s*;".to_string()],
            return_shapes,
            panic_patterns: vec![
                r"panic!\s*\((.+?)\)".to_string(),
                r"unreachable!\s*\(".to_string(),
                r"\.unwrap\(\)".to_string(),
            ],
            test_templates: HashMap::new(),
        }
    }

    #[test]
    fn detect_return_shape_result() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("pub fn foo() -> Result<String, Error> {", &cg);
        assert!(matches!(shape, ReturnShape::ResultType { .. }));
        if let ReturnShape::ResultType { ok_type, err_type } = shape {
            assert_eq!(ok_type, "String");
            assert_eq!(err_type, "Error");
        }
    }

    #[test]
    fn detect_return_shape_option() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn bar() -> Option<usize> {", &cg);
        assert!(matches!(shape, ReturnShape::OptionType { .. }));
    }

    #[test]
    fn detect_return_shape_bool() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn baz() -> bool {", &cg);
        assert!(matches!(shape, ReturnShape::Bool));
    }

    #[test]
    fn detect_return_shape_unit() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn qux() {", &cg);
        assert!(matches!(shape, ReturnShape::Unit));
    }

    #[test]
    fn parse_params_basic() {
        let params = parse_params("root: &Path, files: &[PathBuf]");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "root");
        assert_eq!(params[0].param_type, "&Path");
        assert_eq!(params[1].name, "files");
    }

    #[test]
    fn parse_params_with_self() {
        let params = parse_params("&self, key: &str");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "key");
    }

    #[test]
    fn parse_params_empty() {
        let params = parse_params("");
        assert!(params.is_empty());
    }

    #[test]
    fn detect_receiver_ref() {
        assert!(matches!(
            detect_receiver("&self, key: &str"),
            Some(Receiver::Ref)
        ));
    }

    #[test]
    fn detect_receiver_mut_ref() {
        assert!(matches!(
            detect_receiver("&mut self"),
            Some(Receiver::MutRef)
        ));
    }

    #[test]
    fn detect_receiver_none() {
        assert!(detect_receiver("key: &str").is_none());
    }

    #[test]
    fn split_params_with_generics() {
        let parts = split_params("map: HashMap<String, Vec<u8>>, count: usize");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("HashMap"));
        assert!(parts[1].contains("usize"));
    }

    #[test]
    fn detect_effects_from_body() {
        let cg = make_contract_grammar();
        let body = vec![
            (10, "    let content = std::fs::read_to_string(path)?;"),
            (11, "    std::fs::write(output, &content)?;"),
        ];
        let effects = detect_effects(&body, &cg);
        assert!(effects.iter().any(|e| matches!(e, Effect::FileRead)));
        assert!(effects.iter().any(|e| matches!(e, Effect::FileWrite)));
    }

    #[test]
    fn extract_result_types_basic() {
        let (ok, err) = extract_result_types("Result<ValidationResult, Error>");
        assert_eq!(ok, "ValidationResult");
        assert_eq!(err, "Error");
    }

    #[test]
    fn extract_generic_inner_basic() {
        assert_eq!(extract_generic_inner("Option<String>"), "String");
        assert_eq!(extract_generic_inner("Vec<u8>"), "u8");
    }
}
