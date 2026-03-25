//! helpers — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::TRUE;
use super::FALSE;
use super::CONTAINS;
use super::is_numeric_like;
use super::EMPTY;
use super::EXISTENT_PATH;
use super::SOME_DEFAULT;
use super::POSITIVE;
use super::is_path_like;
use super::NON_EMPTY;
use super::NONE;
use super::ZERO;
use super::NONEXISTENT_PATH;
use super::super::contract::*;


/// Derive the template key from the return type shape and the branch's return variant.
pub(crate) fn derive_template_key(return_type: &ReturnShape, returns: &ReturnValue) -> String {
    match return_type {
        ReturnShape::ResultType { .. } => format!("result_{}", returns.variant),
        ReturnShape::OptionType { .. } => format!("option_{}", returns.variant),
        ReturnShape::Bool => format!("bool_{}", returns.variant),
        ReturnShape::Unit => "unit".to_string(),
        ReturnShape::Collection { .. } => "collection".to_string(),
        _ => format!("value_{}", returns.variant),
    }
}

/// Convert a condition string to a snake_case slug suitable for a test name.
pub(crate) fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '.' || c == ':' || c == '-' || c == '_' {
                '_'
            } else {
                '_'
            }
        })
        .collect::<String>()
        // Collapse multiple underscores
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
        // Truncate to reasonable length
        .chars()
        .take(60)
        .collect()
}

/// Analyze a branch condition to produce a semantic hint for a parameter.
///
/// This is the core of behavioral inference — it recognizes common condition
/// patterns and maps them to language-agnostic hints. The hints are then
/// resolved through the grammar's `type_constructors` to get actual code.
///
/// Returns `None` if no hint can be inferred for this parameter.
pub(crate) fn infer_hint_for_param(condition: &str, condition_lower: &str, param: &Param) -> Option<String> {
    let pname = &param.name;
    let ptype = &param.param_type;

    // ── Negated emptiness — check BEFORE non-negated to avoid false matches ──
    if condition_contains_negated_method(condition, pname, "is_empty") {
        return Some(hints::NON_EMPTY.to_string());
    }

    // ── Emptiness: "param.is_empty()" ──
    if condition_contains_param_method(condition_lower, pname, "is_empty") {
        return Some(hints::EMPTY.to_string());
    }

    // ── Option: "param.is_none()" ──
    if (condition_contains_param_method(condition_lower, pname, "is_none")
        || (condition_lower.contains(&pname.to_lowercase()) && condition_lower.contains("none")))
        && ptype.starts_with("Option")
    {
        return Some(hints::NONE.to_string());
    }

    // ── Option: "param.is_some()" ──
    if (condition_contains_param_method(condition_lower, pname, "is_some")
        || (condition_lower.contains(&pname.to_lowercase()) && condition_lower.contains("some")))
        && ptype.starts_with("Option")
    {
        return Some(hints::SOME_DEFAULT.to_string());
    }

    // ── Path existence ──
    if is_path_like(ptype) {
        if condition_lower.contains("doesn't exist")
            || condition_lower.contains("does not exist")
            || condition_lower.contains("not exist")
            || condition_contains_negated_method(condition, pname, "exists")
        {
            return Some(hints::NONEXISTENT_PATH.to_string());
        }
        if condition_contains_param_method(condition_lower, pname, "exists")
            && !condition_lower.contains("not")
            && !condition.contains('!')
        {
            return Some(hints::EXISTENT_PATH.to_string());
        }
    }

    // ── Boolean params ──
    if ptype.trim() == "bool" {
        if condition_lower.contains(&format!("!{}", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} == false", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} is false", pname.to_lowercase()))
        {
            return Some(hints::FALSE.to_string());
        }
        if condition_lower == pname.to_lowercase()
            || condition_lower.contains(&format!("{} == true", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} is true", pname.to_lowercase()))
        {
            return Some(hints::TRUE.to_string());
        }
    }

    // ── Numeric comparisons ──
    if is_numeric_like(ptype) {
        if condition_lower.contains(&format!("{} == 0", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} < 1", pname.to_lowercase()))
        {
            return Some(hints::ZERO.to_string());
        }
        if condition_lower.contains(&format!("{} > 0", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} >= 1", pname.to_lowercase()))
        {
            return Some(hints::POSITIVE.to_string());
        }
    }

    // ── String content: ".contains(X)" or ".starts_with(X)" ──
    if let Some(literal) = extract_method_string_arg(condition, pname, "contains") {
        // Store the literal in the hint using a separator
        return Some(format!("{}:{}", hints::CONTAINS, literal));
    }
    if let Some(literal) = extract_method_string_arg(condition, pname, "starts_with") {
        return Some(format!("{}:{}", hints::CONTAINS, literal));
    }

    None
}

/// Extract a string literal argument from a method call in a condition.
///
/// E.g., from `name.contains("foo")` extracts `"foo"`.
pub(crate) fn extract_method_string_arg(condition: &str, param: &str, method: &str) -> Option<String> {
    let pattern = format!("{}.{}(\"", param, method);
    if let Some(start) = condition.find(&pattern) {
        let after = &condition[start + pattern.len()..];
        if let Some(end) = after.find('"') {
            return Some(after[..end].to_string());
        }
    }
    // Also try single-quote variant
    let pattern_sq = format!("{}.{}('", param, method);
    if let Some(start) = condition.find(&pattern_sq) {
        let after = &condition[start + pattern_sq.len()..];
        if let Some(end) = after.find('\'') {
            return Some(after[..end].to_string());
        }
    }
    None
}
