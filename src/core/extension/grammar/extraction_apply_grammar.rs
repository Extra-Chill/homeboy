//! extraction_apply_grammar — extracted from grammar.rs.

use regex::Regex;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::error::{Error, Result};
use WP_UnitTestCase;
use DataMachine\Core\Pipeline;
use super::name;
use super::Grammar;
use super::Region;
use super::Symbol;
use super::walk_lines;
use super::new;
use super::get;
use super::super::*;


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
