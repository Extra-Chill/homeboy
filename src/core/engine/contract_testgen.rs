//! Test plan generation from function contracts.
//!
//! Takes a `FunctionContract` and produces a `TestPlan` — a structured
//! description of what tests to generate. The plan is language-agnostic.
//!
//! Rendering the plan into actual source code uses templates from grammar.toml
//! `[contract.test_templates]`. Core fills in the variables, the grammar
//! provides the syntax.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::contract::*;

/// A plan for generating tests for a single function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    /// The function being tested.
    pub function_name: String,
    /// Source file containing the function.
    pub source_file: String,
    /// Individual test cases to generate.
    pub cases: Vec<TestCase>,
}

/// A single test case to generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Suggested test function name (e.g., "test_validate_write_skips_when_empty").
    pub test_name: String,
    /// Which branch this test covers.
    pub branch_condition: String,
    /// The expected return variant (ok, err, some, none, true, false, value).
    pub expected_variant: String,
    /// Description of what the expected return value should be.
    pub expected_value: Option<String>,
    /// The template key to use for rendering (e.g., "result_ok", "option_none", "bool_true").
    pub template_key: String,
    /// Template variables for rendering.
    pub variables: HashMap<String, String>,
}

/// Generate a test plan from a function contract.
///
/// Produces one test case per branch. The plan is language-agnostic —
/// rendering to source code requires templates from the grammar.
pub fn generate_test_plan(contract: &FunctionContract) -> TestPlan {
    let mut cases = Vec::new();

    if contract.branches.is_empty() {
        // No branches detected — generate a basic "does not panic" test
        cases.push(TestCase {
            test_name: format!("test_{}_does_not_panic", contract.name),
            branch_condition: "default invocation".to_string(),
            expected_variant: "no_panic".to_string(),
            expected_value: None,
            template_key: "no_panic".to_string(),
            variables: build_variables(contract, None),
        });
    } else {
        for (i, branch) in contract.branches.iter().enumerate() {
            let condition_slug = slugify(&branch.condition);
            let test_name = format!(
                "test_{}_{}",
                contract.name,
                if condition_slug.is_empty() {
                    format!("branch_{}", i)
                } else {
                    condition_slug
                }
            );

            let template_key =
                derive_template_key(&contract.signature.return_type, &branch.returns);

            let mut vars = build_variables(contract, Some(branch));
            vars.insert("condition".to_string(), branch.condition.clone());
            vars.insert("condition_slug".to_string(), slugify(&branch.condition));

            cases.push(TestCase {
                test_name,
                branch_condition: branch.condition.clone(),
                expected_variant: branch.returns.variant.clone(),
                expected_value: branch.returns.value.clone(),
                template_key,
                variables: vars,
            });
        }
    }

    // If the function has effects, generate an effect-specific test
    if contract.has_effects() && !contract.effects.is_empty() {
        let effect_names: Vec<&str> = contract
            .effects
            .iter()
            .map(|e| match e {
                Effect::FileRead => "file_read",
                Effect::FileWrite => "file_write",
                Effect::FileDelete => "file_delete",
                Effect::ProcessSpawn { .. } => "process_spawn",
                Effect::Mutation { .. } => "mutation",
                Effect::Panic { .. } => "panic",
                Effect::Network => "network",
                Effect::ResourceAlloc { .. } => "resource_alloc",
                Effect::Logging => "logging",
            })
            .collect();

        let mut vars = build_variables(contract, None);
        vars.insert("effects".to_string(), effect_names.join(", "));

        cases.push(TestCase {
            test_name: format!("test_{}_has_expected_effects", contract.name),
            branch_condition: "effect verification".to_string(),
            expected_variant: "effects".to_string(),
            expected_value: Some(effect_names.join(", ")),
            template_key: "effects".to_string(),
            variables: vars,
        });
    }

    TestPlan {
        function_name: contract.name.clone(),
        source_file: contract.file.clone(),
        cases,
    }
}

/// Render a test plan into source code using templates.
///
/// Templates are key → string pairs where keys match `TestCase.template_key`.
/// Template variables are replaced: `{fn_name}`, `{fn_call}`, `{param_list}`, etc.
pub fn render_test_plan(plan: &TestPlan, templates: &HashMap<String, String>) -> String {
    let mut output = String::new();

    for case in &plan.cases {
        let template = match templates.get(&case.template_key) {
            Some(t) => t,
            None => {
                // Fall back to a generic template if the specific one doesn't exist
                match templates.get("default") {
                    Some(t) => t,
                    None => continue,
                }
            }
        };

        let mut rendered = template.clone();
        for (key, value) in &case.variables {
            rendered = rendered.replace(&format!("{{{}}}", key), value);
        }
        // Also replace the test name
        rendered = rendered.replace("{test_name}", &case.test_name);

        output.push_str(&rendered);
        output.push('\n');
    }

    output
}

/// Build template variables from a contract and optional branch.
fn build_variables(
    contract: &FunctionContract,
    branch: Option<&Branch>,
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
    vars.insert(
        "is_method".to_string(),
        contract.signature.receiver.is_some().to_string(),
    );
    vars.insert("is_pure".to_string(), contract.is_pure().to_string());
    vars.insert(
        "branch_count".to_string(),
        contract.branch_count().to_string(),
    );

    vars
}

