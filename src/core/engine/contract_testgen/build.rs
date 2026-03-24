//! build — extracted from contract_testgen.rs.

use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::resolve_type_default;
use super::super::contract::*;
use super::super::*;


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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_variables_if_let_some_branch_branch() {

        let result = build_variables();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(branch) = branch {{");
    }

    #[test]
    fn test_build_variables_let_some_branch_branch() {

        let result = build_variables();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(branch) = branch");
    }

    #[test]
    fn test_build_variables_if_let_some_ref_impl_type_contract_impl_type() {

        let result = build_variables();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(ref impl_type) = contract.impl_type {{");
    }

    #[test]
    fn test_build_variables_let_some_ref_impl_type_contract_impl_type() {

        let result = build_variables();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(ref impl_type) = contract.impl_type");
    }

    #[test]
    fn test_build_variables_has_expected_effects() {
        // Expected effects: mutation

        let _ = build_variables();
    }

    #[test]
    fn test_build_param_inputs_default_path() {

        let _result = build_param_inputs();
    }

    #[test]
    fn test_build_param_inputs_has_expected_effects() {
        // Expected effects: mutation

        let _ = build_param_inputs();
    }

}
