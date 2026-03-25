//! resolve_type_default — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use serde::{Deserialize, Serialize};
use super::super::contract::*;


/// Build template variables from a contract and optional branch.
pub(crate) fn build_variables(
    contract: &FunctionContract,
    branch: Option<&Branch>,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    vars.insert("fn_name".to_string(), contract.name.clone());
    vars.insert("file".to_string(), contract.file.clone());
    vars.insert("line".to_string(), contract.line.to_string());

    // Build param list for function call
    let param_names: Vec<&str> = contract
        .signature
        .params
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    vars.insert("param_names".to_string(), param_names.join(", "));

    // Build typed param declarations
    let param_decls: Vec<String> = contract
        .signature
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.param_type))
        .collect();
    vars.insert("param_decls".to_string(), param_decls.join(", "));

    // Param count
    vars.insert(
        "param_count".to_string(),
        contract.signature.params.len().to_string(),
    );

    // Build param setup lines and call args using type_defaults
    let (setup_lines, call_args, extra_imports) =
        build_param_inputs(&contract.signature.params, type_defaults, fallback_default);
    vars.insert("param_setup".to_string(), setup_lines);
    vars.insert("param_args".to_string(), call_args);
    vars.insert("extra_imports".to_string(), extra_imports);

    // Return type info
    match &contract.signature.return_type {
        ReturnShape::Unit => vars.insert("return_shape".to_string(), "unit".to_string()),
        ReturnShape::Bool => vars.insert("return_shape".to_string(), "bool".to_string()),
        ReturnShape::Value { value_type } => {
            vars.insert("return_shape".to_string(), "value".to_string());
            vars.insert("return_type".to_string(), value_type.clone())
        }
        ReturnShape::OptionType { some_type } => {
            vars.insert("return_shape".to_string(), "option".to_string());
            vars.insert("some_type".to_string(), some_type.clone())
        }
        ReturnShape::ResultType { ok_type, err_type } => {
            vars.insert("return_shape".to_string(), "result".to_string());
            vars.insert("ok_type".to_string(), ok_type.clone());
            vars.insert("err_type".to_string(), err_type.clone())
        }
        ReturnShape::Collection { element_type } => {
            vars.insert("return_shape".to_string(), "collection".to_string());
            vars.insert("element_type".to_string(), element_type.clone())
        }
        ReturnShape::Unknown { raw } => {
            vars.insert("return_shape".to_string(), "unknown".to_string());
            vars.insert("return_type".to_string(), raw.clone())
        }
    };

    // Branch-specific variables
    if let Some(branch) = branch {
        vars.insert("variant".to_string(), branch.returns.variant.clone());
        if let Some(ref val) = branch.returns.value {
            vars.insert("expected_value".to_string(), val.clone());
        }
    }

    // Is it a method (has receiver)?
    let is_method = contract.signature.receiver.is_some();
    vars.insert("is_method".to_string(), is_method.to_string());
    vars.insert("is_pure".to_string(), contract.is_pure().to_string());

    // Method receiver support: impl_type and receiver construction
    if let Some(ref impl_type) = contract.impl_type {
        vars.insert("impl_type".to_string(), impl_type.clone());

        // Determine receiver mutability for the let binding
        let receiver_mut = match &contract.signature.receiver {
            Some(Receiver::MutRef) => "mut ",
            _ => "",
        };
        vars.insert("receiver_mut".to_string(), receiver_mut.to_string());

        // Build receiver setup line. The grammar's fallback_default is "Default::default()"
        // which would produce "Type::Default::default()" — wrong. We need "Type::default()".
        let construction = if fallback_default == "Default::default()" {
            "default()".to_string()
        } else {
            fallback_default.to_string()
        };
        let receiver_setup = format!(
            "        let {}instance = {}::{};",
            receiver_mut, impl_type, construction
        );
        vars.insert("receiver_setup".to_string(), receiver_setup.clone());

        // Override param_setup to include receiver construction
        let existing_setup = vars.get("param_setup").cloned().unwrap_or_default();
        let combined_setup = if existing_setup.trim().is_empty() {
            receiver_setup.clone()
        } else {
            format!("{}\n{}", receiver_setup, existing_setup)
        };
        vars.insert("param_setup".to_string(), combined_setup);

        // Override fn_name to use method call syntax: instance.method_name
        vars.insert("fn_name".to_string(), format!("instance.{}", contract.name));
    };
    vars.insert(
        "branch_count".to_string(),
        contract.branch_count().to_string(),
    );

    vars
}

/// Resolve a default value expression for a parameter type using type_defaults patterns.
///
/// Returns `(value_expr, call_arg_expr, imports)` where:
/// - `value_expr` is the `let` binding right-hand side (e.g., `String::new()`)
/// - `call_arg_expr` is what to pass in the function call (e.g., `&name` for `&str` params)
/// - `imports` are any extra `use` statements needed
pub(crate) fn resolve_type_default<'a>(
    param_type: &str,
    type_defaults: &'a [TypeDefault],
    fallback_default: &str,
) -> (String, Option<String>, Vec<&'a str>) {
    for td in type_defaults {
        if let Ok(re) = Regex::new(&td.pattern) {
            if re.is_match(param_type) {
                let imports: Vec<&str> = td.imports.iter().map(|s| s.as_str()).collect();
                return (td.value.clone(), None, imports);
            }
        }
    }
    // Fallback: language-specific default from grammar
    (fallback_default.to_string(), None, vec![])
}

/// Build parameter setup lines, call arguments, and extra imports from type_defaults.
///
/// Returns `(setup_lines, call_args, extra_imports)` where:
/// - `setup_lines` is newline-separated `let` bindings
/// - `call_args` is comma-separated arguments for the function call
/// - `extra_imports` is newline-separated `use` statements
pub(crate) fn build_param_inputs(
    params: &[Param],
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> (String, String, String) {
    if params.is_empty() {
        return (String::new(), String::new(), String::new());
    }

    let mut setup_lines = Vec::new();
    let mut call_args = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();

    for param in params {
        let (value_expr, call_override, imports) =
            resolve_type_default(&param.param_type, type_defaults, fallback_default);

        // Build the let binding
        setup_lines.push(format!("        let {} = {};", param.name, value_expr));

        // Build the call argument — if the type is a reference, borrow the variable
        let call_arg = call_override.unwrap_or_else(|| {
            let trimmed = param.param_type.trim();
            if trimmed.starts_with('&') {
                format!("&{}", param.name)
            } else {
                param.name.clone()
            }
        });
        call_args.push(call_arg);

        for imp in imports {
            let imp_string = imp.to_string();
            if !all_imports.contains(&imp_string) {
                all_imports.push(imp_string);
            }
        }
    }

    let setup = setup_lines.join("\n");
    let args = call_args.join(", ");
    let imports = all_imports.join("\n");
    (setup, args, imports)
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
