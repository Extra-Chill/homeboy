//! function_extraction — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;
use sha2::{Digest, Sha256};
use super::ImplContext;
use super::structural_hash;
use super::extract_internal_calls;
use super::exact_hash;
use super::extract_implements;
use super::extract_types;
use super::detect_unused_params;
use super::extract_properties;
use super::extract_imports;
use super::is_public_visibility;
use super::extract_dead_code_markers;
use super::extract_functions;
use super::extract_extends;
use super::extract_namespace;
use super::extract_registrations;
use super::find_matching_brace;
use super::extract_hooks;


/// Generate a FileFingerprint from source content using a grammar.
///
/// This is the core replacement for extension fingerprint scripts.
/// Returns None if the grammar doesn't support the minimum required patterns.
pub fn fingerprint_from_grammar(
    content: &str,
    grammar: &Grammar,
    relative_path: &str,
) -> Option<FileFingerprint> {
    // Must have at least a function pattern
    if !grammar.patterns.contains_key("function") && !grammar.patterns.contains_key("method") {
        return None;
    }

    let lang_id = grammar.language.id.as_str();
    let language = Language::from_extension(
        grammar
            .language
            .extensions
            .first()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    // Extract all symbols using the grammar engine
    let symbols = grammar::extract(content, grammar);
    let lines: Vec<&str> = content.lines().collect();

    // Find test module range (for Rust: #[cfg(test)] mod tests { ... })
    let test_range = find_test_range(&symbols, &lines, grammar);

    // Build impl block context map: line → (type_name, trait_name)
    let impl_contexts = build_impl_contexts(&symbols);

    // Extract functions with full context
    let functions = extract_functions(&symbols, &lines, &impl_contexts, test_range, grammar);

    // --- Methods list ---
    let mut methods = Vec::new();
    let mut seen_methods = HashSet::new();
    for f in &functions {
        if f.is_test {
            continue;
        }
        if !seen_methods.contains(&f.name) {
            methods.push(f.name.clone());
            seen_methods.insert(f.name.clone());
        }
    }
    // Add test methods with test_ prefix
    for f in &functions {
        if f.is_test {
            let prefixed = if f.name.starts_with("test_") {
                f.name.clone()
            } else {
                format!("test_{}", f.name)
            };
            if !seen_methods.contains(&prefixed) {
                methods.push(prefixed.clone());
                seen_methods.insert(prefixed);
            }
        }
    }

    // --- Method hashes and structural hashes ---
    let keywords = match lang_id {
        "rust" => RUST_KEYWORDS,
        "php" => PHP_KEYWORDS,
        _ => RUST_KEYWORDS, // fallback
    };

    let mut method_hashes = HashMap::new();
    let mut structural_hashes = HashMap::new();
    for f in &functions {
        if f.is_test || f.body.is_empty() {
            continue;
        }
        // Skip trait impl methods — they MUST exist per-type and cannot be
        // deduplicated, so including them produces false positive findings.
        if f.is_trait_impl {
            continue;
        }
        let exact = exact_hash(&f.body);
        method_hashes.insert(f.name.clone(), exact);
        let structural = structural_hash(&f.body, keywords, lang_id == "php");
        structural_hashes.insert(f.name.clone(), structural);
    }

    // --- Visibility ---
    let mut visibility = HashMap::new();
    for f in &functions {
        if f.is_test {
            continue;
        }
        visibility.insert(f.name.clone(), f.visibility.clone());
    }

    // --- Type names ---
    let (type_name, type_names) = extract_types(&symbols);

    // --- Extends ---
    let extends = extract_extends(&symbols);

    // --- Implements ---
    let implements = extract_implements(&symbols);

    // --- Namespace ---
    let namespace = extract_namespace(&symbols, relative_path, lang_id);

    // --- Imports ---
    let imports = extract_imports(&symbols);

    // --- Registrations ---
    let registrations = extract_registrations(&symbols);

    // --- Internal calls ---
    let skip_calls: &[&str] = match lang_id {
        "rust" => SKIP_CALLS_RUST,
        "php" => SKIP_CALLS_PHP,
        _ => SKIP_CALLS_RUST,
    };
    // Build the effective skip list: exclude names that are also defined as
    // functions in this file. E.g. "write" is in SKIP_CALLS (for the write!
    // macro) but if this file defines `fn write(...)`, we need to track calls
    // to it in internal_calls.
    let defined_names: HashSet<&str> = functions.iter().map(|f| f.name.as_str()).collect();
    let effective_skip: Vec<&str> = skip_calls
        .iter()
        .filter(|name| !defined_names.contains(*name))
        .copied()
        .collect();
    let internal_calls = extract_internal_calls(content, &effective_skip);

    // --- Public API ---
    let public_api: Vec<String> = functions
        .iter()
        .filter(|f| !f.is_test && is_public_visibility(&f.visibility))
        .map(|f| f.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // --- Trait impl methods ---
    let trait_impl_methods: Vec<String> = functions
        .iter()
        .filter(|f| f.is_trait_impl && !f.is_test)
        .map(|f| f.name.clone())
        .collect();

    // --- Unused parameters ---
    let unused_parameters = detect_unused_params(&functions, lang_id);

    // --- Dead code markers ---
    let dead_code_markers = extract_dead_code_markers(&symbols, &lines);

    // --- Properties (PHP-specific, from grammar) ---
    let properties = extract_properties(&symbols);

    // --- Hooks (PHP-specific, from grammar) ---
    let hooks = extract_hooks(&symbols);

    Some(FileFingerprint {
        relative_path: relative_path.to_string(),
        language,
        methods,
        registrations,
        type_name,
        type_names,
        extends,
        implements,
        namespace,
        imports,
        content: content.to_string(),
        method_hashes,
        structural_hashes,
        visibility,
        properties,
        hooks,
        unused_parameters,
        dead_code_markers,
        internal_calls,
        call_sites: Vec::new(), // Core grammar engine doesn't extract call sites yet
        public_api,
        trait_impl_methods,
    })
}

/// Try to load a grammar for a file extension.
///
/// Searches installed extensions for a grammar.toml that handles the given
/// file extension.
pub fn load_grammar_for_ext(ext: &str) -> Option<Grammar> {
    let matched = extension::find_extension_for_file_ext(ext, "fingerprint")?;
    let extension_path = matched.extension_path.as_deref()?;

    // Try grammar.toml first, then grammar.json
    let grammar_path = Path::new(extension_path).join("grammar.toml");
    if grammar_path.exists() {
        return grammar::load_grammar(&grammar_path).ok();
    }

    let grammar_json_path = Path::new(extension_path).join("grammar.json");
    if grammar_json_path.exists() {
        return grammar::load_grammar_json(&grammar_json_path).ok();
    }

    None
}

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
