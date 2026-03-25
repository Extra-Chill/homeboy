//! hashing — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use sha2::{Digest, Sha256};
use crate::extension::grammar::{self, Grammar, Symbol};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;
use std::path::Path;
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use super::extract_extends;
use super::extract_registrations;
use super::extract_namespace;
use super::extract_internal_calls;
use super::is_public_visibility;
use super::extract_dead_code_markers;
use super::extract_types;
use super::extract_properties;
use super::extract_hooks;
use super::detect_unused_params;
use super::extract_imports;
use super::extract_implements;


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

/// Compute exact body hash: normalize whitespace, SHA256, truncate to 16 hex chars.
pub(crate) fn exact_hash(body: &str) -> String {
    let normalized = normalize_whitespace(body);
    sha256_hex16(&normalized)
}

/// Compute structural hash: replace identifiers/literals with positional tokens.
pub(crate) fn structural_hash(body: &str, keywords: &[&str], is_php: bool) -> String {
    let normalized = structural_normalize(body, keywords, is_php);
    sha256_hex16(&normalized)
}

/// Normalize whitespace: collapse all runs to single space.
pub(crate) fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(ch);
            in_space = false;
        }
    }
    result.trim().to_string()
}

/// SHA256 hash, return first 16 hex characters.
pub(crate) fn sha256_hex16(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    format!("{:x}", hash)[..16].to_string()
}

/// Structural normalization: strip to body, replace strings/numbers/identifiers
/// with positional tokens, preserving language keywords as structural markers.
pub(crate) fn structural_normalize(body: &str, keywords: &[&str], is_php: bool) -> String {
    // Strip to body (from first opening brace)
    let text = if let Some(pos) = body.find('{') {
        &body[pos..]
    } else {
        body
    };

    let keyword_set: HashSet<&str> = keywords.iter().copied().collect();

    // Working string — we'll do sequential replacements
    let mut result = text.to_string();

    // Replace string literals with STR
    result = replace_string_literals(&result);

    // Replace numeric literals with NUM
    result = replace_numeric_literals(&result);

    // Replace PHP variables with positional tokens (if PHP)
    if is_php {
        result = replace_php_variables(&result);
    }

    // Replace non-keyword identifiers with positional tokens
    result = replace_identifiers(&result, &keyword_set);

    // Collapse whitespace
    normalize_whitespace(&result)
}

/// Replace string literals ("..." and '...') with STR.
pub(crate) fn replace_string_literals(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            i += 1;
            // Skip contents until matching unescaped quote
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2; // skip escaped char
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            result.push_str("STR");
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Replace numeric literals with NUM.
pub(crate) fn replace_numeric_literals(input: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b\d[\d_]*(?:\.\d[\d_]*)?\b").unwrap());
    RE.replace_all(input, "NUM").to_string()
}

/// Replace PHP $variable references with positional tokens.
pub(crate) fn replace_php_variables(input: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\$\w+").unwrap());
    let mut var_map: HashMap<String, String> = HashMap::new();
    let mut counter = 0;

    RE.replace_all(input, |caps: &regex::Captures| {
        let var = caps[0].to_string();
        if var == "$this" {
            return var;
        }
        let token = var_map.entry(var).or_insert_with(|| {
            let t = format!("VAR_{}", counter);
            counter += 1;
            t
        });
        token.clone()
    })
    .to_string()
}

/// Replace non-keyword identifiers with positional ID_N tokens.
pub(crate) fn replace_identifiers(input: &str, keywords: &HashSet<&str>) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b[a-zA-Z_]\w*\b").unwrap());
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut counter = 0;

    RE.replace_all(input, |caps: &regex::Captures| {
        let word = &caps[0];
        if keywords.contains(word) {
            return word.to_string();
        }
        // Also preserve structural tokens we inserted
        if word.starts_with("STR")
            || word.starts_with("NUM")
            || word.starts_with("CHR")
            || word.starts_with("VAR_")
            || word.starts_with("ID_")
        {
            return word.to_string();
        }
        let token = id_map.entry(word.to_string()).or_insert_with(|| {
            let t = format!("ID_{}", counter);
            counter += 1;
            t
        });
        token.clone()
    })
    .to_string()
}