/// Derive the template key from the return type shape and the branch's return variant.
fn derive_template_key(return_type: &ReturnShape, returns: &ReturnValue) -> String {
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
fn slugify(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result_contract() -> FunctionContract {
        FunctionContract {
            name: "validate_write".to_string(),
            file: "src/core/engine/validate_write.rs".to_string(),
            line: 86,
            signature: Signature {
                params: vec![
                    Param {
                        name: "root".to_string(),
                        param_type: "&Path".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                    Param {
                        name: "changed_files".to_string(),
                        param_type: "&[PathBuf]".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                ],
                return_type: ReturnShape::ResultType {
                    ok_type: "ValidationResult".to_string(),
                    err_type: "Error".to_string(),
                },
                receiver: None,
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "changed_files.is_empty()".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("skipped".to_string()),
                    },
                    effects: vec![],
                    line: Some(91),
                },
                Branch {
                    condition: "validation command fails".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("failed".to_string()),
                    },
                    effects: vec![Effect::ProcessSpawn {
                        command: Some("sh".to_string()),
                    }],
                    line: Some(130),
                },
            ],
            early_returns: 2,
            effects: vec![Effect::ProcessSpawn {
                command: Some("sh".to_string()),
            }],
            calls: vec![],
        }
    }

    fn sample_option_contract() -> FunctionContract {
        FunctionContract {
            name: "find_item".to_string(),
            file: "src/lib.rs".to_string(),
            line: 10,
            signature: Signature {
                params: vec![Param {
                    name: "key".to_string(),
                    param_type: "&str".to_string(),
                    mutable: false,
                    has_default: false,
                }],
                return_type: ReturnShape::OptionType {
                    some_type: "Item".to_string(),
                },
                receiver: Some(Receiver::Ref),
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "key found".to_string(),
                    returns: ReturnValue {
                        variant: "some".to_string(),
                        value: Some("item".to_string()),
                    },
                    effects: vec![],
                    line: Some(15),
                },
                Branch {
                    condition: "key not found".to_string(),
                    returns: ReturnValue {
                        variant: "none".to_string(),
                        value: None,
                    },
                    effects: vec![],
                    line: Some(20),
                },
            ],
            early_returns: 0,
            effects: vec![],
            calls: vec![],
        }
    }

    #[test]
    fn test_plan_generates_one_case_per_branch() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract);
        // 2 branches + 1 effect test
        assert_eq!(plan.cases.len(), 3);
        assert_eq!(plan.function_name, "validate_write");
    }

    #[test]
    fn test_plan_names_are_descriptive() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract);
        assert!(plan.cases[0].test_name.starts_with("test_validate_write_"));
        assert!(plan.cases[0].test_name.contains("empty"));
    }

    #[test]
    fn test_plan_template_keys_match_return_shape() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract);
        assert_eq!(plan.cases[0].template_key, "result_ok");
        assert_eq!(plan.cases[1].template_key, "result_ok");
    }

    #[test]
    fn test_plan_for_option_type() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract);
        assert_eq!(plan.cases.len(), 2);
        assert_eq!(plan.cases[0].template_key, "option_some");
        assert_eq!(plan.cases[1].template_key, "option_none");
    }

    #[test]
    fn test_plan_pure_function_no_effect_test() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract);
        // Pure function — no effect test case
        assert!(plan.cases.iter().all(|c| c.template_key != "effects"));
    }

    #[test]
    fn test_plan_variables_contain_fn_info() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract);
        let vars = &plan.cases[0].variables;
        assert_eq!(vars.get("fn_name").unwrap(), "validate_write");
        assert_eq!(vars.get("param_names").unwrap(), "root, changed_files");
        assert_eq!(vars.get("return_shape").unwrap(), "result");
    }

    #[test]
    fn test_render_with_templates() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract);

        let mut templates = HashMap::new();
        templates.insert(
            "result_ok".to_string(),
            "#[test]\nfn {test_name}() {{\n    // {fn_name} should return Ok\n}}\n".to_string(),
        );

        let output = render_test_plan(&plan, &templates);
        assert!(output.contains("#[test]"));
        assert!(output.contains("test_validate_write_"));
        assert!(output.contains("validate_write should return Ok"));
    }

    #[test]
    fn test_render_missing_template_uses_default() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract);

        let mut templates = HashMap::new();
        templates.insert(
            "default".to_string(),
            "#[test]\nfn {test_name}() {{ todo!() }}\n".to_string(),
        );

        let output = render_test_plan(&plan, &templates);
        assert!(output.contains("test_find_item_"));
        assert!(output.contains("todo!()"));
    }

    #[test]
    fn test_slugify() {
        assert_eq!(
            slugify("changed_files.is_empty()"),
            "changed_files_is_empty"
        );
        assert_eq!(
            slugify("validation command fails"),
            "validation_command_fails"
        );
        assert_eq!(slugify("default path"), "default_path");
    }

    #[test]
    fn test_empty_branches_generates_no_panic_test() {
        let mut contract = sample_result_contract();
        contract.branches.clear();
        contract.effects.clear();
        let plan = generate_test_plan(&contract);
        assert_eq!(plan.cases.len(), 1);
        assert_eq!(plan.cases[0].template_key, "no_panic");
    }
}
