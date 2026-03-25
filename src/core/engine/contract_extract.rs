//! Grammar-driven function contract extraction.
//!
//! Analyzes function bodies using patterns defined in `grammar.toml [contract]`
//! to produce `FunctionContract` structs. No language-specific logic — all
//! pattern knowledge comes from the grammar.
//!
//! This is the primary extraction path. The `scripts/contract.sh` extension
//! hook exists as a fallback for languages that need full AST parsing.

mod extract;
mod helpers;

pub use extract::*;
pub use helpers::*;


use regex::Regex;

use super::contract::*;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};

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
fn parse_params(params_str: &str, param_format: &str) -> Vec<Param> {
    let params_str = params_str.trim();
    if params_str.is_empty() {
        return vec![];
    }

    let mut params = Vec::new();

    for part in split_params(params_str) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        match param_format {
            "type_dollar_name" => {
                // PHP format: `Type $name`, `?Type $name`, `$name`, `Type $name = default`
                // Skip $this
                if part.starts_with("$this") {
                    continue;
                }

                // Check for default value
                let (part_no_default, has_default) = if let Some(eq_pos) = part.find('=') {
                    (part[..eq_pos].trim(), true)
                } else {
                    (part, false)
                };

                if let Some(dollar_pos) = part_no_default.rfind('$') {
                    let name = part_no_default[dollar_pos + 1..].trim().to_string();
                    let type_part = part_no_default[..dollar_pos].trim();
                    let param_type = if type_part.is_empty() {
                        "mixed".to_string()
                    } else {
                        type_part.to_string()
                    };
                    params.push(Param {
                        name,
                        param_type,
                        mutable: false,
                        has_default,
                    });
                }
            }
            _ => {
                // Rust/default format: `name: Type`, `&self`, `mut name: Type`
                // Skip self/receiver params
                if part == "self" || part == "&self" || part == "&mut self" || part == "mut self" {
                    continue;
                }

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
fn detect_error_propagation(
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
fn extract_propagation_call(line: &str) -> String {
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
    static CALL_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(\w+(?:::\w+)*)\s*\(").unwrap());

    let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();

    for (_line_num, text) in body_lines {
        for caps in CALL_RE.captures_iter(text) {
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
    use std::collections::HashMap;

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
            return_type_separator: "->".to_string(),
            param_format: "name_colon_type".to_string(),
            test_templates: HashMap::new(),
            type_defaults: vec![],
            ..Default::default()
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
        let params = parse_params("root: &Path, files: &[PathBuf]", "name_colon_type");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "root");
        assert_eq!(params[0].param_type, "&Path");
        assert_eq!(params[1].name, "files");
    }

    #[test]
    fn parse_params_with_self() {
        let params = parse_params("&self, key: &str", "name_colon_type");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "key");
    }

    #[test]
    fn parse_params_empty() {
        let params = parse_params("", "name_colon_type");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_params_php_format() {
        let params = parse_params("string $name, ?int $count = 0", "type_dollar_name");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "name");
        assert_eq!(params[0].param_type, "string");
        assert!(!params[0].has_default);
        assert_eq!(params[1].name, "count");
        assert_eq!(params[1].param_type, "?int");
        assert!(params[1].has_default);
    }

    #[test]
    fn parse_params_php_untyped() {
        let params = parse_params("$request, $args", "type_dollar_name");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "request");
        assert_eq!(params[0].param_type, "mixed");
        assert_eq!(params[1].name, "args");
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

    #[test]
    fn join_declaration_lines_single_line() {
        let lines = vec!["pub fn foo(x: u32) -> bool {"];
        assert_eq!(
            join_declaration_lines(&lines, 1, 1),
            "pub fn foo(x: u32) -> bool {"
        );
    }

    #[test]
    fn join_declaration_lines_multi_line_params() {
        let lines = vec![
            "pub fn complex(",
            "    root: &Path,",
            "    files: &[PathBuf],",
            "    config: &Config,",
            ") -> Result<(), Error> {",
        ];
        let decl = join_declaration_lines(&lines, 1, 5);
        assert!(decl.contains("root: &Path,"));
        assert!(decl.contains("config: &Config,"));
        assert!(decl.contains("-> Result<(), Error>"));
    }

    #[test]
    fn join_declaration_lines_return_type_on_next_line() {
        let lines = vec![
            "pub fn long_name(arg: Type)",
            "    -> Result<ValidationResult, Error>",
            "{",
        ];
        let decl = join_declaration_lines(&lines, 1, 3);
        assert!(decl.contains("-> Result<ValidationResult, Error>"));
    }

    #[test]
    fn extract_params_from_declaration_simple() {
        let decl = "pub fn foo(x: u32, y: &str) -> bool {";
        assert_eq!(
            extract_params_from_declaration(decl),
            Some("x: u32, y: &str".to_string())
        );
    }

    #[test]
    fn extract_params_from_declaration_nested_generics() {
        let decl = "pub fn bar(map: HashMap<String, Vec<u8>>, flag: bool) -> () {";
        assert_eq!(
            extract_params_from_declaration(decl),
            Some("map: HashMap<String, Vec<u8>>, flag: bool".to_string())
        );
    }

    #[test]
    fn extract_params_from_declaration_multi_line_joined() {
        let decl = "pub fn complex( root: &Path, files: &[PathBuf], config: &Config, ) -> Result<(), Error> {";
        let params = extract_params_from_declaration(decl).unwrap();
        assert!(params.contains("root: &Path"));
        assert!(params.contains("files: &[PathBuf]"));
        assert!(params.contains("config: &Config"));
    }

    #[test]
    fn extract_params_from_declaration_no_params() {
        let decl = "pub fn no_args() -> bool {";
        assert_eq!(extract_params_from_declaration(decl), None);
    }

    #[test]
    fn extract_params_from_declaration_self_receiver() {
        let decl = "pub fn method(&self, x: u32) -> bool {";
        let params = extract_params_from_declaration(decl).unwrap();
        assert!(params.contains("&self"));
        assert!(params.contains("x: u32"));
    }

    #[test]
    fn extract_propagation_call_method() {
        assert_eq!(
            extract_propagation_call("    let content = fs::read_to_string(path)?;"),
            "read_to_string"
        );
    }

    #[test]
    fn extract_propagation_call_function() {
        assert_eq!(
            extract_propagation_call("    let parsed = serde_json::from_str(&content)?;"),
            "from_str"
        );
    }

    #[test]
    fn extract_propagation_call_chained() {
        assert_eq!(
            extract_propagation_call("    config.validate()?;"),
            "validate"
        );
    }

    #[test]
    fn extract_propagation_call_no_match() {
        assert_eq!(extract_propagation_call("    let x = 42;"), "operation");
    }

    #[test]
    fn detect_error_propagation_adds_branch() {
        let body_lines = vec![
            (2, "    let content = fs::read_to_string(path)?;"),
            (3, "    let parsed = serde_json::from_str(&content)?;"),
            (4, "    Ok(parsed)"),
        ];

        let contract = ContractGrammar {
            error_propagation: vec![r"\?\s*;".to_string(), r"\?\s*$".to_string()],
            ..Default::default()
        };

        let mut branches = Vec::new();
        detect_error_propagation(&body_lines, &contract, &mut branches);

        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].returns.variant, "err");
        assert!(branches[0].condition.contains("error propagation via ?"));
        assert!(branches[0].condition.contains("read_to_string"));
        assert!(branches[0].condition.contains("from_str"));
    }

    #[test]
    fn detect_error_propagation_skips_when_explicit_err_exists() {
        let body_lines = vec![
            (2, "    let content = fs::read_to_string(path)?;"),
            (3, "    Ok(content)"),
        ];

        let contract = ContractGrammar {
            error_propagation: vec![r"\?\s*;".to_string()],
            ..Default::default()
        };

        // Pre-existing explicit Err branch
        let mut branches = vec![Branch {
            condition: "invalid input".to_string(),
            returns: ReturnValue {
                variant: "err".to_string(),
                value: None,
            },
            effects: vec![],
            line: Some(5),
        }];

        detect_error_propagation(&body_lines, &contract, &mut branches);

        // Should NOT add another err branch
        assert_eq!(branches.len(), 1);
    }
}
