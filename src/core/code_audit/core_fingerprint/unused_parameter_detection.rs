//! unused_parameter_detection — extracted from core_fingerprint.rs.

use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use sha2::{Digest, Sha256};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;


/// Detect function parameters that are declared but never used in the body.
pub(crate) fn detect_unused_params(functions: &[FunctionInfo], _lang_id: &str) -> Vec<UnusedParam> {
    let mut unused = Vec::new();

    for f in functions {
        if f.is_test || f.is_trait_impl || f.params.is_empty() || f.body.is_empty() {
            continue;
        }

        // Parse parameter names from the params string
        let param_names = parse_param_names(&f.params);

        // Extract body-only text (after first opening brace)
        let body_after_brace = if let Some(pos) = f.body.find('{') {
            &f.body[pos + 1..]
        } else {
            continue;
        };

        for (idx, pname) in param_names.iter().enumerate() {
            // Skip self, mut, underscore-prefixed
            if pname == "self" || pname == "mut" || pname == "Self" || pname.starts_with('_') {
                continue;
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

/// Parse parameter names from a params string like "&self, key: &str, value: String".
pub(crate) fn parse_param_names(params: &str) -> Vec<String> {
    let mut names = Vec::new();
    for chunk in params.split(',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        // Rust: "name: Type" or "mut name: Type"
        // PHP: "TypeHint $name" — handled by checking for $ prefix
        if chunk.contains(':') {
            // Rust-style: everything before the colon is the pattern
            let before_colon = chunk.split(':').next().unwrap_or("").trim();
            // Strip "mut" prefix
            let name = before_colon.trim_start_matches("mut").trim();
            if !name.is_empty() && name != "&self" && name != "self" {
                // Handle & prefix
                let name = name.trim_start_matches('&');
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        } else if chunk.contains('$') {
            // PHP-style: $name
            static RE: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new(r"\$(\w+)").unwrap());
            if let Some(caps) = RE.captures(chunk) {
                names.push(caps[1].to_string());
            }
        }
    }
    names
}

/// Extract dead code suppression markers.
pub(crate) fn extract_dead_code_markers(symbols: &[Symbol], lines: &[&str]) -> Vec<DeadCodeMarker> {
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
