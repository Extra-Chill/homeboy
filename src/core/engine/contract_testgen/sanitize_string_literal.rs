//! sanitize_string_literal — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::super::contract::*;


/// Resolve an assertion for a branch return using grammar-defined assertion templates.
///
/// Core selects an assertion key based on the return type and variant. The grammar's
/// `assertion_templates` section provides language-specific assertion code for each key.
/// Falls back to simple variant-check assertions if no template is found.
pub(crate) fn resolve_assertion(
    returns: &ReturnValue,
    return_type: &ReturnShape,
    condition: &str,
    assertion_templates: &HashMap<String, String>,
) -> String {
    let indent = "        ";

    // Determine the assertion key based on return type + variant + whether we have a value
    let has_value = returns.value.is_some();
    let variant = returns.variant.as_str();

    let key = match return_type {
        ReturnShape::ResultType { .. } => {
            if has_value {
                format!("result_{}_value", variant)
            } else {
                format!("result_{}", variant)
            }
        }
        ReturnShape::OptionType { .. } => {
            if has_value {
                format!("option_{}_value", variant)
            } else {
                format!("option_{}", variant)
            }
        }
        ReturnShape::Bool => format!("bool_{}", variant),
        ReturnShape::Collection { .. } => {
            if condition.contains("empty") || condition.contains("is_empty") {
                "collection_empty".to_string()
            } else {
                "collection_non_empty".to_string()
            }
        }
        _ => "value_default".to_string(),
    };

    // Try the specific key first, then fall back to the base key (without _value)
    let template = assertion_templates.get(&key).or_else(|| {
        // Fall back: result_ok_value → result_ok
        let base = key.rsplit_once('_').map(|(base, _)| base.to_string());
        base.and_then(|b| assertion_templates.get(&b))
    });

    if let Some(tmpl) = template {
        // Substitute variables in the assertion template.
        // Sanitize condition text for embedding inside Rust string literals.
        // Source-level conditions can contain quotes, backticks, and braces
        // (e.g. `Some(format!("```{} block", language))`) that break generated
        // assert messages. Escape quotes and replace braces to avoid format
        // string interpretation in the outer assert! macro.
        let mut rendered = tmpl.clone();
        rendered = rendered.replace("{condition}", &sanitize_for_string_literal(condition));
        if let Some(ref val) = returns.value {
            rendered = rendered.replace("{expected_value}", &val.replace('"', "\\\""));
        }
        rendered = rendered.replace("{variant}", variant);
        rendered
    } else {
        // No grammar template — produce a minimal language-agnostic placeholder
        let escaped_condition = sanitize_for_string_literal(condition);
        format!("{indent}let _ = result; // {variant}: {escaped_condition}")
    }
}

/// When a value-level assertion template couldn't be enriched (type not in registry),
/// fall back to the simpler base assertion that tests the discriminant only.
///
/// For example: `result_ok_value` (has TODO placeholder) → `result_ok` (asserts is_ok()).
/// This produces a real test instead of a dead stub.
pub(crate) fn fallback_to_simple_assertion(
    returns: &ReturnValue,
    return_type: &ReturnShape,
    condition: &str,
    assertion_templates: &HashMap<String, String>,
) -> Option<String> {
    let variant = returns.variant.as_str();

    // Build the base key (without _value suffix)
    let base_key = match return_type {
        ReturnShape::ResultType { .. } => format!("result_{}", variant),
        ReturnShape::OptionType { .. } => format!("option_{}", variant),
        _ => return None, // Bool/Collection/etc don't have _value variants
    };

    let tmpl = assertion_templates.get(&base_key)?;

    let mut rendered = tmpl.clone();
    rendered = rendered.replace("{condition}", &sanitize_for_string_literal(condition));
    if let Some(ref val) = returns.value {
        rendered = rendered.replace("{expected_value}", &val.replace('"', "\\\""));
    }
    rendered = rendered.replace("{variant}", variant);
    Some(rendered)
}

/// Sanitize a source-level string for safe embedding inside a Rust string literal.
///
/// Escapes double quotes and replaces curly braces with Unicode lookalikes
/// so the text doesn't interfere with `format!` / `assert!` macro parsing.
/// Backticks in groups of 3+ are replaced to avoid raw string prefix confusion.
pub(crate) fn sanitize_for_string_literal(s: &str) -> String {
    s.replace('"', "\\\"")
        .replace('{', "{{")
        .replace('}', "}}")
        .replace("```", "'''")
}
