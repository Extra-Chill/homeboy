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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::core::extension::grammar::{self, AggregateSeamConfig, Grammar, Symbol};
use crate::core::extension::{
    self, AggregateConstructionSeam, AggregateLiteral, CallSite, DeadCodeMarker, HookRef,
    UnusedParam,
};

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

    // --- Aggregate construction facts (grammar-driven) ---
    let aggregate_construction_seams = extract_aggregate_construction_seams(&functions, grammar);
    let aggregate_literals = extract_aggregate_literals(content, grammar);

    // --- Hook/callback targets and call sites (grammar-driven) ---
    let hook_callbacks = extract_hook_callbacks(content, grammar);
    let call_sites = extract_call_sites(content, grammar);

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
        call_sites,
        public_api,
        hook_callbacks,
        runtime_dispatched_types,
        convention_tags: Vec::new(),
        trait_impl_methods,
        aggregate_literals,
        aggregate_construction_seams,
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
    /// Owning aggregate type name when this function is an impl method.
    /// `None` for free functions. Drives construction-seam recognition.
    impl_type: Option<String>,
    params: String,
    /// 1-indexed declaration line. Used as the construction-seam source line.
    start_line: usize,
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
                type_name,
                trait_name,
            }
        })
        .collect()
}

struct ImplContext {
    line: usize,
    depth: i32,
    type_name: String,
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

