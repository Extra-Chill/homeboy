//! extract — extracted from contract_extract.rs.

use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use regex::Regex;
use super::count_early_returns;
use super::detect_branches;
use super::parse_params;
use super::join_declaration_lines;
use super::find_function_body_range;
use super::detect_return_shape;
use super::super::contract::*;


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

    // Step 2: Extract all symbols
    let all_symbols = grammar::extract(content, grammar);
    let function_symbols: Vec<_> = all_symbols
        .iter()
        .filter(|s| s.concept == "function")
        .collect();

    // Build impl block line ranges for correlating methods with their parent types.
    // Each impl block symbol has a line number and a type_name capture.
    // We correlate by depth: functions at depth > 0 whose line falls after an impl
    // block declaration belong to that impl's type.
    let impl_blocks: Vec<(usize, String)> = all_symbols
        .iter()
        .filter(|s| s.concept == "impl_block")
        .filter_map(|s| {
            let type_name = s.get("type_name")?.to_string();
            Some((s.line, type_name))
        })
        .collect();

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
        let visibility = sym.get("visibility").map(|v| v.trim());
        let is_public = visibility.is_some_and(|v| v.starts_with("pub"));

        // Build the full declaration text by joining lines from the fn keyword
        // through the opening brace. This handles multi-line signatures where
        // params and/or return type span multiple lines. (#818)
        let full_decl = join_declaration_lines(&raw_lines, fn_line, body_start);

        // Detect return type from the FULL declaration (not just the fn line)
        let return_type = detect_return_shape(&full_decl, contract_grammar);

        // Extract params: prefer the full declaration's param list over the
        // grammar symbol's single-line capture, which truncates multi-line params.
        let full_params = extract_params_from_declaration(&full_decl);
        let params_str_owned;
        let params_str = if let Some(ref fp) = full_params {
            fp.as_str()
        } else {
            params_str_owned = sym.get("params").unwrap_or("").to_string();
            &params_str_owned
        };

        // Parse params
        let params = parse_params(params_str, &contract_grammar.param_format);

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

        // Detect async from the full declaration
        let is_async = full_decl.contains("async ");

        // Determine the impl type for ALL functions inside impl blocks (depth > 0).
        // This covers both methods (&self) and associated functions (Type::new()).
        // Find the nearest impl_block that starts before this function's line.
        let impl_type = if fn_depth > 0 {
            impl_blocks
                .iter()
                .rev()
                .find(|(impl_line, _)| *impl_line < fn_line)
                .map(|(_, type_name)| type_name.clone())
        } else {
            None
        };

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
            impl_type,
        });
    }

    Some(contracts)
}

/// Extract the full parameter list from a joined declaration string.
///
/// Finds the balanced parenthesized parameter list, handling nested generics
/// like `HashMap<String, Vec<u8>>` correctly.
pub(crate) fn extract_params_from_declaration(decl: &str) -> Option<String> {
    // Find the opening paren after "fn name"
    let open = decl.find('(')?;
    let bytes = decl.as_bytes();
    let mut depth = 0i32;
    let mut close = None;

    for (i, &b) in bytes[open..].iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }

    let close = close?;
    // Return the content between parens (exclusive)
    let params = &decl[open + 1..close];
    let trimmed = params.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
