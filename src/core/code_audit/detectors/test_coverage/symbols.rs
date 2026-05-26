use std::collections::HashSet;
use std::path::Path;

use crate::core::code_audit::fingerprint::FileFingerprint;

pub(super) fn collect_source_symbol_names(source_fps: &[&FileFingerprint]) -> HashSet<String> {
    let mut names = HashSet::new();

    for fp in source_fps {
        collect_source_symbols(fp, &mut names);
    }

    names
}

pub(super) fn references_multiple_source_symbols(
    test_fp: &FileFingerprint,
    source_symbol_names: &HashSet<String>,
) -> bool {
    let mut referenced = HashSet::new();
    let mut haystacks = Vec::new();
    haystacks.push(test_fp.content.as_str());
    haystacks.extend(test_fp.imports.iter().map(|s| s.as_str()));

    for symbol in source_symbol_names {
        if haystacks
            .iter()
            .any(|haystack| contains_symbol(haystack, symbol))
        {
            referenced.insert(symbol.as_str());
            if referenced.len() >= 2 {
                return true;
            }
        }
    }

    false
}

pub(super) fn references_source_file(
    test_fp: &FileFingerprint,
    source_fp: &FileFingerprint,
) -> bool {
    let mut symbols = HashSet::new();
    collect_source_symbols(source_fp, &mut symbols);
    if symbols.is_empty() {
        return false;
    }

    let mut haystacks = Vec::new();
    haystacks.push(test_fp.content.as_str());
    haystacks.extend(test_fp.imports.iter().map(|s| s.as_str()));

    symbols.iter().any(|symbol| {
        haystacks
            .iter()
            .any(|haystack| contains_symbol(haystack, symbol))
    })
}

fn collect_source_symbols(fp: &FileFingerprint, symbols: &mut HashSet<String>) {
    if let Some(type_name) = &fp.type_name {
        if is_meaningful_symbol_name(type_name) {
            symbols.insert(type_name.clone());
        }
    }
    for type_name in &fp.type_names {
        if is_meaningful_symbol_name(type_name) {
            symbols.insert(type_name.clone());
        }
    }
    if let Some(stem) = Path::new(&fp.relative_path)
        .file_stem()
        .and_then(|s| s.to_str())
    {
        if is_meaningful_symbol_name(stem) {
            symbols.insert(stem.to_string());
        }
    }
}

fn is_meaningful_symbol_name(name: &str) -> bool {
    name.len() >= 3 && name.chars().any(|c| c.is_alphabetic())
}

fn contains_symbol(haystack: &str, symbol: &str) -> bool {
    haystack.match_indices(symbol).any(|(start, _)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + symbol.len()..].chars().next();
        !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
    })
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