        // Find the nearest enclosing impl context (the last one that starts
        // before this function at a shallower depth). Using `any()` was wrong —
        // it matched unrelated impl blocks earlier in the file. This drives both
        // trait-impl detection and the owning type used for construction-seam
        // recognition.
        let enclosing_impl = if symbol.depth > 0 {
            impl_contexts
                .iter()
                .rfind(|ctx| ctx.depth < symbol.depth && ctx.line < symbol.line)
        } else {
            None
        };
        let is_trait_impl = enclosing_impl
            .is_some_and(|ctx| ctx.trait_name.as_ref().is_some_and(|t| !t.is_empty()));
        let impl_type = enclosing_impl
            .map(|ctx| ctx.type_name.clone())
            .filter(|t| !t.is_empty());

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
            impl_type,
            params,
            start_line: symbol.line,
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
// Aggregate construction facts (grammar-driven)
// ============================================================================

/// Extract canonical aggregate construction seams from impl methods.
///
/// A seam is a method whose owning type and name match the grammar's
/// construction-seam naming policy (e.g. `Config::new`, `Plan::from_parts`).
/// Drives the `aggregate_construction_seams` fingerprint field consumed by the
/// direct-aggregate-construction detector. Test functions and free functions
/// with no owning type are excluded. Returns empty when the grammar declares no
/// seam policy.
fn extract_aggregate_construction_seams(
    functions: &[FunctionInfo],
    grammar: &Grammar,
) -> Vec<AggregateConstructionSeam> {
    let Some(cfg) = grammar.fingerprint.aggregate_seams.as_ref() else {
        return Vec::new();
    };

    let mut seams = Vec::new();
    let mut seen = HashSet::new();
    for f in functions {
        if f.is_test {
            continue;
        }
        let Some(type_name) = f.impl_type.as_ref() else {
            continue;
        };
        if !is_canonical_seam(&f.name, type_name, cfg) {
            continue;
        }
        if seen.insert((type_name.clone(), f.name.clone())) {
            seams.push(AggregateConstructionSeam {
                type_name: type_name.clone(),
                method: f.name.clone(),
                line: f.start_line,
            });
        }
    }

    seams
}

/// Whether `method` is a canonical construction seam for `type_name` under the
/// grammar's naming policy.
fn is_canonical_seam(method: &str, type_name: &str, cfg: &AggregateSeamConfig) -> bool {
    if cfg.method_names.iter().any(|name| name == method) {
        return true;
    }
    if cfg
        .method_prefixes
        .iter()
        .any(|prefix| method.starts_with(prefix.as_str()))
    {
        return true;
    }
    if !cfg.type_method_templates.is_empty() {
        let snake = to_snake_case(type_name);
        if cfg
            .type_method_templates
            .iter()
            .any(|template| method == template.replace("{type}", &snake))
        {
            return true;
        }
    }
    false
}

/// Convert a type name to snake_case (e.g. `DispatchPlan` → `dispatch_plan`).
fn to_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Extract direct aggregate literal construction sites from raw content.
///
/// Drives the `aggregate_literals` fingerprint field. The grammar supplies the
/// literal-site regex (capture 1 = type, capture 2 = body), the field regex
/// (capture 1 = field name), the minimum field count, and an optional
/// "skip if preceded by a definition keyword" guard. Returns empty when the
/// grammar declares no literal policy.
fn extract_aggregate_literals(content: &str, grammar: &Grammar) -> Vec<AggregateLiteral> {
    let Some(cfg) = grammar.fingerprint.aggregate_literals.as_ref() else {
        return Vec::new();
    };
    let Some(literal_re) = grammar::cached_regex(&cfg.pattern) else {
        return Vec::new();
    };
    let Some(field_re) = grammar::cached_regex(&cfg.field_pattern) else {
        return Vec::new();
    };
    let skip_before_re = cfg
        .skip_before_pattern
        .as_ref()
        .and_then(|pattern| grammar::cached_regex(pattern));

    let mut literals = Vec::new();
    let mut seen: HashSet<(String, Vec<String>, usize)> = HashSet::new();

    for caps in literal_re.captures_iter(content) {
        let full = match caps.get(0) {
            Some(m) => m,
            None => continue,
        };
        let Some(type_match) = caps.get(1) else {
            continue;
        };
        let type_name = type_match.as_str().to_string();
        let before = &content[..full.start()];

        // Skip type definitions (e.g. `struct Foo { ... }`); only construction
        // sites count as literals.
        if let Some(re) = &skip_before_re {
            if re.is_match(window_suffix(before, cfg.skip_before_window)) {
                continue;
            }
        }

        let body = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let mut fields: Vec<String> = Vec::new();
        for field_caps in field_re.captures_iter(body) {
            if let Some(field) = field_caps.get(1) {
                let field = field.as_str().to_string();
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        if fields.len() < cfg.min_fields {
            continue;
        }

        let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let key = (type_name.clone(), fields.clone(), line);
        if seen.insert(key) {
            literals.push(AggregateLiteral {
                type_name,
                fields,
                line,
            });
        }
    }

    literals
}

/// Return the last `window` characters of `s`. A `window` of 0 returns all of
/// `s` (no limit), which is safe for end-anchored guard patterns.
fn window_suffix(s: &str, window: usize) -> &str {
    if window == 0 {
        return s;
    }
    match s.char_indices().rev().nth(window - 1) {
        Some((idx, _)) => &s[idx..],
        None => s,
    }
}

// ============================================================================
// Hook callbacks and call sites (grammar-driven)
// ============================================================================

/// Extract framework runtime hook/callback registration targets from raw
/// content.
///
/// Drives the `hook_callbacks` fingerprint field, which the dead-code detector
/// uses to recognize that a function defined and hook-registered in the same
/// file is live code invoked by the framework runtime. Each grammar pattern's
/// first capture group is the callback name. Returns a sorted, de-duplicated
/// list; empty when the grammar declares no hook-callback patterns.
fn extract_hook_callbacks(content: &str, grammar: &Grammar) -> Vec<String> {
    let patterns = &grammar.fingerprint.hook_callback_patterns;
    if patterns.is_empty() {
        return Vec::new();
    }

    let mut callbacks: BTreeSet<String> = BTreeSet::new();
    for pattern in patterns {
        let Some(re) = grammar::cached_regex(pattern) else {
            continue;
        };
        for caps in re.captures_iter(content) {
            if let Some(name) = caps.get(1) {
                callbacks.insert(name.as_str().to_string());
            }
        }
    }

    callbacks.into_iter().collect()
}

/// Extract call sites with argument counts from raw content.
///
/// Drives the `call_sites` fingerprint field consumed by cross-file parameter
/// analysis (dead-code) and deprecation-age reference counting. The grammar
/// supplies call-match patterns (capture 1 = target, match ends at the opening
/// delimiter), an optional declaration-line skip pattern, and target name
/// prefixes to ignore. Patterns are applied in order per line; the first to
/// record a `(target, line)` pair wins. Returns empty when the grammar declares
/// no call-site policy.
fn extract_call_sites(content: &str, grammar: &Grammar) -> Vec<CallSite> {
    let Some(cfg) = grammar.fingerprint.call_sites.as_ref() else {
        return Vec::new();
    };
    if cfg.patterns.is_empty() {
        return Vec::new();
    }

    let skip_line_re = cfg
        .skip_line_pattern
        .as_ref()
        .and_then(|pattern| grammar::cached_regex(pattern));
    let skip_calls: HashSet<&str> = grammar
        .fingerprint
        .skip_calls
        .iter()
        .map(String::as_str)
        .collect();
    let compiled: Vec<(Option<regex::Regex>, bool)> = cfg
        .patterns
        .iter()
        .map(|pattern| (grammar::cached_regex(&pattern.regex), pattern.apply_skip_calls))
        .collect();

    let mut sites = Vec::new();
    let mut seen: HashSet<(String, usize)> = HashSet::new();

    for (idx, line) in content.split('\n').enumerate() {
        let line_no = idx + 1;

        // Declaration lines are signatures, not call sites.
        if let Some(re) = &skip_line_re {
            let trimmed = line.trim_start();
            if re.find(trimmed).is_some_and(|m| m.start() == 0) {
                continue;
            }
        }

        for (re, apply_skip_calls) in &compiled {
            let Some(re) = re else {
                continue;
            };
            for caps in re.captures_iter(line) {
                let Some(name_match) = caps.get(1) else {
                    continue;
                };
                let name = name_match.as_str();
                if cfg
                    .skip_name_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix.as_str()))
                {
                    continue;
                }
                if *apply_skip_calls && skip_calls.contains(name) {
                    continue;
                }
                let key = (name.to_string(), line_no);
                if seen.contains(&key) {
                    continue;
                }
                // The match ends at the opening delimiter; count args from there.
                let full = match caps.get(0) {
                    Some(m) => m,
                    None => continue,
                };
                let remaining = &line[full.end().saturating_sub(1)..];
                let Some(arg_count) = count_call_args(remaining) else {
                    continue;
                };
                seen.insert(key);
                sites.push(CallSite {
                    target: name.to_string(),
                    line: line_no,
                    arg_count,
                });
            }
        }
    }

    sites
}

/// Count the arguments in a call whose text begins at the opening `(`.
///
/// Tracks paren depth and counts top-level commas, honoring single/double
/// quoted strings so commas and parens inside string literals do not skew the
/// count. Returns `None` when the parentheses do not balance within `text`
/// (e.g. a multi-line call), matching the reference behavior of skipping such
/// call sites.
fn count_call_args(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() != Some(&'(') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut commas: usize = 0;
    let mut has_content = false;
    let mut in_single = false;
    let mut in_double = false;
    for i in 0..chars.len() {
        let ch = chars[i];
        if in_single {
            if ch == '\'' && (i == 0 || chars[i - 1] != '\\') {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if ch == '"' && (i == 0 || chars[i - 1] != '\\') {
                in_double = false;
            }
            continue;
        }
        match ch {
            '\'' => {
                in_single = true;
                has_content = true;
            }
            '"' => {
                in_double = true;
                has_content = true;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(if has_content { commas + 1 } else { 0 });
                }
            }
            ',' if depth == 1 => commas += 1,
            c if depth == 1 && !c.is_whitespace() => has_content = true,
            _ => {}
        }
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
