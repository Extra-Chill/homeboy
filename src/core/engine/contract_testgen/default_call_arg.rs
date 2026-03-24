//! default_call_arg — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::SetupOverride;
use super::resolve_type_default;
use super::infer_hint_for_param;
use super::infer_setup_with_complements;
use super::super::contract::*;
use super::super::*;


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

/// Produce the default call argument for a parameter based on its type.
pub(crate) fn default_call_arg(name: &str, param_type: &str) -> String {
    if param_type.trim().starts_with('&') {
        format!("&{}", name)
    } else {
        name.to_string()
    }
}

/// Resolve a semantic hint + param type through the grammar's type_constructors.
///
/// Tries constructors in order; first match on both `hint` and `pattern` wins.
/// Falls back to `type_defaults` if no constructor matches, then to `fallback_default`.
///
/// The `{param_name}` placeholder in constructor values is replaced with the
/// actual parameter name.
pub(crate) fn resolve_constructor(
    hint: &str,
    param_name: &str,
    param_type: &str,
    constructors: &[TypeConstructor],
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> (String, String, Vec<String>) {
    // Split compound hints like "contains:foo" into base hint + argument
    let (base_hint, hint_arg) = if let Some(colon_pos) = hint.find(':') {
        (&hint[..colon_pos], Some(&hint[colon_pos + 1..]))
    } else {
        (hint, None)
    };

    // Try type_constructors first
    for tc in constructors {
        if tc.hint != base_hint {
            continue;
        }
        if let Ok(re) = Regex::new(&tc.pattern) {
            if re.is_match(param_type) {
                // Found a match — apply parameter name substitution
                let mut value = tc.value.replace("{param_name}", param_name);
                // For "contains" hints, also substitute the literal argument
                if let Some(arg) = hint_arg {
                    value = value.replace("{hint_arg}", arg);
                }

                let call_arg = tc
                    .call_arg
                    .as_ref()
                    .map(|c| c.replace("{param_name}", param_name))
                    .unwrap_or_else(|| default_call_arg(param_name, param_type));

                let imports: Vec<String> = tc.imports.to_vec();
                return (value, call_arg, imports);
            }
        }
    }

    // No constructor matched — fall back to type_defaults
    let (val, call_override, imps) =
        resolve_type_default(param_type, type_defaults, fallback_default);
    let call = call_override.unwrap_or_else(|| default_call_arg(param_name, param_type));
    let imp_strs: Vec<String> = imps.into_iter().map(|s| s.to_string()).collect();
    (val, call, imp_strs)
}
