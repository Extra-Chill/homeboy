//! function_extraction — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use crate::extension::grammar::{self, Grammar, Symbol};
use std::path::Path;
use sha2::{Digest, Sha256};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;
use super::ImplContext;
use super::FunctionInfo;


/// Build a map of line ranges → impl context.
///
/// For each impl_block symbol, we record the type name and optional trait name.
/// Functions inside these ranges inherit the context.
pub(crate) fn build_impl_contexts(symbols: &[Symbol]) -> Vec<ImplContext> {
    symbols
        .iter()
        .filter(|s| s.concept == "impl_block")
        .map(|s| {
            let type_name = s.get("type_name").unwrap_or("").to_string();
            let trait_name = s.get("trait_name").map(|t| t.to_string());
            ImplContext {
                line: s.line,
                depth: s.depth,
                _type_name: type_name,
                trait_name,
            }
        })
        .collect()
}

/// Find the line range of the test module (if any).
///
/// For Rust: looks for #[cfg(test)] followed by mod tests { ... }.
/// Returns (start_line_0indexed, end_line_0indexed).
pub(crate) fn find_test_range(
    symbols: &[Symbol],
    lines: &[&str],
    grammar: &Grammar,
) -> Option<(usize, usize)> {
    // Look for cfg_test attribute followed by mod declaration
    let cfg_tests: Vec<usize> = symbols
        .iter()
        .filter(|s| s.concept == "cfg_test" || s.concept == "test_attribute")
        .filter(|s| s.concept == "cfg_test")
        .map(|s| s.line)
        .collect();

    for cfg_line in cfg_tests {
        // Look for the mod declaration within the next few lines
        let start_idx = cfg_line.saturating_sub(1); // 0-indexed
        for i in start_idx..std::cmp::min(start_idx + 5, lines.len()) {
            if lines[i].trim().contains("mod ") && lines[i].contains('{') {
                // Found the test module — find its end
                let end = find_matching_brace(lines, i, grammar);
                return Some((start_idx, end));
            }
        }
    }

    None
}

/// Find the matching closing brace for a block starting at `start_line`.
pub(crate) fn find_matching_brace(lines: &[&str], start_line: usize, _grammar: &Grammar) -> usize {
    let mut depth: i32 = 0;
    let mut found_open = false;

    for i in start_line..lines.len() {
        for ch in lines[i].chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if found_open && depth == 0 {
            return i;
        }
    }

    lines.len().saturating_sub(1)
}

/// Determine if a function symbol is inside a test module.
pub(crate) fn is_in_test_range(line: usize, test_range: Option<(usize, usize)>) -> bool {
    if let Some((start, end)) = test_range {
        let idx = line.saturating_sub(1);
        idx >= start && idx <= end
    } else {
        false
    }
}

/// Extract all functions from the grammar symbols with full context.
pub(crate) fn extract_functions(
    symbols: &[Symbol],
    lines: &[&str],
    impl_contexts: &[ImplContext],
    test_range: Option<(usize, usize)>,
    grammar: &Grammar,
) -> Vec<FunctionInfo> {
    let fn_concepts = ["function", "method", "free_function"];
    let test_attr_lines: HashSet<usize> = symbols
        .iter()
        .filter(|s| s.concept == "test_attribute")
        .map(|s| s.line)
        .collect();

    let mut functions = Vec::new();

    for symbol in symbols
        .iter()
        .filter(|s| fn_concepts.contains(&s.concept.as_str()))
    {
        let name = match symbol.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip "tests" pseudo-function
        if name == "tests" {
            continue;
        }

        // Determine if this is a test function
        let has_test_attr = (1..=3).any(|offset| {
            symbol.line >= offset && test_attr_lines.contains(&(symbol.line - offset))
        });
        let in_test_mod = is_in_test_range(symbol.line, test_range);
        let is_test = has_test_attr || in_test_mod;

        // Determine if inside a trait impl by finding the nearest enclosing
        // impl context (the last one that starts before this function at a
        // shallower depth). Using `any()` was wrong — it matched unrelated
        // impl blocks earlier in the file.
        let is_trait_impl = if symbol.depth > 0 {
            impl_contexts
                .iter()
                .rfind(|ctx| ctx.depth < symbol.depth && ctx.line < symbol.line)
                .is_some_and(|ctx| ctx.trait_name.as_ref().is_some_and(|t| !t.is_empty()))
        } else {
            false
        };

        // Extract visibility
        let visibility = extract_fn_visibility(symbol);

        // Extract params
        let params = symbol.get("params").unwrap_or("").to_string();

        // Extract function body
        let body = extract_fn_body(lines, symbol.line.saturating_sub(1), grammar);

        functions.push(FunctionInfo {
            name,
            body,
            visibility,
            is_test,
            is_trait_impl,
            params,
            _start_line: symbol.line,
        });
    }

    functions
}

/// Extract function visibility from its symbol.
pub(crate) fn extract_fn_visibility(symbol: &Symbol) -> String {
    if let Some(vis) = symbol.visibility() {
        let vis = vis.trim();
        if vis.contains("pub(crate)") {
            "pub(crate)".to_string()
        } else if vis.contains("pub(super)") {
            "pub(super)".to_string()
        } else if vis.contains("pub") {
            "public".to_string()
        } else {
            "private".to_string()
        }
    } else if let Some(mods) = symbol.get("modifiers") {
        // PHP-style: modifiers capture with public/protected/private
        let mods = mods.trim();
        if mods.contains("private") {
            "private".to_string()
        } else if mods.contains("protected") {
            "protected".to_string()
        } else {
            "public".to_string()
        }
    } else {
        "private".to_string()
    }
}

/// Extract a function body from source lines, starting at the declaration line.
///
/// Finds the opening brace and tracks depth to the matching close.
pub(crate) fn extract_fn_body(lines: &[&str], start_idx: usize, _grammar: &Grammar) -> String {
    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut body_lines = Vec::new();

    for i in start_idx..lines.len() {
        let trimmed = lines[i].trim();

        // Trait method declarations end with `;` and have no body.
        // If we hit a semicolon before finding any `{`, this is a bodyless declaration.
        if !found_open && trimmed.ends_with(';') {
            return String::new();
        }

        for ch in lines[i].chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        body_lines.push(lines[i]);
        if found_open && depth == 0 {
            break;
        }
    }

    body_lines.join(" ")
}
