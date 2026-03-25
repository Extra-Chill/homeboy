//! php_specific_extraction — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use sha2::{Digest, Sha256};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;


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

/// Extract PHP class properties from property symbols.
pub(crate) fn extract_properties(symbols: &[Symbol]) -> Vec<String> {
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
pub(crate) fn extract_hooks(symbols: &[Symbol]) -> Vec<HookRef> {
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
