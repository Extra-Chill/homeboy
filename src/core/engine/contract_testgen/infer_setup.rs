//! infer_setup — extracted from contract_testgen.rs.

use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use regex::Regex;
use serde::{Deserialize, Serialize};
use super::SetupOverride;
use super::resolve_type_default;
use super::default_call_arg;
use super::resolve_constructor;
use super::super::contract::*;


/// Infer parameter setup code from a branch condition string (without cross-branch complements).
///
/// Delegates to `infer_setup_with_complements` with no complement hints.
/// Used in tests and simple single-branch scenarios.
#[cfg(test)]
pub(crate) fn infer_setup_from_condition(
    condition: &str,
    params: &[Param],
    type_defaults: &[TypeDefault],
    type_constructors: &[TypeConstructor],
    fallback_default: &str,
) -> Option<SetupOverride> {
    let condition_lower = condition.to_lowercase();

    // Step 1: Produce semantic hints for each parameter
    let mut param_hints: HashMap<String, String> = HashMap::new();
    for param in params {
        if let Some(hint) = infer_hint_for_param(condition, &condition_lower, param) {
            param_hints.insert(param.name.clone(), hint);
        }
    }

    if param_hints.is_empty() {
        return None;
    }

    // Step 2: Resolve hints through grammar constructors
    let mut setup_lines = Vec::new();
    let mut call_args = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();

    for param in params {
        let (value_expr, call_arg, imports) = if let Some(hint) = param_hints.get(&param.name) {
            resolve_constructor(
                hint,
                &param.name,
                &param.param_type,
                type_constructors,
                type_defaults,
                fallback_default,
            )
        } else {
            // No hint for this param — use type_defaults
            let (val, call_override, imps) =
                resolve_type_default(&param.param_type, type_defaults, fallback_default);
            let call =
                call_override.unwrap_or_else(|| default_call_arg(&param.name, &param.param_type));
            let imp_strs: Vec<String> = imps.into_iter().map(|s| s.to_string()).collect();
            (val, call, imp_strs)
        };

        setup_lines.push(format!("        let {} = {};", param.name, value_expr));
        call_args.push(call_arg);

        for imp in imports {
            if !all_imports.contains(&imp) {
                all_imports.push(imp);
            }
        }
    }

    Some(SetupOverride {
        setup_lines: setup_lines.join("\n"),
        call_args: call_args.join(", "),
        extra_imports: all_imports.join("\n"),
    })
}

/// Like `infer_setup_from_condition` but also applies complement hints
/// for params that aren't matched by the current condition.
pub(crate) fn infer_setup_with_complements(
    condition: &str,
    params: &[Param],
    type_defaults: &[TypeDefault],
    type_constructors: &[TypeConstructor],
    fallback_default: &str,
    complement_hints: &HashMap<String, String>,
) -> Option<SetupOverride> {
    let condition_lower = condition.to_lowercase();

    // Step 1: Produce direct hints from this branch's condition
    let mut param_hints: HashMap<String, String> = HashMap::new();
    for param in params {
        if let Some(hint) = infer_hint_for_param(condition, &condition_lower, param) {
            param_hints.insert(param.name.clone(), hint);
        }
    }

    // Step 2: Apply complement hints for params not directly matched
    for (param_name, complement) in complement_hints {
        if !param_hints.contains_key(param_name) {
            param_hints.insert(param_name.clone(), complement.clone());
        }
    }

    if param_hints.is_empty() {
        return None;
    }

    // Step 3: Resolve all hints through grammar constructors
    let mut setup_lines = Vec::new();
    let mut call_args = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();

    for param in params {
        let (value_expr, call_arg, imports) = if let Some(hint) = param_hints.get(&param.name) {
            resolve_constructor(
                hint,
                &param.name,
                &param.param_type,
                type_constructors,
                type_defaults,
                fallback_default,
            )
        } else {
            let (val, call_override, imps) =
                resolve_type_default(&param.param_type, type_defaults, fallback_default);
            let call =
                call_override.unwrap_or_else(|| default_call_arg(&param.name, &param.param_type));
            let imp_strs: Vec<String> = imps.into_iter().map(|s| s.to_string()).collect();
            (val, call, imp_strs)
        };

        setup_lines.push(format!("        let {} = {};", param.name, value_expr));
        call_args.push(call_arg);

        for imp in imports {
            if !all_imports.contains(&imp) {
                all_imports.push(imp);
            }
        }
    }

    Some(SetupOverride {
        setup_lines: setup_lines.join("\n"),
        call_args: call_args.join(", "),
        extra_imports: all_imports.join("\n"),
    })
}
