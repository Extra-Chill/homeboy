//! Extraction — apply grammar patterns to source to produce symbols.
//!
//! Also holds the compiled-regex cache and convenience helpers for feature
//! consumers (name/type/import extraction, namespace lookup, block body
//! extraction).

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::parser::{walk_lines, Region};
use super::types::Grammar;

#[cfg(test)]
use super::parser::ContextualLine;

// ============================================================================
// Extraction — apply grammar patterns to get symbols
// ============================================================================

/// A symbol extracted from source code.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    /// What kind of concept this is (matches the pattern key in the grammar).
    /// e.g., "method", "class", "import", "namespace"
    pub concept: String,

    /// Named captures from the pattern match.
    /// e.g., {"name": "foo", "visibility": "pub", "params": "&self, key: &str"}
    pub captures: HashMap<String, String>,

    /// 1-indexed line number where the symbol was found.
    pub line: usize,

    /// Brace depth at the match location.
    pub depth: i32,

    /// The full matched text.
    pub matched_text: String,
}

impl Symbol {
    /// Get a named capture value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.captures.get(key).map(|s| s.as_str())
    }

    /// Get the "name" capture (most symbols have one).
    pub fn name(&self) -> Option<&str> {
        self.get("name")
    }

    /// Get the "visibility" capture.
    pub fn visibility(&self) -> Option<&str> {
        self.get("visibility")
    }
}

/// Extract all symbols from source content using a grammar.
pub fn extract(content: &str, grammar: &Grammar) -> Vec<Symbol> {
    let lines = walk_lines(content, grammar);
    let mut symbols = Vec::new();

    for (concept_name, pattern) in &grammar.patterns {
        let re = match cached_regex(&pattern.regex) {
            Some(r) => r,
            None => continue, // Skip invalid patterns
        };

        for ctx_line in &lines {
            // Always skip lines inside string literals — function declarations,
            // imports, etc. inside raw strings are not real code.
            if ctx_line.region == Region::StringLiteral {
                continue;
            }

            // Skip based on region
            if pattern.skip_comments
                && (ctx_line.region == Region::LineComment
                    || ctx_line.region == Region::BlockComment)
            {
                continue;
            }

            // Skip based on context constraint
            match pattern.context.as_str() {
                "top_level" if ctx_line.depth != 0 => {
                    continue;
                }
                "in_block" if ctx_line.depth == 0 => {
                    continue;
                }
                _ => {} // "any" or "line" — no constraint
            }

            // Try to match
            if let Some(caps) = re.captures(ctx_line.text) {
                let mut capture_map = HashMap::new();

                for (name, &index) in &pattern.captures {
                    if let Some(m) = caps.get(index) {
                        capture_map.insert(name.clone(), m.as_str().to_string());
                    }
                }

                // Check require_capture filter
                if let Some(ref required) = pattern.require_capture {
                    if capture_map.get(required).is_none_or(|v| v.is_empty()) {
                        continue;
                    }
                }

                symbols.push(Symbol {
                    concept: concept_name.clone(),
                    captures: capture_map,
                    line: ctx_line.line_num,
                    depth: ctx_line.depth,
                    matched_text: caps[0].to_string(),
                });
            }
        }
    }

    // Sort by line number for stable output
    symbols.sort_by_key(|s| s.line);
    symbols
}

static REGEX_CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();

fn cached_regex(pattern: &str) -> Option<Regex> {
    let cache = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(cached) = cache
        .lock()
        .expect("regex cache lock")
        .get(pattern)
        .cloned()
    {
        return cached;
    }

    let compiled = Regex::new(pattern).ok();
    let mut guard = cache.lock().expect("regex cache lock");
    guard
        .entry(pattern.to_string())
        .or_insert_with(|| compiled.clone())
        .clone()
}

#[cfg(test)]
pub(crate) fn regex_cache_has_for_tests(pattern: &str) -> bool {
    REGEX_CACHE
        .get()
        .map(|cache| {
            cache
                .lock()
                .expect("regex cache lock")
                .contains_key(pattern)
        })
        .unwrap_or(false)
}

/// Extract symbols of a specific concept only.
#[cfg(test)]
pub(crate) fn extract_concept(content: &str, grammar: &Grammar, concept: &str) -> Vec<Symbol> {
    extract(content, grammar)
        .into_iter()
        .filter(|s| s.concept == concept)
        .collect()
}

// ============================================================================
// Convenience helpers for feature consumers
// ============================================================================

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

// ============================================================================
// Block body extraction
// ============================================================================

/// Extract the body of a block starting from a given line.
///
/// Finds the opening brace on or after `start_line` (0-indexed into lines),
/// then returns all lines until the matching closing brace.
#[cfg(test)]
pub(crate) fn extract_block_body<'a>(
    lines: &[ContextualLine<'a>],
    start_line_idx: usize,
    grammar: &Grammar,
) -> Option<Vec<&'a str>> {
    let open = grammar.blocks.open.chars().next().unwrap_or('{');
    let close = grammar.blocks.close.chars().next().unwrap_or('}');

    // Find the opening brace
    let mut idx = start_line_idx;
    let mut found_open = false;
    let mut depth: i32 = 0;
    let mut body_lines = Vec::new();

    while idx < lines.len() {
        let line = lines[idx].text;
        for ch in line.chars() {
            if ch == open {
                depth += 1;
                found_open = true;
            } else if ch == close {
                depth -= 1;
                if found_open && depth == 0 {
                    body_lines.push(line);
                    return Some(body_lines);
                }
            }
        }
        if found_open {
            body_lines.push(line);
        }
        idx += 1;
    }

    None
}
