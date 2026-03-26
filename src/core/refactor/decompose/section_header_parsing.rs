//! section_header_parsing — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use super::Section;


/// Extract section headers from file content.
///
/// Recognizes patterns like:
/// - `// === Section Name ===`
/// - `// --- Section Name ---`
/// - `// *** Section Name ***`
/// - `// Section Name` (preceded by a blank line + separator comment)
pub(crate) fn extract_sections(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let separator_re =
        regex::Regex::new(r"^\s*//\s*[=\-*]{3,}\s*$").expect("valid separator regex");
    let header_re =
        regex::Regex::new(r"^\s*//\s*[=\-*]{2,}\s+(.+?)\s+[=\-*]{2,}\s*$").expect("valid regex");

    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = header_re.captures(line) {
            let name = cap[1].trim().to_string();
            let slug = section_name_to_slug(&name);
            if !slug.is_empty() {
                sections.push(Section {
                    name: slug,
                    start_line: i + 1, // 1-indexed
                });
            }
        } else if separator_re.is_match(line) {
            // Check if the next non-empty line is a comment with a section name
            // (handles the pattern: // ===\n// Section Name\n// ===)
            if let Some(next) = lines.get(i + 1) {
                let trimmed = next.trim();
                if let Some(name) = trimmed
                    .strip_prefix("//")
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.chars().all(|c| "=-*".contains(c)))
                {
                    let slug = section_name_to_slug(name);
                    if !slug.is_empty() && !sections.iter().any(|s| s.name == slug) {
                        sections.push(Section {
                            name: slug,
                            start_line: i + 1,
                        });
                    }
                }
            }
        }
    }

    sections
}

/// Convert a section header name to a snake_case slug suitable for filenames.
///
/// Hyphens are converted to underscores because Rust module names must be
/// valid identifiers (no hyphens). "Whole-file move" → "whole_file_move".
pub(crate) fn section_name_to_slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else if c == '-' {
                '_'
            } else {
                ' '
            }
        })

/// Ensure a group name is a valid Rust module name (identifier).
///
/// Rust identifiers allow `[a-zA-Z_][a-zA-Z0-9_]*`. This is a safety net
/// applied at the final filename construction point — even if earlier stages
/// produce names with invalid characters (hyphens, dots, etc.), the filename
/// will be valid.
pub(crate) fn sanitize_module_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })

/// Truncate a module name to at most `MAX_MODULE_NAME_WORDS` meaningful words.
///
/// Stop words (prepositions, articles) are dropped entirely rather than counted
/// toward the limit. This produces names like `grammar_loading` instead of
/// `grammar_definition_loaded_from_extension_toml_json`.
pub(crate) fn truncate_module_name(name: &str) -> String {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();

    let mut meaningful_count = 0;
    let mut kept: Vec<&str> = Vec::new();

    for part in &parts {
        if is_stop_word(part) {
            // Drop stop words entirely — they add length without meaning
            continue;
        }
        meaningful_count += 1;
        kept.push(part);
        if meaningful_count >= MAX_MODULE_NAME_WORDS {
            break;
        }
    }

    if kept.is_empty() {
        // All words were stop words; fall back to the first segment
        parts.first().map(|s| s.to_string()).unwrap_or_default()
    } else {
        kept.join("_")
    }
}

/// Assign an item to a section based on its line number.
pub(crate) fn find_section_for_item(sections: &[Section], item_start_line: usize) -> Option<&str> {
    // Find the last section whose start_line is <= item_start_line
    sections
        .iter()
        .rev()
        .find(|s| s.start_line <= item_start_line)
        .map(|s| s.name.as_str())
}
