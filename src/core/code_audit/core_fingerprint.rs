//! Grammar-driven core fingerprint engine.
//!
//! Replaces the per-language Python fingerprint scripts with a single Rust
//! implementation that uses the grammar engine (`utils/grammar.rs`) for
//! structural parsing. Extensions only need to ship a `grammar.toml` —
//! no more Python-in-bash fingerprint scripts.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs           (structural parsing, brace tracking)
//!     ↓
//! core_fingerprint.rs        (this file: hashing, method extraction, visibility)
//!     ↓
//! FileFingerprint            (consumed by duplication, conventions, dead_code, etc.)
//! ```
//!
//! # What this handles (generic across languages)
//!
//! - Method/function extraction with deduplication
//! - Body extraction and exact/structural hashing
//! - Visibility extraction from grammar captures
//! - Type name and type_names extraction
//! - Import/namespace extraction
//! - Internal calls extraction
//! - Public API collection
//! - Unused parameter detection
//! - Dead code marker detection
//! - Impl context tracking (trait impl methods excluded from dedup hashes)
//!
//! # What extensions configure via grammar.toml
//!
//! - Language-specific patterns (function, class, impl_block, etc.)
//! - Comment and string syntax
//! - Block delimiters

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};

use super::conventions::Language;
use super::fingerprint::FileFingerprint;

// ============================================================================
// Configuration
// ============================================================================

/// Keywords preserved during structural normalization (not replaced with ID_N).
/// These are language-specific; we detect the language from the grammar ID.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield",
    // Common types kept as structural markers
    "Some", "None", "Ok", "Err", "Result", "Option", "Vec", "String", "Box", "Arc", "Rc", "HashMap",
    "HashSet", "bool", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64", "str", "char",
];

const PHP_KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "enddeclare",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "eval",
    "exit",
    "extends",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "goto",
    "if",
    "implements",
    "include",
    "include_once",
    "instanceof",
    "insteadof",
    "interface",
    "isset",
    "list",
    "match",
    "namespace",
    "new",
    "or",
    "print",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "require_once",
    "return",
    "static",
    "switch",
    "throw",
    "trait",
    "try",
    "unset",
    "use",
    "var",
    "while",
    "xor",
    "yield",
    "null",
    "true",
    "false",
    "self",
    "parent",
    // Common types
    "int",
    "float",
    "string",
    "bool",
    "void",
    "mixed",
    "object",
    "iterable",
    "never",
];

/// Generic names too common to flag as near-duplicates.
/// These are the same as in duplication.rs — kept here for internal_calls filtering.
const SKIP_CALLS_RUST: &[&str] = &[
    "if",
    "while",
    "for",
    "match",
    "loop",
    "return",
    "Some",
    "None",
    "Ok",
    "Err",
    "Box",
    "Vec",
    "Arc",
    "Rc",
    "String",
    "println",
    "eprintln",
    "format",
    "write",
    "writeln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "todo",
    "unimplemented",
    "unreachable",
    "dbg",
    "cfg",
    "include",
    "include_str",
    "concat",
    "env",
    "compile_error",
    "stringify",
    "vec",
    "hashmap",
    "bail",
    "ensure",
    "anyhow",
    "matches",
    "debug_assert",
    "debug_assert_eq",
    "allow",
    "deny",
    "warn",
    "derive",
    "serde",
    "test",
    "inline",
    "must_use",
    "doc",
    "feature",
    "pub",
    "crate",
    "super",
];

const SKIP_CALLS_PHP: &[&str] = &[
    "if",
    "while",
    "for",
    "foreach",
    "switch",
    "match",
    "catch",
    "return",
    "echo",
    "print",
    "isset",
    "unset",
    "empty",
    "list",
    "array",
    "function",
    "class",
    "interface",
    "trait",
    "new",
    "require",
    "require_once",
    "include",
    "include_once",
    "define",
    "defined",
    "die",
    "exit",
    "eval",
    "compact",
    "extract",
    "var_dump",
    "print_r",
    "var_export",
];

