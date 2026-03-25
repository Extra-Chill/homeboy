//! helpers — extracted from contract.rs.

use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};
use crate::error::{Error, Result};
use super::FieldDef;


/// Parse field definitions from a struct/class source body using a regex pattern.
///
/// The `field_pattern` is a regex with capture groups for field name and type.
/// `name_group` and `type_group` specify which capture groups to use (1-indexed).
///
/// `visibility_pattern` optionally matches a visibility prefix (e.g., `pub`).
///
/// This is language-agnostic: the grammar provides the regex patterns and
/// capture group assignments.
pub fn parse_fields_from_source(
    source: &str,
    field_pattern: &str,
    visibility_pattern: Option<&str>,
    name_group: usize,
    type_group: usize,
) -> Vec<FieldDef> {
    let field_re = match regex::Regex::new(field_pattern) {
        Ok(re) => re,
        Err(_) => return vec![],
    };
    let vis_re = visibility_pattern.and_then(|p| regex::Regex::new(p).ok());

    let mut fields = Vec::new();
    // Skip the first line (struct declaration) and last line (closing brace)
    let lines: Vec<&str> = source.lines().collect();
    for line in &lines {
        let trimmed = line.trim();
        // Skip empty lines, comments, attributes
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('{')
            || trimmed == "}"
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }
        // Skip the struct/class declaration line itself
        if trimmed.contains("struct ") || trimmed.contains("class ") || trimmed.contains("enum ") {
            continue;
        }

        if let Some(caps) = field_re.captures(trimmed) {
            let name = caps
                .get(name_group)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let field_type = caps
                .get(type_group)
                .map(|m| m.as_str().trim_end_matches(',').trim().to_string())
                .unwrap_or_default();

            if name.is_empty() || field_type.is_empty() {
                continue;
            }

            let is_public = vis_re
                .as_ref()
                .map(|re| re.is_match(trimmed))
                .unwrap_or(false);

            fields.push(FieldDef {
                name,
                field_type,
                is_public,
            });
        }
    }

    fields
}
