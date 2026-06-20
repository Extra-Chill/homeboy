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

use crate::core::extension::grammar::{self, Grammar, Symbol};
use crate::core::extension::{self, DeadCodeMarker, HookRef, UnusedParam};

use super::conventions::Language;
use super::fingerprint::FileFingerprint;

mod hash;
mod relationships;

use self::hash::{exact_hash, structural_hash};
use self::relationships::{extract_extends, extract_implements};

#[cfg(test)]
use self::hash::{normalize_whitespace, replace_string_literals};

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
    //
    // Production methods and inline test methods are kept in SEPARATE lists so
    // a production method whose name begins with the test prefix (e.g.
    // `ExtensionManifest::test_script()`) is never confused with an inline
    // `#[test] fn test_script()`. See Extra-Chill/homeboy#1471 for the bug
    // this separation prevents.
    //
    // `methods` holds non-test functions. `test_methods` holds test functions
    // (those with an explicit `#[test]` attribute) with the test prefix
    // normalized on — this is the canonical form the test-coverage detector
    // expects.
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

    // Collect test methods separately.
    //
    // Only functions with an explicit `#[test]` attribute qualify. Helpers
    // inside `#[cfg(test)]` modules without `#[test]` (factories, fixtures,
    // grammar builders) are deliberately excluded — including them would
    // cause the orphaned-test detector to flag them when no matching source
    // method exists.
    let mut test_methods = Vec::new();
    let mut seen_test_methods = HashSet::new();
    for f in &functions {
        if f.is_test && f.has_test_attr {
            let prefixed = if f.name.starts_with("test_") {
                f.name.clone()
            } else {
                format!("test_{}", f.name)
            };
            if !seen_test_methods.contains(&prefixed) {
                test_methods.push(prefixed.clone());
                seen_test_methods.insert(prefixed);
            }
        }
    }

    // --- Method hashes and structural hashes ---
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
        let structural = structural_hash(&f.body, grammar);
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
    let namespace = extract_namespace(&symbols, relative_path, grammar);

    // --- Imports ---
    let imports = extract_imports(&symbols);

    // --- Registrations ---
    let registrations = extract_registrations(&symbols, grammar);

    // --- Internal calls ---
    // Build the effective skip list: exclude names that are also defined as
    // functions in this file. E.g. a grammar may skip "write" for a language
    // macro, but if this file defines `fn write(...)`, calls to it should
    // still appear in internal_calls.
    let defined_names: HashSet<&str> = functions.iter().map(|f| f.name.as_str()).collect();
    let effective_skip: Vec<&str> = grammar
        .fingerprint
        .skip_calls
        .iter()
        .map(|name| name.as_str())
        .filter(|name| !defined_names.contains(*name))
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
    let unused_parameters = detect_unused_params(&functions, grammar);

    // --- Dead code markers ---
    let dead_code_markers = extract_dead_code_markers(&symbols, &lines);

    // --- Properties (PHP-specific, from grammar) ---
    let properties = extract_properties(&symbols);

    // --- Hooks (PHP-specific, from grammar) ---
    let hooks = extract_hooks(&symbols, grammar);

    // --- Runtime-dispatched types (extension-owned grammar metadata) ---
    let runtime_dispatched_types = extract_runtime_dispatched_types(&symbols);

    Some(FileFingerprint {
        relative_path: relative_path.to_string(),
        language,
        methods,
        test_methods,
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
        hook_callbacks: Vec::new(), // Core grammar engine doesn't extract hook callbacks yet
        runtime_dispatched_types,
        convention_tags: Vec::new(),
        trait_impl_methods,
        aggregate_literals: Vec::new(),
        aggregate_construction_seams: Vec::new(),
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

/// Extract namespace from symbols or derive from grammar-owned path metadata.
fn extract_namespace(symbols: &[Symbol], relative_path: &str, grammar: &Grammar) -> Option<String> {
    // Direct namespace symbol (PHP: namespace SamplePlugin\Abilities;)
    for s in symbols.iter().filter(|s| s.concept == "namespace") {
        if let Some(name) = s.name() {
            return Some(name.to_string());
        }
    }

    derive_namespace_from_path(relative_path, grammar)
}

fn derive_namespace_from_path(relative_path: &str, grammar: &Grammar) -> Option<String> {
    let rule = grammar.fingerprint.namespace_derivation.as_ref()?;
    let path_without_extension = Path::new(relative_path)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");
    let parts: Vec<&str> = path_without_extension
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let stripped = parts.get(rule.strip_leading_segments..)?;

    let namespace_parts = if stripped.len() > 1 {
        &stripped[..stripped.len() - 1]
    } else if rule.include_file_stem_when_root {
        stripped
    } else {
        &[]
    };

    if namespace_parts.is_empty() {
        return None;
    }

    Some(format!(
        "{}{}",
        rule.prefix.as_deref().unwrap_or(""),
        namespace_parts.join(&rule.separator)
    ))
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
/// Matches registration-like concepts supplied by grammar fingerprint metadata.
fn extract_registrations(symbols: &[Symbol], grammar: &Grammar) -> Vec<String> {
    let registration_concepts: HashSet<&str> = grammar
        .fingerprint
        .registration_concepts
        .iter()
        .map(|concept| concept.as_str())
        .collect();
    let skip_names: HashSet<&str> = grammar
        .fingerprint
        .registration_skip_names
        .iter()
        .map(|name| name.as_str())
        .collect();
    let skip_prefixes = &grammar.fingerprint.registration_skip_prefixes;
    let mut registrations = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols
        .iter()
        .filter(|s| registration_concepts.contains(s.concept.as_str()))
    {
        if let Some(name) = s.name() {
            if skip_names.contains(name) {
                continue;
            }
            if skip_prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                continue;
            }
            if seen.insert(name.to_string()) {
                registrations.push(name.to_string());
            }
        }
    }

    registrations
}

/// Extract types registered with runtime dispatchers from grammar symbols.
fn extract_runtime_dispatched_types(symbols: &[Symbol]) -> Vec<String> {
    let mut dispatched_types = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols
        .iter()
        .filter(|s| s.concept == "runtime_dispatched_type")
    {
        if let Some(name) = s.name() {
            let normalized = name.trim_start_matches('\\').to_string();
            if seen.insert(normalized.clone()) {
                dispatched_types.push(normalized);
            }
        }
    }

    dispatched_types
}

/// Extract internal function calls from content.
fn extract_internal_calls(content: &str, skip_calls: &[&str]) -> Vec<String> {
    let skip_set: HashSet<&str> = skip_calls.iter().copied().collect();
    let mut calls = HashSet::new();

    // Match function_name( patterns
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b(\w+)\s*\(").unwrap());
    for caps in RE.captures_iter(content) {
        insert_unskipped_call(&caps, &skip_set, &mut calls);
    }

    // Match .method( and ::method( patterns
    static METHOD_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[.:](\w+)\s*\(").unwrap());
    for caps in METHOD_RE.captures_iter(content) {
        insert_unskipped_call(&caps, &skip_set, &mut calls);
    }

    let mut result: Vec<String> = calls.into_iter().collect();
    result.sort();
    result
}

fn insert_unskipped_call(
    caps: &regex::Captures<'_>,
    skip_set: &HashSet<&str>,
    calls: &mut HashSet<String>,
) {
    let name = &caps[1];
    if !skip_set.contains(name) {
        calls.insert(name.to_string());
    }
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
fn detect_unused_params(functions: &[FunctionInfo], grammar: &Grammar) -> Vec<UnusedParam> {
    let mut unused = Vec::new();

    for f in functions {
        if f.is_test || f.is_trait_impl || f.params.is_empty() || f.body.is_empty() {
            continue;
        }

        // Skip contract methods entirely. These have a fixed signature imposed
        // by a framework/interface and the parameters cannot be removed even
        // when unused. Flagging them produces churny CI noise (#1136).
        if is_contract_method_by_name(&f.name, grammar) {
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

            // Skip params whose type hint is a grammar-declared framework
            // contract type. The parameter exists to satisfy a callback
            // signature, not because the function must use it (#1136).
            if let Some(type_hint) = &p.type_hint {
                if is_contract_type_hint(type_hint, grammar) {
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
    /// and nullable markers are preserved.
    /// For Rust, this is the type after the colon (e.g. `&str`).
    type_hint: Option<String>,
}

/// Parse parameters from a params string into (name, type_hint) pairs.
///
/// Supports both Rust (`name: Type`) and PHP (`Type $name`) signatures.
fn parse_params(params: &str) -> Vec<Param> {
    let mut out = Vec::new();
    for chunk in split_top_level_commas(params) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if let Some(colon_pos) = top_level_param_colon(chunk) {
            // Rust-style: "name: Type" or "mut name: Type" or "&self"
            let before_colon = chunk[..colon_pos].trim();
            let after_colon = chunk[colon_pos + 1..].trim();
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

/// Split a parameter list on commas that are not inside nested types.
fn split_top_level_commas(params: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;

    for (idx, ch) in params.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                chunks.push(&params[start..idx]);
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }

    chunks.push(&params[start..]);
    chunks
}

/// Find the Rust parameter-name colon, ignoring `::` in type paths.
fn top_level_param_colon(param: &str) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = param.as_bytes();

    for (idx, ch) in param.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                let prev_is_colon = idx > 0 && bytes[idx - 1] == b':';
                let next_is_colon = bytes.get(idx + 1).is_some_and(|b| *b == b':');
                if !prev_is_colon && !next_is_colon {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }

    None
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
/// The concrete list is owned by grammar fingerprint metadata so framework
/// contracts stay outside Homeboy core.
fn is_contract_method_by_name(name: &str, grammar: &Grammar) -> bool {
    grammar
        .fingerprint
        .contract_method_names
        .iter()
        .any(|contract_name| contract_name == name)
}

/// Whether a type hint names a framework contract type whose presence
/// in a parameter list indicates the signature is callback-shaped.
///
/// When a parameter's type hint matches one of these, the parameter exists
/// to satisfy a framework callback contract (e.g. WordPress hook callback,
/// REST route callback) and cannot be removed even when unused.
///
/// Handles leading `\` and nullable `?` markers. Matches on the *terminal*
/// class name only so namespaced references are still caught.
fn is_contract_type_hint(type_hint: &str, grammar: &Grammar) -> bool {
    // Strip nullable marker and leading backslashes
    let hint = type_hint.trim_start_matches('?').trim_start_matches('\\');
    // Split on union/intersection markers and check each alternative
    for alt in hint.split(['|', '&']) {
        let alt = alt.trim().trim_start_matches('\\');
        // Extract terminal class name (last backslash-separated segment)
        let terminal = alt.rsplit('\\').next().unwrap_or(alt);
        if grammar
            .fingerprint
            .contract_type_hints
            .iter()
            .any(|contract_name| contract_name == terminal)
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Dead code markers
// ============================================================================

/// Extract dead code suppression markers.
fn extract_dead_code_markers(symbols: &[Symbol], lines: &[&str]) -> Vec<DeadCodeMarker> {
    static ITEM_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:static\s+)?(?:fn|struct|enum|type|trait|const|static|mod)\s+(\w+)",
        )
        .unwrap()
    });

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
            if let Some(caps) = ITEM_RE.captures(line) {
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
// Grammar-symbol extraction helpers
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

/// Extract hook/event references from grammar symbols.
fn extract_hooks(symbols: &[Symbol], grammar: &Grammar) -> Vec<HookRef> {
    let mut hooks = Vec::new();
    let mut seen = HashSet::new();

    for s in symbols {
        let Some(hook_type) = grammar.fingerprint.hook_concepts.get(&s.concept) else {
            continue;
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

    const TEST_GRAMMAR_BLOCKS: &str = r#"
                escape = "\\"
                [blocks]
                open = "{"
                close = "}"
"#;

    fn assert_no_unused_parameters(fp: &FileFingerprint, message: &str) {
        assert!(
            fp.unused_parameters.is_empty(),
            "{}: {:?}",
            message,
            fp.unused_parameters
        );
    }

    fn rust_grammar() -> Grammar {
        let grammar_path = std::path::Path::new(
            "/var/lib/sampleplugin/workspace/homeboy-extensions/rust/grammar.toml",
        );
        if grammar_path.exists() {
            grammar::load_grammar(grammar_path).expect("Failed to load Rust grammar")
        } else {
            // Minimal test grammar
            let source = r#"
                [language]
                id = "rust"
                extensions = ["rs"]
                [comments]
                line = ["//"]
                block = [["/*", "*/"]]
                doc = ["///", "//!"]
                [strings]
                quotes = ['"']
                __TEST_GRAMMAR_BLOCKS__
                [fingerprint]
                keywords = ["fn", "let", "if", "for", "return", "true", "false", "pub", "struct", "impl", "trait", "Self", "Result", "String", "bool", "i32", "usize"]
                skip_calls = ["if", "for", "return", "println", "write", "assert"]
                contract_method_names = []
                contract_type_hints = []
                registration_concepts = ["macro_invocation"]
                registration_skip_names = ["println", "assert", "write"]
                registration_skip_prefixes = ["test"]
                [fingerprint.namespace_derivation]
                prefix = "crate::"
                strip_leading_segments = 1
                separator = "::"
                include_file_stem_when_root = true
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
                "#
            .replace("__TEST_GRAMMAR_BLOCKS__", TEST_GRAMMAR_BLOCKS);
            toml::from_str(&source).expect("Failed to parse minimal grammar")
        }
    }

    #[test]
    fn rust_namespace_comes_from_file_path_not_crate_imports() {
        let mut grammar = rust_grammar();
        grammar.fingerprint.namespace_derivation = Some(grammar::NamespaceDerivationConfig {
            prefix: Some("crate::".to_string()),
            strip_leading_segments: 1,
            separator: "::".to_string(),
            include_file_stem_when_root: true,
        });

        let command_content = r#"
use crate::help_topics;

pub fn run() {
    help_topics::print_all();
}
"#;
        let command_fp =
            fingerprint_from_grammar(command_content, &grammar, "src/commands/docs.rs")
                .expect("fingerprint should succeed");

        assert_eq!(command_fp.namespace.as_deref(), Some("crate::commands"));

        let nested_content = r#"
use crate::core::Result;

pub fn undo() -> Result<()> {
    Ok(())
}
"#;
        let nested_fp =
            fingerprint_from_grammar(nested_content, &grammar, "src/core/engine/undo.rs")
                .expect("fingerprint should succeed");

        assert_eq!(nested_fp.namespace.as_deref(), Some("crate::core::engine"));
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
            structural_hash(a, &rust_grammar()),
            structural_hash(b, &rust_grammar()),
        );
    }

    #[test]
    fn test_structural_hash_different_structure() {
        let a = "{ let x = 1; if x > 0 { return true; } }";
        let b = "{ let x = 1; for i in 0..x { print(i); } }";
        assert_ne!(
            structural_hash(a, &rust_grammar()),
            structural_hash(b, &rust_grammar()),
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
    fn test_parse_param_names_rust_nested_type_paths() {
        let names = parse_param_names("overrides: &[(String, serde_json::Value)]");
        assert_eq!(names, vec!["overrides"]);
    }

    #[test]
    fn test_parse_param_names_ignores_bare_type_paths() {
        let names = parse_param_names("serde_json::Value");
        assert!(names.is_empty());
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
        // Test method lives in `test_methods`, not `methods`, so a production
        // method named `test_*` can't be confused with an inline `#[test]`.
        assert!(fp.test_methods.contains(&"test_real_fn".to_string()));
        assert!(
            !fp.methods.contains(&"test_real_fn".to_string()),
            "Inline #[test] functions must not leak into `methods`"
        );
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
    fn rust_unused_param_detection_handles_typed_nested_params() {
        let grammar = rust_grammar();
        let content = r#"
pub fn settings_json(overrides: &[(String, serde_json::Value)]) -> Self {
    self.settings_json_overrides.extend(overrides.iter().cloned());
    self
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert_no_unused_parameters(&fp, "Nested type paths should not be parsed as parameters");
    }

    #[test]
    fn rust_unused_param_detection_sees_comparison_usage() {
        let grammar = rust_grammar();
        let content = r#"
fn parse_field_line(line: &str, syntax: FieldSyntax) -> Option<FieldSignature> {
    let trimmed = line.trim();

    if syntax == FieldSyntax::Php {
        return parse_php_property_line(trimmed);
    }

    None
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "parse_field_line" && p.param == "syntax"),
            "Parameter usage in comparisons should be detected. Got: {:?}",
            fp.unused_parameters
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
    // Trait declarations have no body to inspect.
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert_no_unused_parameters(
            &fp,
            "Trait method declarations should not produce unused param findings",
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
        // The grammar suppresses "write" (for a macro-like call), but this
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
    fn rust_internal_calls_include_test_prefixed_production_helpers() {
        let grammar = rust_grammar();
        let content = r#"
use crate::core::code_audit::test_mapping::test_to_source_path;

pub fn map_source() {
    let _ = test_to_source_path("tests/core/audit_test.rs", &Default::default());
}
"#;

        let fp =
            fingerprint_from_grammar(content, &grammar, "src/core/code_audit/test_coverage.rs")
                .unwrap();

        assert!(
            fp.internal_calls
                .contains(&"test_to_source_path".to_string()),
            "test_-prefixed production helpers should be retained as references, got: {:?}",
            fp.internal_calls
        );
    }

    #[test]
    fn grammar_skip_calls_drive_internal_call_extraction() {
        let grammar = rust_grammar();
        let content = r#"
fn run() {
    guard();
    helper();
}

fn helper() {}
"#;

        let mut grammar = grammar;
        grammar.fingerprint.skip_calls = vec!["guard".to_string()];

        let fp = fingerprint_from_grammar(content, &grammar, "src/file.rs").unwrap();

        assert!(fp.internal_calls.contains(&"helper".to_string()));
        assert!(
            !fp.internal_calls.contains(&"guard".to_string()),
            "grammar skip_calls should suppress guard(), got: {:?}",
            fp.internal_calls
        );
    }

    fn php_metadata_grammar() -> Grammar {
        let source = r##"
            [language]
            id = "php"
            extensions = ["php"]
            [comments]
            line = ["//", "#"]
            block = [["/*", "*/"]]
            [strings]
            quotes = ['"', "'"]
            __TEST_GRAMMAR_BLOCKS__
            [fingerprint]
            keywords = ["class", "function", "public", "return", "int", "string", "bool", "true", "false"]
            skip_calls = ["if", "return"]
            variable_prefixes = ["$"]
            preserved_variables = ["$this"]
            contract_method_names = ["contractExecute"]
            contract_type_hints = ["FrameworkRequest"]
            registration_concepts = []
            [fingerprint.hook_concepts]
            emit_event = "action"
            transform_value = "filter"
            [patterns.method]
            regex = '((?:(?:public|protected|private|static|abstract|final)\s+)*)function\s+(\w+)\s*\(([^)]*)\)'
            context = "any"
            [patterns.method.captures]
            modifiers = 1
            name = 2
            params = 3
            [patterns.class]
            regex = '^\s*(?:class|trait|interface)\s+(\w+)'
            context = "top_level"
            [patterns.class.captures]
            name = 1
            [patterns.emit_event]
            regex = "emit_event\\s*\\(\\s*['\"]([^'\"]+)['\"]"
            context = "any"
            skip_strings = false
            [patterns.emit_event.captures]
            name = 1
            [patterns.transform_value]
            regex = "transform_value\\s*\\(\\s*['\"]([^'\"]+)['\"]"
            context = "any"
            skip_strings = false
            [patterns.transform_value.captures]
            name = 1
            "##
        .replace("__TEST_GRAMMAR_BLOCKS__", TEST_GRAMMAR_BLOCKS);
        toml::from_str(&source).expect("metadata grammar should parse")
    }

    #[test]
    fn grammar_contract_metadata_suppresses_framework_unused_params() {
        let grammar = php_metadata_grammar();
        let content = "<?php\nclass Sample {\n    public function contractExecute( string $input ): bool {\n        return true;\n    }\n    public function route( FrameworkRequest $request ): bool {\n        return true;\n    }\n    public function helper( int $left, int $right ): int {\n        return $left * 2;\n    }\n}\n";

        let fp = fingerprint_from_grammar(content, &grammar, "src/Sample.php").unwrap();

        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "contractExecute"),
            "grammar contract_method_names should suppress contractExecute params: {:?}",
            fp.unused_parameters
        );
        assert!(
            !fp.unused_parameters
                .iter()
                .any(|p| p.function == "route" && p.param == "request"),
            "grammar contract_type_hints should suppress FrameworkRequest param: {:?}",
            fp.unused_parameters
        );
        assert!(
            fp.unused_parameters
                .iter()
                .any(|p| p.function == "helper" && p.param == "right"),
            "normal helper params should still be flagged: {:?}",
            fp.unused_parameters
        );
    }

    #[test]
    fn grammar_hook_concepts_drive_hook_extraction() {
        let grammar = php_metadata_grammar();
        let content = "<?php\nclass Sample {\n    public function fire() {\n        emit_event( 'sample_event' );\n        transform_value( 'sample_value', 'x' );\n    }\n}\n";

        let fp = fingerprint_from_grammar(content, &grammar, "src/Sample.php").unwrap();

        assert!(fp
            .hooks
            .iter()
            .any(|hook| hook.hook_type == "action" && hook.name == "sample_event"));
        assert!(fp
            .hooks
            .iter()
            .any(|hook| hook.hook_type == "filter" && hook.name == "sample_value"));
    }

    #[test]
    fn grammar_variable_prefixes_drive_structural_hash_normalization() {
        let grammar = php_metadata_grammar();
        let a = "{ $first = make_value(); return $first; }";
        let b = "{ $second = make_value(); return $second; }";

        assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
    }

    #[test]
    fn existing_grammar_patterns_can_infer_dollar_variable_prefixes() {
        let mut grammar = php_metadata_grammar();
        grammar.fingerprint.variable_prefixes.clear();
        grammar.fingerprint.preserved_variables.clear();
        let a = "{ $first = make_value(); return $first; }";
        let b = "{ $second = make_value(); return $second; }";

        assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
    }

    #[test]
    fn grammar_preserved_variables_keep_receiver_references_stable() {
        let grammar = php_metadata_grammar();
        let a = "{ $first = $this->make_value(); return $first; }";
        let b = "{ $second = $this->make_value(); return $second; }";

        assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
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
            "/var/lib/sampleplugin/workspace/homeboy-extensions/wordpress/grammar.toml",
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

        let content = "<?php\nnamespace SamplePlugin\\Engine\\AI\\Tools\\Global;\n\nclass WebFetch {\n    public function handle() {}\n}\n";

        let fp =
            fingerprint_from_grammar(content, &grammar, "inc/Engine/AI/Tools/Global/WebFetch.php")
                .expect("fingerprint should succeed");

        assert_eq!(
            fp.namespace.as_deref(),
            Some("SamplePlugin\\Engine\\AI\\Tools\\Global"),
            "Namespace with reserved word segment 'Global' should be extracted. Got: {:?}",
            fp.namespace
        );
    }

    #[test]
    fn namespace_with_leading_whitespace_is_extracted() {
        // Regression test for #1134 (real-world case).
        //
        // sample-plugin has files like Engine/AI/Tools/Global/AgentMemory.php
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

        let content = "<?php\n/**\n * Docblock.\n */\n\n\tnamespace SamplePlugin\\Engine\\AI\\Tools\\Global;\n\nclass AgentMemory {}\n";

        let fp = fingerprint_from_grammar(
            content,
            &grammar,
            "inc/Engine/AI/Tools/Global/AgentMemory.php",
        )
        .expect("fingerprint should succeed");

        assert_eq!(
            fp.namespace.as_deref(),
            Some("SamplePlugin\\Engine\\AI\\Tools\\Global"),
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

        // test_from_insertion has #[test] → should be in `test_methods`,
        // and NOT in `methods` (production methods list).
        assert!(
            fp.test_methods.contains(&"test_from_insertion".to_string()),
            "Actual #[test] function should be in test_methods. test_methods: {:?}",
            fp.test_methods
        );
        assert!(
            !fp.methods.contains(&"test_from_insertion".to_string()),
            "Inline #[test] functions must not leak into `methods`. Methods: {:?}",
            fp.methods
        );

        // test_insertion is a helper (no #[test]) → should NOT be in either
        // list; it's neither a production method nor an inline test.
        assert!(
            !fp.methods.contains(&"test_insertion".to_string()),
            "Helper fn test_insertion() without #[test] should NOT be in methods. Methods: {:?}",
            fp.methods
        );
        assert!(
            !fp.test_methods.contains(&"test_insertion".to_string()),
            "Helper fn test_insertion() without #[test] should NOT be in test_methods. test_methods: {:?}",
            fp.test_methods
        );

        // rust_grammar is a helper (no #[test]) → should NOT appear with or
        // without a test_ prefix in either list.
        assert!(
            !fp.methods.contains(&"test_rust_grammar".to_string()),
            "Helper fn rust_grammar() without #[test] should NOT be in methods. Methods: {:?}",
            fp.methods
        );
        assert!(
            !fp.test_methods.contains(&"test_rust_grammar".to_string()),
            "Helper fn rust_grammar() without #[test] should NOT be in test_methods. test_methods: {:?}",
            fp.test_methods
        );

        // from_insertion is a real source method → should be in methods list
        assert!(
            fp.methods.contains(&"from_insertion".to_string()),
            "Source method from_insertion should be in methods. Methods: {:?}",
            fp.methods
        );
    }
}
