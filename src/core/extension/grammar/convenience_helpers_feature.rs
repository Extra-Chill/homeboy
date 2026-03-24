//! convenience_helpers_feature — extracted from grammar.rs.

use super::visibility;
use super::Symbol;
use super::name;
use super::get;
use super::super::*;


/// Get all method/function names from extracted symbols.
#[cfg(test)]
pub(crate) fn method_names(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| {
            s.concept == "method" || s.concept == "function" || s.concept == "free_function"
        })
        .filter_map(|s| s.name().map(|n| n.to_string()))
        .collect()
}

/// Get all class/struct/trait names from extracted symbols.
#[cfg(test)]
pub(crate) fn type_names(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| {
            s.concept == "class"
                || s.concept == "struct"
                || s.concept == "trait"
                || s.concept == "enum"
                || s.concept == "interface"
                || s.concept == "type"
        })
        .filter_map(|s| s.name().map(|n| n.to_string()))
        .collect()
}

/// Get all import paths from extracted symbols.
#[cfg(test)]
pub(crate) fn import_paths(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| s.concept == "import" || s.concept == "use")
        .filter_map(|s| s.get("path").map(|p| p.to_string()))
        .collect()
}

/// Get the namespace from extracted symbols.
pub fn namespace(symbols: &[Symbol]) -> Option<String> {
    symbols
        .iter()
        .find(|s| s.concept == "namespace" || s.concept == "module")
        .and_then(|s| s.name().map(|n| n.to_string()))
}

/// Filter symbols to only public API (visibility contains "pub" or "public").
#[cfg(test)]
pub(crate) fn public_symbols(symbols: &[Symbol]) -> Vec<&Symbol> {
    symbols
        .iter()
        .filter(|s| {
            s.visibility()
                .is_none_or(|v| v.contains("pub") || v == "public")
        })
        .collect()
}
