//! extraction_apply_grammar — extracted from grammar.rs.

use regex::Regex;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::error::{Error, Result};
use super::Grammar;
use super::Region;
use super::Symbol;
use super::walk_lines;
use super::ContextualLine;


/// Extract all symbols from source content using a grammar.
pub fn extract(content: &str, grammar: &Grammar) -> Vec<Symbol> {
    let lines = walk_lines(content, grammar);
    let mut symbols = Vec::new();

    for (concept_name, pattern) in &grammar.patterns {
        let re = match Regex::new(&pattern.regex) {
            Ok(r) => r,
            Err(_) => continue, // Skip invalid patterns
        };

        for ctx_line in &lines {
            // Skip based on region
            if pattern.skip_comments
                && (ctx_line.region == Region::LineComment
                    || ctx_line.region == Region::BlockComment)
            {
                continue;
            }

            // Skip based on context constraint
            match pattern.context.as_str() {
                "top_level" => {
                    if ctx_line.depth != 0 {
                        continue;
                    }
                }
                "in_block" => {
                    if ctx_line.depth == 0 {
                        continue;
                    }
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

/// Extract symbols of a specific concept only.
#[cfg(test)]
pub fn extract_concept(content: &str, grammar: &Grammar, concept: &str) -> Vec<Symbol> {
    extract(content, grammar)
        .into_iter()
        .filter(|s| s.concept == concept)
        .collect()
}

/// Extract the body of a block starting from a given line.
///
/// Finds the opening brace on or after `start_line` (0-indexed into lines),
/// then returns all lines until the matching closing brace.
#[cfg(test)]
pub fn extract_block_body<'a>(
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