// ============================================================================
// Public API
// ============================================================================

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
    // Add test methods with test_ prefix.
    //
    // Only include functions that have an explicit #[test] attribute.
    // Functions inside #[cfg(test)] modules without #[test] are helpers
    // (factories, fixtures, grammar builders) — not actual tests. Including
    // them causes the orphaned test detector to flag them when no matching
    // source method exists.
    for f in &functions {
        if f.is_test && f.has_test_attr {
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

// ============================================================================
// Function extraction
// ============================================================================

/// A function extracted from source with full context.
struct FunctionInfo {
    name: String,
    body: String,
    visibility: String,
    is_test: bool,
    /// Whether the function has an explicit `#[test]` attribute (vs. just
    /// being inside a `#[cfg(test)]` module). Helpers inside test modules
    /// that happen to start with `test_` are NOT actual tests.
    has_test_attr: bool,
    is_trait_impl: bool,
    params: String,
    _start_line: usize,
}

/// Build a map of line ranges → impl context.
///
/// For each impl_block symbol, we record the type name and optional trait name.
/// Functions inside these ranges inherit the context.
fn build_impl_contexts(symbols: &[Symbol]) -> Vec<ImplContext> {
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

struct ImplContext {
    line: usize,
    depth: i32,
    _type_name: String,
    trait_name: Option<String>,
}

/// Find the line range of the test module (if any).
///
/// For Rust: looks for #[cfg(test)] followed by mod tests { ... }.
/// Returns (start_line_0indexed, end_line_0indexed).
fn find_test_range(
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
fn find_matching_brace(lines: &[&str], start_line: usize, _grammar: &Grammar) -> usize {
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
fn is_in_test_range(line: usize, test_range: Option<(usize, usize)>) -> bool {
    if let Some((start, end)) = test_range {
        let idx = line.saturating_sub(1);
        idx >= start && idx <= end
    } else {
        false
    }
}

/// Extract all functions from the grammar symbols with full context.
fn extract_functions(
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
            has_test_attr,
            is_trait_impl,
            params,
            _start_line: symbol.line,
        });
    }

    functions
}

/// Extract function visibility from its symbol.
fn extract_fn_visibility(symbol: &Symbol) -> String {
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
fn extract_fn_body(lines: &[&str], start_idx: usize, _grammar: &Grammar) -> String {
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

// ============================================================================
// Hashing
// ============================================================================

/// Compute exact body hash: normalize whitespace, SHA256, truncate to 16 hex chars.
fn exact_hash(body: &str) -> String {
    let normalized = normalize_whitespace(body);
    sha256_hex16(&normalized)
}

/// Compute structural hash: replace identifiers/literals with positional tokens.
fn structural_hash(body: &str, keywords: &[&str], is_php: bool) -> String {
    let normalized = structural_normalize(body, keywords, is_php);
    sha256_hex16(&normalized)
}

/// Normalize whitespace: collapse all runs to single space.
fn normalize_whitespace(s: &str) -> String {
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
fn sha256_hex16(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    format!("{:x}", hash)[..16].to_string()
}

/// Structural normalization: strip to body, replace strings/numbers/identifiers
/// with positional tokens, preserving language keywords as structural markers.
fn structural_normalize(body: &str, keywords: &[&str], is_php: bool) -> String {
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
fn replace_string_literals(input: &str) -> String {
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
fn replace_numeric_literals(input: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b\d[\d_]*(?:\.\d[\d_]*)?\b").unwrap());
    RE.replace_all(input, "NUM").to_string()
}

/// Replace PHP $variable references with positional tokens.
fn replace_php_variables(input: &str) -> String {
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
fn replace_identifiers(input: &str, keywords: &HashSet<&str>) -> String {
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

// ============================================================================
// Symbol extraction helpers
// ============================================================================

/// Extract type_name and type_names from struct/class symbols.
fn extract_types(symbols: &[Symbol]) -> (Option<String>, Vec<String>) {
    let mut type_names = Vec::new();
    let mut primary_type = None;

    for s in symbols {
        if s.concept == "struct" || s.concept == "class" {
            if let Some(name) = s.name() {
                type_names.push(name.to_string());
                // First public type is the primary
                if primary_type.is_none() {
                    let vis = s.visibility().unwrap_or("");
                    if vis.contains("pub") || vis.contains("public") || vis.is_empty() {
                        primary_type = Some(name.to_string());
                    }
                }
            }
        }
    }

    // If no public type, use the first type found
    if primary_type.is_none() && !type_names.is_empty() {
        primary_type = Some(type_names[0].clone());
    }

    (primary_type, type_names)
}

/// Extract extends (parent class) from symbols.
fn extract_extends(symbols: &[Symbol]) -> Option<String> {
    symbols
        .iter()
        .filter(|s| s.concept == "class" || s.concept == "struct")
        .find_map(|s| {
            s.get("extends").map(|e| {
                // PHP: take last segment of backslash-separated name
                e.split('\\').next_back().unwrap_or(e).to_string()
            })
        })
}

/// Extract implements (traits/interfaces) from symbols.
fn extract_implements(symbols: &[Symbol]) -> Vec<String> {
    let mut implements = Vec::new();
    let mut seen = HashSet::new();

    // From impl_block symbols (Rust: impl Trait for Type)
    for s in symbols.iter().filter(|s| s.concept == "impl_block") {
        if let Some(trait_name) = s.get("trait_name") {
            if !trait_name.is_empty() && seen.insert(trait_name.to_string()) {
                // Take last segment for qualified names
                let short = trait_name.split("::").last().unwrap_or(trait_name);
                implements.push(short.to_string());
            }
        }
    }

    // From implements pattern (PHP)
    for s in symbols.iter().filter(|s| s.concept == "implements") {
        if let Some(interfaces) = s.get("interfaces") {
            for iface in interfaces.split(',') {
                let iface = iface.trim();
                if !iface.is_empty() {
                    let short = iface.split('\\').next_back().unwrap_or(iface);
                    if seen.insert(short.to_string()) {
                        implements.push(short.to_string());
                    }
                }
            }
        }
    }

    // From trait_use pattern (PHP: use SomeTrait;)
    for s in symbols.iter().filter(|s| s.concept == "trait_use") {
        if let Some(name) = s.name() {
            let short = name.split('\\').next_back().unwrap_or(name);
            if seen.insert(short.to_string()) {
                implements.push(short.to_string());
            }
        }
    }

    implements
}

/// Extract namespace from symbols or derive from path.
fn extract_namespace(symbols: &[Symbol], relative_path: &str, lang_id: &str) -> Option<String> {
    // Direct namespace symbol (PHP: namespace DataMachine\Abilities;)
    for s in symbols.iter().filter(|s| s.concept == "namespace") {
        if let Some(name) = s.name() {
            return Some(name.to_string());
        }
    }

    // Rust: derive from crate:: imports or file path
    if lang_id == "rust" {
        // Count crate:: use paths
        let mut module_counts: HashMap<&str, usize> = HashMap::new();
        for s in symbols.iter().filter(|s| s.concept == "import") {
            if let Some(path) = s.get("path") {
                if let Some(rest) = path.strip_prefix("crate::") {
                    if let Some(module) = rest.split("::").next() {
                        *module_counts.entry(module).or_insert(0) += 1;
                    }
                }
            }
        }
        if let Some((most_common, _)) = module_counts.iter().max_by_key(|(_, count)| *count) {
            return Some(format!("crate::{}", most_common));
        }

        // Fall back to file path
        let parts: Vec<&str> = relative_path.trim_end_matches(".rs").split('/').collect();
        if parts.len() > 2 {
            let ns = parts[1..parts.len() - 1].join("::");
            return Some(format!("crate::{}", ns));
        } else if parts.len() == 2 {
            return Some(format!("crate::{}", parts.last().unwrap_or(&"")));
        }
    }

    None
}

/// Extract imports from symbols.
fn extract_imports(symbols: &[Symbol]) -> Vec<String> {
    let mut imports = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols.iter().filter(|s| s.concept == "import") {
        if let Some(path) = s.get("path") {
            if seen.insert(path.to_string()) {
                imports.push(path.to_string());
            }
        }
    }

    imports
}

/// Extract registrations from grammar symbols.
///
/// Matches registration-like concepts: register_post_type, add_action,
/// macro invocations, etc.
fn extract_registrations(symbols: &[Symbol]) -> Vec<String> {
    let registration_concepts = [
        "register_post_type",
        "register_taxonomy",
        "register_rest_route",
        "register_block_type",
        "add_action",
        "add_filter",
        "add_shortcode",
        "wp_cli_command",
        "wp_register_ability",
        "macro_invocation",
    ];

    let skip_macros: HashSet<&str> = [
        "println",
        "eprintln",
        "format",
        "vec",
        "assert",
        "assert_eq",
        "assert_ne",
        "panic",
        "todo",
        "unimplemented",
        "cfg",
        "derive",
        "include",
        "include_str",
        "include_bytes",
        "concat",
        "stringify",
        "env",
        "option_env",
        "compile_error",
        "write",
        "writeln",
        "matches",
        "dbg",
        "debug_assert",
        "debug_assert_eq",
        "debug_assert_ne",
        "unreachable",
        "cfg_if",
        "lazy_static",
        "thread_local",
        "once_cell",
        "macro_rules",
        "serde_json",
        "if_chain",
        "bail",
        "anyhow",
        "ensure",
        "Ok",
        "Err",
        "Some",
        "None",
        "Box",
        "Arc",
        "Rc",
        "RefCell",
        "Mutex",
        "map",
        "hashmap",
        "btreemap",
        "hashset",
    ]
    .iter()
    .copied()
    .collect();

    let mut registrations = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols
        .iter()
        .filter(|s| registration_concepts.contains(&s.concept.as_str()))
    {
        if let Some(name) = s.name() {
            // Skip common macros for Rust
            if s.concept == "macro_invocation" && skip_macros.contains(name) {
                continue;
            }
            if s.concept == "macro_invocation" && name.starts_with("test") {
                continue;
            }
            if seen.insert(name.to_string()) {
                registrations.push(name.to_string());
            }
        }
    }

    registrations
}

/// Extract internal function calls from content.
fn extract_internal_calls(content: &str, skip_calls: &[&str]) -> Vec<String> {
    let skip_set: HashSet<&str> = skip_calls.iter().copied().collect();
    let mut calls = HashSet::new();

    // Match function_name( patterns
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b(\w+)\s*\(").unwrap());
    for caps in RE.captures_iter(content) {
        let name = &caps[1];
        if !skip_set.contains(name) && !name.starts_with("test_") {
            calls.insert(name.to_string());
        }
    }

    // Match .method( and ::method( patterns
    static METHOD_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[.:](\w+)\s*\(").unwrap());
    for caps in METHOD_RE.captures_iter(content) {
        let name = &caps[1];
        if !skip_set.contains(name) && !name.starts_with("test_") {
            calls.insert(name.to_string());
        }
    }

    let mut result: Vec<String> = calls.into_iter().collect();
    result.sort();
    result
}

/// Returns true only for truly public visibility — external API.
///
/// "pub(crate)" and "pub(super)" are crate-internal and should NOT
/// appear in `public_api`. Only bare "pub" (mapped to "public" by
/// `extract_fn_visibility`) is external.
fn is_public_visibility(vis: &str) -> bool {
    vis == "public"
}

// ============================================================================
// Unused parameter detection
// ============================================================================

/// Detect function parameters that are declared but never used in the body.
fn detect_unused_params(functions: &[FunctionInfo], _lang_id: &str) -> Vec<UnusedParam> {
    let mut unused = Vec::new();

    for f in functions {
        if f.is_test || f.is_trait_impl || f.params.is_empty() || f.body.is_empty() {
            continue;
        }

        // Skip contract methods entirely. These have a fixed signature imposed
        // by a framework/interface (WordPress abilities, REST callbacks,
        // common PHP magic methods) and the parameters cannot be removed
        // even when unused. Flagging them produces churny CI noise (#1136).
        if is_contract_method_by_name(&f.name) {
            continue;
        }

        // Parse parameter names with their (optional) type hints
        let params = parse_params(&f.params);

        // Extract body-only text (after first opening brace)
        let body_after_brace = if let Some(pos) = f.body.find('{') {
            &f.body[pos + 1..]
        } else {
            continue;
        };

        for (idx, p) in params.iter().enumerate() {
            let pname = &p.name;

            // Skip self, mut, underscore-prefixed
            if pname == "self" || pname == "mut" || pname == "Self" || pname.starts_with('_') {
                continue;
            }

            // Skip params whose type hint is a known framework contract type.
            // e.g. \WP_REST_Request, WP_REST_Request, WP_Post, WP_User, etc.
            // The parameter exists to satisfy the framework callback signature,
            // not because the function must use it (#1136).
            if let Some(type_hint) = &p.type_hint {
                if is_contract_type_hint(type_hint) {
                    continue;
                }
            }

            // Check if the parameter name appears as a word in the body
            let pattern = format!(r"\b{}\b", regex::escape(pname));
            if let Ok(re) = regex::Regex::new(&pattern) {
                if !re.is_match(body_after_brace) {
                    unused.push(UnusedParam {
                        function: f.name.clone(),
                        param: pname.clone(),
                        position: idx,
                    });
                }
            }
        }
    }

    unused
}

/// A parameter with its (optional) type hint and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Param {
    name: String,
    /// Type hint as it appeared in source, if any. For PHP, leading backslashes
    /// and nullable markers are preserved (e.g. `\WP_REST_Request`, `?WP_Post`).
    /// For Rust, this is the type after the colon (e.g. `&str`).
    type_hint: Option<String>,
}

/// Parse parameters from a params string into (name, type_hint) pairs.
///
/// Supports both Rust (`name: Type`) and PHP (`Type $name`) signatures.
fn parse_params(params: &str) -> Vec<Param> {
    let mut out = Vec::new();
    for chunk in params.split(',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if chunk.contains(':') {
            // Rust-style: "name: Type" or "mut name: Type" or "&self"
            let mut parts = chunk.splitn(2, ':');
            let before_colon = parts.next().unwrap_or("").trim();
            let after_colon = parts.next().unwrap_or("").trim();
            let name = before_colon.trim_start_matches("mut").trim();
            if name.is_empty() || name == "&self" || name == "self" {
                continue;
            }
            let name = name.trim_start_matches('&');
            if name.is_empty() {
                continue;
            }
            let type_hint = if after_colon.is_empty() {
                None
            } else {
                Some(after_colon.to_string())
            };
            out.push(Param {
                name: name.to_string(),
                type_hint,
            });
        } else if chunk.contains('$') {
            // PHP-style: "TypeHint $name" or "$name" or "array $input" or "?\WP_Post $post"
            static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^([?]?[\\\w|&]+)?\s*\$(\w+)").unwrap()
            });
            if let Some(caps) = RE.captures(chunk) {
                let type_hint = caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .filter(|s| !s.is_empty());
                let name = caps[2].to_string();
                out.push(Param { name, type_hint });
            }
        }
    }
    out
}

/// Parse parameter names from a params string.
///
/// Retained as a thin wrapper over [`parse_params`] for tests and callers
/// that only care about names.
#[cfg(test)]
fn parse_param_names(params: &str) -> Vec<String> {
    parse_params(params).into_iter().map(|p| p.name).collect()
}

/// Whether a method name corresponds to a framework/contract callback where
/// the parameter list is imposed by the contract and cannot be adjusted.
///
/// Covers:
/// - WordPress Abilities API: `execute`, `checkPermission`
/// - REST controller callbacks: `register_routes`, `permission_callback_*`
/// - PHP magic methods: `__construct`, `__get`, `__call`, etc.
///
/// This is intentionally a small, conservative list — we only match on
/// names that are almost universally contract-driven. Specific type-hint
/// checks (see `is_contract_type_hint`) handle the long tail.
fn is_contract_method_by_name(name: &str) -> bool {
    matches!(
        name,
        // WordPress Abilities API (WP_Ability contract)
        "execute"
        | "checkPermission"
        | "check_permission"
        // PHP magic methods — signatures are fixed by PHP itself
        | "__construct"
        | "__destruct"
        | "__get"
        | "__set"
        | "__isset"
        | "__unset"
        | "__call"
        | "__callStatic"
        | "__toString"
        | "__invoke"
        | "__clone"
        | "__sleep"
        | "__wakeup"
        | "__serialize"
        | "__unserialize"
        | "__set_state"
        | "__debugInfo"
    )
}

/// Whether a PHP type hint names a framework contract type whose presence
/// in a parameter list indicates the signature is callback-shaped.
///
/// When a parameter's type hint matches one of these, the parameter exists
/// to satisfy a framework callback contract (e.g. WordPress hook callback,
/// REST route callback) and cannot be removed even when unused.
///
/// Handles leading `\` and nullable `?` markers. Matches on the *terminal*
/// class name only so namespaced references like `\MyPlugin\WP_REST_Request`
/// are still caught.
fn is_contract_type_hint(type_hint: &str) -> bool {
    // Strip nullable marker and leading backslashes
    let hint = type_hint.trim_start_matches('?').trim_start_matches('\\');
    // Split on union/intersection markers and check each alternative
    for alt in hint.split(['|', '&']) {
        let alt = alt.trim().trim_start_matches('\\');
        // Extract terminal class name (last backslash-separated segment)
        let terminal = alt.rsplit('\\').next().unwrap_or(alt);
        if is_contract_class_name(terminal) {
            return true;
        }
    }
    false
}

/// Whether a bare class name refers to a WordPress/PHP framework type that
/// commonly appears in callback signatures.
fn is_contract_class_name(name: &str) -> bool {
    matches!(
        name,
        // REST API
        "WP_REST_Request"
        | "WP_REST_Response"
        | "WP_REST_Server"
        // Core models
        | "WP_Post"
        | "WP_User"
        | "WP_Term"
        | "WP_Comment"
        | "WP_Site"
        | "WP_Network"
        | "WP_Query"
        | "WP_Block"
        | "WP_Block_Type"
        // Errors
        | "WP_Error"
        // HTTP
        | "WP_Http_Response"
        | "WP_HTTP_Requests_Response"
        // CLI / admin
        | "WP_CLI_Command"
        | "WP_List_Table"
    )
}

// ============================================================================
// Dead code markers
// ============================================================================

/// Extract dead code suppression markers.
fn extract_dead_code_markers(symbols: &[Symbol], lines: &[&str]) -> Vec<DeadCodeMarker> {
    let mut markers = Vec::new();

    // Look for dead_code_marker pattern matches
    for s in symbols.iter().filter(|s| s.concept == "dead_code_marker") {
        // Find the next declaration item within 5 lines
        let start_line = s.line; // 1-indexed
        for offset in 0..5 {
            let check_idx = start_line + offset; // 1-indexed, looking at lines after
            if check_idx > lines.len() {
                break;
            }
            let line = lines[check_idx - 1].trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            // Try to find a declaration
            let item_re = regex::Regex::new(
                r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:static\s+)?(?:fn|struct|enum|type|trait|const|static|mod)\s+(\w+)",
            )
            .unwrap();
            if let Some(caps) = item_re.captures(line) {
                markers.push(DeadCodeMarker {
                    item: caps[1].to_string(),
                    line: s.line,
                    marker_type: "allow_dead_code".to_string(),
                });
            }
            break;
        }
    }

    markers
}

// ============================================================================
// PHP-specific extraction from grammar symbols
// ============================================================================

/// Extract PHP class properties from property symbols.
fn extract_properties(symbols: &[Symbol]) -> Vec<String> {
    let mut properties = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols.iter().filter(|s| s.concept == "property") {
        let vis = s.get("visibility").unwrap_or("public");
        if vis == "private" {
            continue; // Only public/protected
        }
        if let Some(name) = s.get("name") {
            let type_hint = s.get("type_hint").unwrap_or("");
            let prop = if type_hint.is_empty() {
                format!("${}", name)
            } else {
                format!("{} ${}", type_hint, name)
            };
            if seen.insert(prop.clone()) {
                properties.push(prop);
            }
        }
    }

    properties
}

/// Extract PHP hooks (do_action, apply_filters) from grammar symbols.
fn extract_hooks(symbols: &[Symbol]) -> Vec<HookRef> {
    let mut hooks = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols {
        let hook_type = match s.concept.as_str() {
            "do_action" => "action",
            "apply_filters" => "filter",
            _ => continue,
        };
        if let Some(name) = s.name() {
            if seen.insert((hook_type.to_string(), name.to_string())) {
                hooks.push(HookRef {
                    hook_type: hook_type.to_string(),
                    name: name.to_string(),
                });
            }
        }
    }

    hooks
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_grammar() -> Grammar {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-extensions/rust/grammar.toml",
        );
        if grammar_path.exists() {
            grammar::load_grammar(grammar_path).expect("Failed to load Rust grammar")
        } else {
            // Minimal test grammar
            toml::from_str(
                r#"
                [language]
                id = "rust"
                extensions = ["rs"]
                [comments]
                line = ["//"]
                block = [["/*", "*/"]]
                doc = ["///", "//!"]
                [strings]
                quotes = ['"']
                escape = "\\"
                [blocks]
                open = "{"
                close = "}"
                [patterns.function]
                regex = '^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)'
                context = "any"
                [patterns.function.captures]
                visibility = 1
                name = 2
                params = 3
                [patterns.struct]
                regex = '^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)'
                context = "top_level"
                [patterns.struct.captures]
                visibility = 1
                kind = 2
                name = 3
                [patterns.import]
                regex = '^use\s+([\w:]+(?:::\{[^}]+\})?)\s*;'
                context = "top_level"
                [patterns.import.captures]
                path = 1
                [patterns.impl_block]
                regex = '^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)'
                context = "any"
                [patterns.impl_block.captures]
                trait_name = 1
                type_name = 2
                [patterns.test_attribute]
                regex = '#\[test\]'
                context = "any"
                [patterns.cfg_test]
                regex = '#\[cfg\(test\)\]'
                context = "any"
                "#,
            )
            .expect("Failed to parse minimal grammar")
        }
    }

    #[test]
    fn test_exact_hash_deterministic() {
        let body = "fn foo() { let x = 1; }";
        let h1 = exact_hash(body);
        let h2 = exact_hash(body);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_exact_hash_whitespace_insensitive() {
        let a = "fn foo() {  let x = 1;  }";
        let b = "fn foo() { let x = 1; }";
        assert_eq!(exact_hash(a), exact_hash(b));
    }

    #[test]
    fn test_structural_hash_different_names() {
        let a = "{ let foo = bar(); baz(foo); }";
        let b = "{ let qux = quux(); corge(qux); }";
        assert_eq!(
            structural_hash(a, RUST_KEYWORDS, false),
            structural_hash(b, RUST_KEYWORDS, false),
        );
    }

    #[test]
    fn test_structural_hash_different_structure() {
        let a = "{ let x = 1; if x > 0 { return true; } }";
        let b = "{ let x = 1; for i in 0..x { print(i); } }";
        assert_ne!(
            structural_hash(a, RUST_KEYWORDS, false),
            structural_hash(b, RUST_KEYWORDS, false),
        );
    }

    #[test]
    fn test_parse_param_names_rust() {
        let names = parse_param_names("&self, key: &str, value: String");
        assert_eq!(names, vec!["key", "value"]);
    }

    #[test]
    fn test_parse_param_names_empty() {
        let names = parse_param_names("");
        assert!(names.is_empty());
    }

    #[test]
    fn test_parse_param_names_mut() {
        let names = parse_param_names("&mut self, mut count: usize");
        assert_eq!(names, vec!["count"]);
    }

    #[test]
    fn test_trait_impl_excluded_from_hashes() {
        let grammar = rust_grammar();
        let content = r#"
pub trait Entity {
    fn id(&self) -> &str;
}

pub struct Foo {
    id: String,
}

impl Entity for Foo {
    fn id(&self) -> &str {
        &self.id
    }
}

pub struct Bar {
    id: String,
}

impl Bar {
    fn id(&self) -> &str {
        &self.id
    }
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/test.rs").unwrap();

        // Trait impl method should NOT be in method_hashes
        // But the inherent method on Bar SHOULD be
        // Both should appear in methods list
        assert!(fp.methods.contains(&"id".to_string()));

        // The inherent impl's id() should be hashed (it's a real function)
        // The trait impl's id() should NOT be hashed
        // Since there's only one "id" key in the HashMap, the inherent one wins
        // (or the trait one is excluded, leaving only the inherent one)
        // In practice: with our logic, trait impl is skipped, so only Bar::id is hashed
        assert!(
            fp.method_hashes.contains_key("id"),
            "Bar's inherent id() should be in method_hashes"
        );
    }

    #[test]
    fn test_basic_rust_fingerprint() {
        let grammar = rust_grammar();
        let content = r#"
use std::path::Path;

pub struct Config {
    pub name: String,
}

pub fn load(path: &Path) -> Config {
    let content = std::fs::read_to_string(path).unwrap();
    Config { name: content }
}

fn helper() -> bool {
    true
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/config.rs").unwrap();

        assert!(fp.methods.contains(&"load".to_string()));
        assert!(fp.methods.contains(&"helper".to_string()));
        assert_eq!(fp.type_name, Some("Config".to_string()));
        assert!(fp.method_hashes.contains_key("load"));
        assert!(fp.method_hashes.contains_key("helper"));
        assert_eq!(fp.visibility.get("load"), Some(&"public".to_string()));
        assert_eq!(fp.visibility.get("helper"), Some(&"private".to_string()));
    }

    #[test]
    fn test_test_functions_excluded_from_hashes() {
        let grammar = rust_grammar();
        let content = r#"
pub fn real_fn() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_real_fn() {
        assert!(super::real_fn());
    }
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert!(fp.method_hashes.contains_key("real_fn"));
        assert!(
            !fp.method_hashes.contains_key("test_real_fn"),
            "Test functions should not be in method_hashes"
        );
        // Test method should still be in the methods list
        assert!(fp.methods.contains(&"test_real_fn".to_string()));
    }

    #[test]
    fn test_unused_param_detection() {
        let grammar = rust_grammar();
        let content = r#"
pub(crate) fn uses_both(a: i32, b: i32) -> i32 {
    a + b
}

pub(crate) fn ignores_second(a: i32, b: i32) -> i32 {
    a * 2
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        // ignores_second has unused param b
        assert!(
            fp.unused_parameters
                .iter()
                .any(|p| p.function == "ignores_second" && p.param == "b"),
            "Should detect unused param 'b' in ignores_second"
        );
        // uses_both has no unused params
        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "uses_both"),
            "uses_both should have no unused params"
        );
    }

    #[test]
    fn trait_method_declarations_not_flagged_as_unused_params() {
        let grammar = rust_grammar();
        let content = r#"
pub trait FileSystem {
    fn read(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, content: &str) -> Result<()>;
    fn delete(&self, path: &Path) -> Result<()>;
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert!(
            fp.unused_parameters.is_empty(),
            "Trait method declarations should not produce unused param findings, got: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn trait_impl_methods_not_flagged_as_unused_params() {
        let grammar = rust_grammar();
        // The trait impl uses `path` via display(), but the detector shouldn't
        // even check — trait impls must match the trait's param names.
        let content = r#"
pub trait Store {
    fn save(&self, key: &str, value: &str) -> bool;
}

pub struct MemStore;

impl Store for MemStore {
    fn save(&self, key: &str, value: &str) -> bool {
        key.len() > 0
    }
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        // value is unused in the impl, but since it's a trait impl it should be skipped
        assert!(
            !fp.unused_parameters.iter().any(|p| p.function == "save"),
            "Trait impl methods should not produce unused param findings, got: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn skip_list_does_not_suppress_defined_function_calls() {
        let grammar = rust_grammar();
        // "write" is in SKIP_CALLS_RUST (for the write! macro), but this
        // file defines fn write(...) and calls it — so it should appear
        // in internal_calls.
        let content = r#"
fn run() {
    let result = write("hello");
}

fn write(msg: &str) -> bool {
    println!("{}", msg);
    true
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/file.rs").unwrap();

        assert!(
            fp.internal_calls.contains(&"write".to_string()),
            "write should be in internal_calls when the file defines fn write(), got: {:?}",
            fp.internal_calls
        );
    }

    #[test]
    fn test_normalize_whitespace() {
        assert_eq!(normalize_whitespace("a  b\n\tc"), "a b c");
        assert_eq!(normalize_whitespace("  hello  "), "hello");
    }

    #[test]
    fn test_replace_string_literals() {
        assert_eq!(
            replace_string_literals(r#"let x = "hello" + 'world'"#),
            "let x = STR + STR"
        );
    }

    /// Load the WordPress (PHP) grammar for reproducer tests.
    ///
    /// Tests that need real PHP parsing are gated on the grammar being
    /// available in the local workspace. In CI, the grammar is checked out
    /// alongside this repo via the standard workspace layout.
    fn php_grammar() -> Option<Grammar> {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-extensions/wordpress/grammar.toml",
        );
        if !grammar_path.exists() {
            return None;
        }
        grammar::load_grammar(grammar_path).ok()
    }

    #[test]
    fn namespace_with_php_reserved_word_segment_is_extracted() {
        // Regression test for #1134.
        //
        // PHP 7.0+ allows reserved words as namespace segments via context-
        // sensitive lexing. The auditor must not lose the namespace just
        // because `Global`, `List`, `Class`, etc. appear in it.
        let Some(grammar) = php_grammar() else {
            eprintln!("Skipping — wordpress grammar not available");
            return;
        };

        let content = "<?php\nnamespace DataMachine\\Engine\\AI\\Tools\\Global;\n\nclass WebFetch {\n    public function handle() {}\n}\n";

        let fp =
            fingerprint_from_grammar(content, &grammar, "inc/Engine/AI/Tools/Global/WebFetch.php")
                .expect("fingerprint should succeed");

        assert_eq!(
            fp.namespace.as_deref(),
            Some("DataMachine\\Engine\\AI\\Tools\\Global"),
            "Namespace with reserved word segment 'Global' should be extracted. Got: {:?}",
            fp.namespace
        );
    }

    #[test]
    fn namespace_with_leading_whitespace_is_extracted() {
        // Regression test for #1134 (real-world case).
        //
        // data-machine has files like Engine/AI/Tools/Global/AgentMemory.php
        // where the namespace line has a leading tab/indent (stylistic choice
        // after a docblock). The grammar regex is anchored to `^namespace`,
        // which fails when the line has leading whitespace.
        //
        // The auditor must handle this — PHP is insensitive to indentation
        // of the namespace declaration, and indented namespace declarations
        // are valid PHP.
        let Some(grammar) = php_grammar() else {
            eprintln!("Skipping — wordpress grammar not available");
            return;
        };

        let content = "<?php\n/**\n * Docblock.\n */\n\n\tnamespace DataMachine\\Engine\\AI\\Tools\\Global;\n\nclass AgentMemory {}\n";

        let fp = fingerprint_from_grammar(
            content,
            &grammar,
            "inc/Engine/AI/Tools/Global/AgentMemory.php",
        )
        .expect("fingerprint should succeed");

        assert_eq!(
            fp.namespace.as_deref(),
            Some("DataMachine\\Engine\\AI\\Tools\\Global"),
            "Namespace with leading whitespace (valid PHP) should be extracted. Got: {:?}",
            fp.namespace
        );
    }

    #[test]
    fn unused_param_not_flagged_for_wp_rest_request_contract() {
        // Regression test for #1136.
        //
        // A REST route callback receives a WP_REST_Request $request but may
        // not use it (e.g., reads directly from options). The contract is
        // fixed by register_rest_route(); the parameter cannot be removed.
        let Some(grammar) = php_grammar() else {
            eprintln!("Skipping — wordpress grammar not available");
            return;
        };

        let content = "<?php\nnamespace X;\n\nclass Tokens {\n    public function list_external_tokens( \\WP_REST_Request $request ): \\WP_REST_Response {\n        $tokens = get_option( 'keys', array() );\n        return rest_ensure_response( $tokens );\n    }\n}\n";

        let fp = fingerprint_from_grammar(content, &grammar, "inc/Tokens.php")
            .expect("fingerprint should succeed");

        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "list_external_tokens" && p.param == "request"),
            "WP_REST_Request contract param should not be flagged as unused. Got: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn unused_param_not_flagged_for_ability_execute_contract() {
        // A WP_Ability execute() method has a fixed signature that receives
        // array $input. Even when the method doesn't use $input (checks global
        // caps), the parameter is required by the ability contract.
        let Some(grammar) = php_grammar() else {
            eprintln!("Skipping — wordpress grammar not available");
            return;
        };

        let content = "<?php\nnamespace X;\n\nclass PermissionHelper {\n    public function checkPermission( array $input ): bool {\n        return current_user_can( 'manage_options' );\n    }\n}\n";

        let fp = fingerprint_from_grammar(content, &grammar, "inc/PermissionHelper.php")
            .expect("fingerprint should succeed");

        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "checkPermission" && p.param == "input"),
            "Ability checkPermission() $input contract param should not be flagged. Got: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn unused_param_still_flagged_for_normal_helper_method() {
        // Sanity check: genuine unused params in normal helper methods
        // should still be flagged. The contract-aware exclusions must not
        // swallow real findings.
        let Some(grammar) = php_grammar() else {
            eprintln!("Skipping — wordpress grammar not available");
            return;
        };

        let content = "<?php\nnamespace X;\n\nclass Helper {\n    public function compute( int $left, int $right ): int {\n        return $left * 2;\n    }\n}\n";

        let fp = fingerprint_from_grammar(content, &grammar, "inc/Helper.php")
            .expect("fingerprint should succeed");

        assert!(
            fp.unused_parameters
                .iter()
                .any(|p| p.function == "compute" && p.param == "right"),
            "Genuine unused param should still be flagged. Got: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn test_helpers_without_test_attr_not_counted_as_test_methods() {
        // Regression: functions inside #[cfg(test)] without #[test] attribute
        // were fingerprinted as test methods. Two cases:
        //
        // 1. fn test_insertion() — starts with test_, is a factory helper
        //    → was included as-is, orphan detector looked for "insertion"
        //
        // 2. fn rust_grammar() — doesn't start with test_, is a grammar builder
        //    → was prefixed to "test_rust_grammar", orphan detector looked for "rust_grammar"
        //
        // Both caused false orphaned test findings when no matching source method existed.
        let grammar = rust_grammar();
        let content = r#"
pub fn from_insertion(ins: &str) -> String {
    ins.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_insertion() -> String {
        "fixture".to_string()
    }

    fn rust_grammar() -> String {
        "grammar".to_string()
    }

    #[test]
    fn test_from_insertion() {
        let result = from_insertion("hello");
        assert_eq!(result, "hello");
    }
}
"#;
        let fp = fingerprint_from_grammar(content, &grammar, "src/core/engine/edit_op.rs")
            .expect("fingerprint should succeed");

        // test_from_insertion has #[test] → should be in methods as "test_from_insertion"
        assert!(
            fp.methods.contains(&"test_from_insertion".to_string()),
            "Actual #[test] function should be in methods list. Methods: {:?}",
            fp.methods
        );

        // test_insertion is a helper (no #[test]) → should NOT be in methods list
        assert!(
            !fp.methods.contains(&"test_insertion".to_string()),
            "Helper fn test_insertion() without #[test] should NOT be in methods. Methods: {:?}",
            fp.methods
        );

        // rust_grammar is a helper (no #[test]) → should NOT be in methods as test_rust_grammar
        assert!(
            !fp.methods.contains(&"test_rust_grammar".to_string()),
            "Helper fn rust_grammar() without #[test] should NOT be in methods. Methods: {:?}",
            fp.methods
        );

        // from_insertion is a real source method → should be in methods list
        assert!(
            fp.methods.contains(&"from_insertion".to_string()),
            "Source method from_insertion should be in methods. Methods: {:?}",
            fp.methods
        );
    }
}
