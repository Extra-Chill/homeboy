//! symbol_extraction_helpers — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use crate::extension::grammar::{self, Grammar, Symbol};
use std::path::Path;
use sha2::{Digest, Sha256};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;


/// Extract type_name and type_names from struct/class symbols.
pub(crate) fn extract_types(symbols: &[Symbol]) -> (Option<String>, Vec<String>) {
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
pub(crate) fn extract_extends(symbols: &[Symbol]) -> Option<String> {
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
pub(crate) fn extract_implements(symbols: &[Symbol]) -> Vec<String> {
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
pub(crate) fn extract_namespace(symbols: &[Symbol], relative_path: &str, lang_id: &str) -> Option<String> {
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
pub(crate) fn extract_imports(symbols: &[Symbol]) -> Vec<String> {
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
pub(crate) fn extract_registrations(symbols: &[Symbol]) -> Vec<String> {
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
pub(crate) fn extract_internal_calls(content: &str, skip_calls: &[&str]) -> Vec<String> {
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
pub(crate) fn is_public_visibility(vis: &str) -> bool {
    vis == "public"
}
