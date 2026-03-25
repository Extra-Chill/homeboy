//! default_field_type — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use serde::{Deserialize, Serialize};
use super::super::contract::*;


/// Replace assertion TODO placeholders with real field-level assertions.
///
/// When the return type is a known struct, generates `assert_eq!` for each
/// public field using the field's type-default value as the expected value.
/// This produces tests that actually **assert behavior**, not just document
/// what fields exist.
///
/// Turns:
///   `let _ = inner; // TODO: assert specific value for "skipped"`
/// Into:
///   `assert_eq!(inner.success, false);`
///   `assert_eq!(inner.command, None);`
///   `assert_eq!(inner.rolled_back, false);`
///   `assert_eq!(inner.files_checked, 0);`
pub(crate) fn enrich_assertion_with_fields(
    assertion: &str,
    returns: &ReturnValue,
    return_type: &ReturnShape,
    type_registry: &HashMap<String, TypeDefinition>,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
    field_assertion_template: Option<&str>,
) -> String {
    // Only enrich if the assertion has a TODO placeholder
    if !assertion.contains("TODO:") {
        return assertion.to_string();
    }

    // Determine the inner type name to look up
    let type_name = match return_type {
        ReturnShape::ResultType { ok_type, .. } => {
            if returns.variant == "ok" {
                Some(ok_type.as_str())
            } else {
                None
            }
        }
        ReturnShape::OptionType { some_type } => {
            if returns.variant == "some" {
                Some(some_type.as_str())
            } else {
                None
            }
        }
        ReturnShape::Value { value_type } => Some(value_type.as_str()),
        _ => None,
    };

    let type_name = match type_name {
        Some(n) => n.trim(),
        None => return assertion.to_string(),
    };

    let base_name = type_name
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();

    // Try exact match first, then strip path prefixes (e.g., "crate::engine::ValidationResult" → "ValidationResult")
    let type_def = match type_registry.get(base_name) {
        Some(td) => td,
        None => {
            // Strip Rust path prefix — registry stores bare names
            let short_name = base_name.rsplit("::").next().unwrap_or(base_name);
            match type_registry.get(short_name) {
                Some(td) => td,
                None => return assertion.to_string(),
            }
        }
    };

    let public_fields: Vec<&FieldDef> = type_def.fields.iter().filter(|f| f.is_public).collect();
    if public_fields.is_empty() {
        return assertion.to_string();
    }

    // Must have a field assertion template from the grammar to generate assertions
    let template = match field_assertion_template {
        Some(t) => t,
        None => return assertion.to_string(),
    };

    let indent = "        ";

    // Find the TODO line and everything after it (including the `let _ = inner;` line before it)
    let todo_pos = match assertion.find("// TODO:") {
        Some(pos) => pos,
        None => return assertion.to_string(),
    };

    // Find the start of the line containing the `let _ =` before the TODO
    // We want to replace both the `let _ = inner;` line AND the TODO line
    let search_region = &assertion[..todo_pos];
    let let_underscore_pos = search_region.rfind("let _ = ");
    let replace_start = if let Some(lpos) = let_underscore_pos {
        // Find the line start before `let _ =`
        assertion[..lpos].rfind('\n').map(|p| p + 1).unwrap_or(0)
    } else {
        // No `let _ =` found, just replace from the TODO line start
        assertion[..todo_pos]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0)
    };

    let replace_end = assertion[todo_pos..]
        .find('\n')
        .map(|p| todo_pos + p + 1)
        .unwrap_or(assertion.len());

    // Build real field assertions using the grammar's field_assertion_template
    let mut field_assertions = Vec::new();
    for field in &public_fields {
        let expected = default_for_field_type(&field.field_type, type_defaults, fallback_default);
        let rendered = template
            .replace("{indent}", indent)
            .replace("{field_name}", &field.name)
            .replace("{expected_value}", &expected.replace('"', "\\\""));
        field_assertions.push(rendered);
    }

    format!(
        "{}{}{}",
        &assertion[..replace_start],
        field_assertions.join("\n"),
        &assertion[replace_end..],
    )
}

/// Resolve a default/zero value for a field type to use as expected assertion value.
///
/// Uses the grammar's `type_defaults` exclusively — no language-specific fallbacks
/// in core. If no `type_default` matches, uses `fallback_default` from the grammar.
pub(crate) fn default_for_field_type(
    field_type: &str,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> String {
    let trimmed = field_type.trim();

    for td in type_defaults {
        if let Ok(re) = Regex::new(&td.pattern) {
            if re.is_match(trimmed) {
                return td.value.clone();
            }
        }
    }

    fallback_default.to_string()
}
