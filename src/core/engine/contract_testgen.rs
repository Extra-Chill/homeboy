//! Test plan generation from function contracts.
//!
//! Takes a `FunctionContract` and produces a `TestPlan` — a structured
//! description of what tests to generate. The plan is language-agnostic.
//!
//! Rendering the plan into actual source code uses templates from grammar.toml
//! `[contract.test_templates]`. Core fills in the variables, the grammar
//! provides the syntax.
//!
//! ## Behavioral inference
//!
//! The test plan generator analyzes branch conditions and return values to
//! produce **behavioral** tests rather than smoke tests. For each branch:
//!
//! 1. **Setup inference** — pattern-matches the condition string against the
//!    parameter types to derive input values that trigger the branch. E.g.,
//!    `"commits.is_empty()"` with a `Vec<CommitInfo>` param → pass `vec![]`.
//!
//! 2. **Assertion inference** — uses the return variant and value description
//!    to derive specific assertions. E.g., `Ok("skipped")` → assert the
//!    unwrapped value matches the expected description, not just `is_ok()`.

mod complement_hint;
mod condition_contains;
mod constants;
mod default_field_type;
mod generate_test;
mod helpers;
mod infer_setup;
mod like;
mod render_test_plan;
mod resolve_type_default;
mod sanitize_string_literal;
mod types;

pub use complement_hint::*;
pub use condition_contains::*;
pub use constants::*;
pub use default_field_type::*;
pub use generate_test::*;
pub use helpers::*;
pub use infer_setup::*;
pub use like::*;
pub use render_test_plan::*;
pub use resolve_type_default::*;
pub use sanitize_string_literal::*;
pub use types::*;


use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::contract::*;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};

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

// ── Behavioral inference ──
//
// Core analyzes branch conditions to produce **semantic hints** — language-agnostic
// descriptions of what a parameter value should be (e.g., "empty", "none",
// "nonexistent_path"). The grammar's `type_constructors` section then maps
// each (hint, type_pattern) pair to a language-specific code expression.
//
// This keeps core completely language-agnostic. Adding a new language is just
// adding a new grammar file — no core changes needed.

/// Well-known semantic hints that core can produce from condition analysis.
/// These are the "vocabulary" between core and grammar extensions.
///
/// Extensions define `[[contract.type_constructors]]` entries with `hint` fields
/// matching these strings. Extensions may also define custom hints.
mod hints {
}

// ── Type registry ──

// ── End-to-end API ──

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
            impl_type: None,
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
            impl_type: None,
        }
    }

    fn sample_type_defaults() -> Vec<TypeDefault> {
        vec![
            TypeDefault {
                pattern: r"^&str$".to_string(),
                value: r#""""#.to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^&Path$".to_string(),
                value: r#"Path::new("")"#.to_string(),
                imports: vec!["use std::path::Path;".to_string()],
            },
            TypeDefault {
                pattern: r"^&\[.*\]$".to_string(),
                value: "Vec::new()".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^bool$".to_string(),
                value: "false".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^Option<.*>$".to_string(),
                value: "None".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^usize$|^u\d+$|^i\d+$".to_string(),
                value: "0".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^String$".to_string(),
                value: "String::new()".to_string(),
                imports: vec![],
            },
        ]
    }

    /// Build a minimal ContractGrammar for testing.
    fn empty_grammar() -> ContractGrammar {
        ContractGrammar::default()
    }

    /// Build a ContractGrammar with type_defaults populated.
    fn grammar_with_defaults() -> ContractGrammar {
        ContractGrammar {
            type_defaults: sample_type_defaults(),
            ..Default::default()
        }
    }

    /// Build a ContractGrammar with type_defaults + type_constructors + assertion_templates + test_templates.
    fn full_grammar() -> ContractGrammar {
        ContractGrammar {
            type_defaults: sample_type_defaults(),
            type_constructors: sample_type_constructors(),
            assertion_templates: sample_assertion_templates(),
            test_templates: sample_test_templates(),
            field_assertion_template: Some(
                "{indent}assert_eq!(inner.{field_name}, {expected_value});".to_string(),
            ),
            ..Default::default()
        }
    }

    fn sample_test_templates() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("result_ok".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("result_err".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("option_some".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("option_none".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("bool_true".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("bool_false".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("no_panic".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let _ = {fn_name}({param_args});\n    }}\n".to_string());
        m.insert("effects".to_string(),
            "    #[test]\n    fn {test_name}() {{\n        // Expected effects: {effects}\n{param_setup}\n        let _ = {fn_name}({param_args});\n    }}\n".to_string());
        m.insert("default".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let _result = {fn_name}({param_args});\n    }}\n".to_string());
        m
    }

    fn sample_type_constructors() -> Vec<TypeConstructor> {
        vec![
            TypeConstructor {
                hint: "empty".to_string(),
                pattern: r"^&?\[.*\]$|^Vec<.*>$".to_string(),
                value: "Vec::new()".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "empty".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: r#""""#.to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "non_empty".to_string(),
                pattern: r"^&?\[.*\]$|^Vec<.*>$".to_string(),
                value: "vec![Default::default()]".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "non_empty".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: "\"test_{param_name}\"".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "nonexistent_path".to_string(),
                pattern: r"(?i)path".to_string(),
                value: r#"Path::new("/tmp/nonexistent_test_path_818")"#.to_string(),
                call_arg: None,
                imports: vec!["use std::path::Path;".to_string()],
            },
            TypeConstructor {
                hint: "none".to_string(),
                pattern: r"^Option<.*>$".to_string(),
                value: "None".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "some_default".to_string(),
                pattern: r"^Option<.*>$".to_string(),
                value: "Some(Default::default())".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "true".to_string(),
                pattern: r"^bool$".to_string(),
                value: "true".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "false".to_string(),
                pattern: r"^bool$".to_string(),
                value: "false".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "zero".to_string(),
                pattern: r"^u\w+$|^i\w+$|^f\w+$".to_string(),
                value: "0".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "positive".to_string(),
                pattern: r"^u\w+$|^i\w+$|^f\w+$".to_string(),
                value: "1".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "contains".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: "\"{hint_arg}\"".to_string(),
                call_arg: None,
                imports: vec![],
            },
        ]
    }

    fn sample_assertion_templates() -> HashMap<String, String> {
        let indent = "        ";
        let mut m = HashMap::new();
        m.insert(
            "result_ok".to_string(),
            format!("{indent}assert!(result.is_ok(), \"expected Ok for: {{condition}}\");"),
        );
        m.insert(
            "result_ok_value".to_string(),
            format!(
                "{indent}let inner = result.unwrap();\n\
             {indent}// Branch returns Ok({{expected_value}}) when: {{condition}}\n\
             {indent}let _ = inner; // TODO: assert specific value for \"{{expected_value}}\""
            ),
        );
        m.insert(
            "result_err".to_string(),
            format!("{indent}assert!(result.is_err(), \"expected Err for: {{condition}}\");"),
        );
        m.insert(
            "result_err_value".to_string(),
            format!(
                "{indent}let err = result.unwrap_err();\n\
             {indent}// Branch returns Err({{expected_value}}) when: {{condition}}\n\
             {indent}let err_msg = format!(\"{{{{:?}}}}\", err);\n\
             {indent}let _ = err_msg; // TODO: assert error contains \"{{expected_value}}\""
            ),
        );
        m.insert(
            "option_some".to_string(),
            format!("{indent}assert!(result.is_some(), \"expected Some for: {{condition}}\");"),
        );
        m.insert(
            "option_some_value".to_string(),
            format!(
                "{indent}let inner = result.expect(\"expected Some for: {{condition}}\");\n\
             {indent}// Branch returns Some({{expected_value}})\n\
             {indent}let _ = inner; // TODO: assert value matches \"{{expected_value}}\""
            ),
        );
        m.insert(
            "option_none".to_string(),
            format!("{indent}assert!(result.is_none(), \"expected None for: {{condition}}\");"),
        );
        m.insert(
            "bool_true".to_string(),
            format!("{indent}assert!(result, \"expected true when: {{condition}}\");"),
        );
        m.insert(
            "bool_false".to_string(),
            format!("{indent}assert!(!result, \"expected false when: {{condition}}\");"),
        );
        m.insert(
            "collection_empty".to_string(),
            format!(
            "{indent}assert!(result.is_empty(), \"expected empty collection for: {{condition}}\");"
        ),
        );
        m.insert("collection_non_empty".to_string(), format!(
            "{indent}assert!(!result.is_empty(), \"expected non-empty collection for: {{condition}}\");"
        ));
        m
    }

    #[test]
    fn test_plan_generates_one_case_per_branch() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        // 2 branches + 1 effect test
        assert_eq!(plan.cases.len(), 3);
        assert_eq!(plan.function_name, "validate_write");
    }

    #[test]
    fn test_plan_names_are_descriptive() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert!(plan.cases[0].test_name.starts_with("test_validate_write_"));
        assert!(plan.cases[0].test_name.contains("empty"));
    }

    #[test]
    fn test_plan_template_keys_match_return_shape() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases[0].template_key, "result_ok");
        assert_eq!(plan.cases[1].template_key, "result_ok");
    }

    #[test]
    fn test_plan_for_option_type() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases.len(), 2);
        assert_eq!(plan.cases[0].template_key, "option_some");
        assert_eq!(plan.cases[1].template_key, "option_none");
    }

    #[test]
    fn test_plan_pure_function_no_effect_test() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        // Pure function — no effect test case
        assert!(plan.cases.iter().all(|c| c.template_key != "effects"));
    }

    #[test]
    fn test_plan_variables_contain_fn_info() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        let vars = &plan.cases[0].variables;
        assert_eq!(vars.get("fn_name").unwrap(), "validate_write");
        assert_eq!(vars.get("param_names").unwrap(), "root, changed_files");
        assert_eq!(vars.get("return_shape").unwrap(), "result");
    }

    #[test]
    fn test_plan_with_type_defaults_generates_param_setup() {
        let contract = sample_result_contract();
        let grammar = grammar_with_defaults();
        let plan = generate_test_plan(&contract, &grammar);
        let vars = &plan.cases[0].variables;

        let param_setup = vars.get("param_setup").unwrap();
        assert!(
            param_setup.contains("let root ="),
            "should have root binding"
        );
        assert!(
            param_setup.contains("let changed_files ="),
            "should have changed_files binding"
        );
        assert!(
            param_setup.contains("Path::new"),
            "should use Path::new for &Path"
        );
        assert!(
            param_setup.contains("Vec::new()"),
            "should use Vec::new() for &[PathBuf]"
        );

        let param_args = vars.get("param_args").unwrap();
        assert!(
            param_args.contains("&root"),
            "should borrow root for &Path param"
        );
        assert!(
            param_args.contains("&changed_files"),
            "should borrow changed_files for &[PathBuf] param"
        );

        let extra_imports = vars.get("extra_imports").unwrap();
        assert!(
            extra_imports.contains("use std::path::Path;"),
            "should include Path import"
        );
    }

    #[test]
    fn test_render_test_plan_with_templates() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());

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
    fn test_render_test_plan_missing_template_uses_default() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());

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
    fn test_render_test_plan_deduplicates_identical_names() {
        // Simulate two branches with identical slugified conditions
        let plan = TestPlan {
            function_name: "check_status".to_string(),
            source_file: "src/lib.rs".to_string(),
            is_async: false,
            cases: vec![
                TestCase {
                    test_name: "test_check_status_none_return_false".to_string(),
                    branch_condition: "None => return false".to_string(),
                    expected_variant: "ok".to_string(),
                    expected_value: None,
                    template_key: "default".to_string(),
                    variables: HashMap::new(),
                },
                TestCase {
                    test_name: "test_check_status_none_return_false".to_string(),
                    branch_condition: "None => return false (other arm)".to_string(),
                    expected_variant: "ok".to_string(),
                    expected_value: None,
                    branch_condition: "None => return false (third arm)".to_string(),
                    expected_variant: "ok".to_string(),
                    expected_value: None,
                    template_key: "default".to_string(),
                    variables: HashMap::new(),
                },
            ],
        };

        let mut templates = HashMap::new();
        templates.insert("default".to_string(), "fn {test_name}() {{}}\n".to_string());

        let output = render_test_plan(&plan, &templates);

        // First occurrence keeps original name
        assert!(output.contains("fn test_check_status_none_return_false()"));
        // Subsequent occurrences get numeric suffixes
        assert!(output.contains("fn test_check_status_none_return_false_2()"));
        assert!(output.contains("fn test_check_status_none_return_false_3()"));
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
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases.len(), 1);
        assert_eq!(plan.cases[0].template_key, "no_panic");
    }

    // ── Behavioral inference tests ──

    #[test]
    fn test_setup_infers_empty_vec_for_is_empty_condition() {
        let params = vec![Param {
            name: "items".to_string(),
            param_type: "Vec<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "items.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Vec::new()"),
            "should use Vec::new() for empty vec, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_empty_slice_for_is_empty_condition() {
        let params = vec![Param {
            name: "changed_files".to_string(),
            param_type: "&[PathBuf]".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "changed_files.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for slice is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Vec::new()"),
            "should use Vec::new() for empty slice, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_non_empty_vec_for_negated_is_empty() {
        let params = vec![Param {
            name: "commits".to_string(),
            param_type: "Vec<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "!commits.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for negated is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("vec![Default::default()]"),
            "should use vec![Default::default()] for non-empty vec, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_nonexistent_path() {
        let params = vec![Param {
            name: "path".to_string(),
            param_type: "&Path".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "path doesn't exist",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for nonexistent path");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("nonexistent"),
            "should use a nonexistent path, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_none_for_option_is_none() {
        let params = vec![Param {
            name: "config".to_string(),
            param_type: "Option<Config>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "config.is_none()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_none");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("None"),
            "should use None for is_none, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_some_for_option_is_some() {
        let params = vec![Param {
            name: "config".to_string(),
            param_type: "Option<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "config.is_some()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_some");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Some("),
            "should use Some(...) for is_some, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_returns_none_for_unrecognized_condition() {
        let params = vec![Param {
            name: "x".to_string(),
            param_type: "CustomType".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "some random condition",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(
            result.is_none(),
            "should return None for unrecognized condition"
        );
    }

    #[test]
    fn test_setup_preserves_non_overridden_params() {
        let type_defaults = sample_type_defaults();
        let params = vec![
            Param {
                name: "root".to_string(),
                param_type: "&Path".to_string(),
                mutable: false,
                has_default: false,
            },
            Param {
                name: "items".to_string(),
                param_type: "&[String]".to_string(),
                mutable: false,
                has_default: false,
            },
        ];
        // Condition only targets items, root should keep its type_default
        let result = infer_setup_from_condition(
            "items.is_empty()",
            &params,
            &type_defaults,
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("let root = Path::new"),
            "root should keep its type_default, got: {}",
            so.setup_lines
        );
        assert!(
            so.setup_lines.contains("let items = Vec::new()"),
            "items should be overridden to Vec::new(), got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_assertion_result_ok_with_value() {
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("skipped".to_string()),
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "ValidationResult".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "items.is_empty()",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("unwrap()"),
            "should unwrap Ok value, got: {}",
            assertion
        );
        assert!(
            assertion.contains("skipped"),
            "should mention the expected value, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_result_err_with_value() {
        let returns = ReturnValue {
            variant: "err".to_string(),
            value: Some("not found".to_string()),
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "String".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "path doesn't exist",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("unwrap_err()"),
            "should unwrap Err, got: {}",
            assertion
        );
        assert!(
            assertion.contains("not found"),
            "should mention the error description, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_result_ok_without_value_falls_back() {
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: None,
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "()".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "default path",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_ok()"),
            "should fall back to is_ok(), got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_option_none() {
        let returns = ReturnValue {
            variant: "none".to_string(),
            value: None,
        };
        let return_type = ReturnShape::OptionType {
            some_type: "Item".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "key not found",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_none()"),
            "should assert is_none(), got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_escapes_inner_quotes_in_condition() {
        // Regression: conditions like `Some("changed")` contain double quotes
        // that must be escaped when embedded inside generated string literals.
        let returns = ReturnValue {
            variant: "some".to_string(),
            value: Some("changed".to_string()),
        };
        let return_type = ReturnShape::OptionType {
            some_type: "String".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            r#"CommitCategory::Other => Some("changed"),"#,
            &sample_assertion_templates(),
        );
        // The generated code must not contain unescaped inner quotes that would
        // break compilation. Raw `"changed"` inside a string literal is invalid.
        assert!(
            !assertion.contains(r#"Some("changed")"#),
            "inner quotes must be escaped in generated code, got:\n{}",
            assertion
        );
        // Escaped form should be present instead
        assert!(
            assertion.contains(r#"Some(\"changed\")"#),
            "should contain escaped quotes, got:\n{}",
            assertion
        );
    }

    #[test]
    fn test_assertion_bool_true() {
        let returns = ReturnValue {
            variant: "true".to_string(),
            value: None,
        };
        let return_type = ReturnShape::Bool;
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "input is valid",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("assert!(result"),
            "should assert true, got: {}",
            assertion
        );
        assert!(
            !assertion.contains("!result"),
            "should NOT negate, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_collection_empty_condition() {
        let returns = ReturnValue {
            variant: "value".to_string(),
            value: None,
        };
        let return_type = ReturnShape::Collection {
            element_type: "String".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "input.is_empty()",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_empty()"),
            "should assert emptiness, got: {}",
            assertion
        );
    }

    #[test]
    fn test_behavioral_plan_overrides_setup_for_is_empty_branch() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // First branch: changed_files.is_empty()
        let vars = &plan.cases[0].variables;
        let setup = vars.get("param_setup").unwrap();
        assert!(
            setup.contains("let changed_files = Vec::new()"),
            "should override changed_files to empty vec for is_empty branch, got:\n{}",
            setup
        );
        // root should still use its type_default (Path::new)
        assert!(
            setup.contains("let root = Path::new"),
            "root should keep its type_default, got:\n{}",
            setup
        );
    }

    #[test]
    fn test_behavioral_plan_generates_assertion_code() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // First branch returns Ok("skipped") — should have rich assertion
        let vars = &plan.cases[0].variables;
        let assertion = vars.get("assertion_code").unwrap();
        assert!(
            assertion.contains("unwrap()") || assertion.contains("is_ok()"),
            "should have an assertion for Ok branch, got:\n{}",
            assertion
        );
    }

    #[test]
    fn test_setup_bool_param_true() {
        let params = vec![Param {
            name: "verbose".to_string(),
            param_type: "bool".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "verbose == true",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("true"),
            "should set bool to true, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_numeric_param_zero() {
        let params = vec![Param {
            name: "count".to_string(),
            param_type: "usize".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "count == 0",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("let count = 0"),
            "should set count to 0, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_string_contains() {
        let params = vec![Param {
            name: "name".to_string(),
            param_type: "&str".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "name.contains(\"test\")",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("\"test\""),
            "should use the literal from contains(), got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_cross_branch_complement_gives_non_empty_to_other_branch() {
        // Branch 1: changed_files.is_empty() → hint "empty"
        // Branch 2: "validation command fails" → no hint for changed_files
        // Branch 2 should get the COMPLEMENT: "non_empty" for changed_files
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // Branch 2 (index 1): "validation command fails"
        let vars = &plan.cases[1].variables;
        let setup = vars.get("param_setup").unwrap();
        assert!(
            setup.contains("vec![Default::default()]")
                || setup.contains("vec![")
                || !setup.contains("Vec::new()"),
            "branch 2 should get non-empty changed_files (complement of branch 1's 'empty'), got:\n{}",
            setup
        );
    }

    #[test]
    fn test_full_pipeline_renders_with_behavioral_assertions() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);
        let rendered = render_test_plan(&plan, &grammar.test_templates);

        // Should produce output
        assert!(!rendered.is_empty(), "rendered output should not be empty");

        // Without type registry, enrichment can't resolve struct fields so the
        // result_ok_value assertion template has a // TODO placeholder. The pipeline
        // correctly falls back to the simpler result_ok assertion (is_ok()) because
        // a real assertion is better than a TODO stub. (#818)
        assert!(
            rendered.contains("result.is_ok()"),
            "without type registry, should fall back to is_ok() assertion, got:\n{}",
            rendered
        );

        // Should mention the branch condition in the assertion message
        assert!(
            rendered.contains("changed_files.is_empty()"),
            "should reference the branch condition, got:\n{}",
            rendered
        );
    }

    #[test]
    fn test_full_pipeline_renders_field_assertions_with_type_registry() {
        let contract = sample_result_contract();
        let grammar = full_grammar();

        // Build a type registry containing ValidationResult
        let mut type_registry = HashMap::new();
        type_registry.insert(
            "ValidationResult".to_string(),
            TypeDefinition {
                name: "ValidationResult".to_string(),
                kind: "struct".to_string(),
                file: "src/core/engine/validate_write.rs".to_string(),
                line: 10,
                fields: vec![
                    FieldDef {
                        name: "success".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "command".to_string(),
                        field_type: "Option<String>".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "output".to_string(),
                        field_type: "Option<String>".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "rolled_back".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                ],
                is_public: true,
            },
        );

        // Use generate_test_plan_with_types — this is the path the fixers use
        let plan = generate_test_plan_with_types(&contract, &grammar, &type_registry);
        let rendered = render_test_plan(&plan, &grammar.test_templates);

        // Should produce output
        assert!(!rendered.is_empty(), "rendered output should not be empty");

        // With type registry, enrichment should replace the TODO placeholder with
        // real field-level assertions instead of falling back to is_ok()
        assert!(
            rendered.contains("result.unwrap()"),
            "with type registry, should unwrap() to get at the inner value, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.success, false)"),
            "should assert success field, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.command, None)"),
            "should assert command field, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.rolled_back, false)"),
            "should assert rolled_back field, got:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("// TODO:"),
            "should NOT contain TODO placeholders, got:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("is_ok()"),
            "should NOT fall back to is_ok() when type info is available, got:\n{}",
            rendered
        );
    }

    #[test]
    fn test_parse_fields_from_rust_struct_source() {
        let source = r#"
pub struct ValidationResult {
    pub success: bool,
    pub command: Option<String>,
    pub output: Option<String>,
    pub rolled_back: bool,
    files_checked: usize,
}
"#;
        let fields = parse_fields_from_source(
            source,
            r"^\s*(?:pub\s+)?(\w+)\s*:\s*(.+?),?\s*$",
            Some(r"^\s*pub\b"),
            1, // name_group
            2, // type_group
        );

        assert_eq!(fields.len(), 5, "should find 5 fields");
        assert_eq!(fields[0].name, "success");
        assert_eq!(fields[0].field_type, "bool");
        assert!(fields[0].is_public, "success should be public");
        assert_eq!(fields[1].name, "command");
        assert_eq!(fields[1].field_type, "Option<String>");
        assert_eq!(fields[4].name, "files_checked");
        assert!(!fields[4].is_public, "files_checked should be private");
    }

    #[test]
    fn test_parse_fields_from_php_class_source() {
        let source = r#"
class AbilityResult {
    public string $status;
    public ?array $data;
    protected int $code;
    private string $internal_key;
    public bool $success;
}
"#;
        // PHP: type is group 1, name is group 2
        let fields = parse_fields_from_source(
            source,
            r"^\s*(?:public|protected|private)\s+(?:readonly\s+)?(\??\w+)\s+\$(\w+)",
            Some(r"^\s*public\b"),
            2, // name_group (PHP has name in group 2)
            1, // type_group (PHP has type in group 1)
        );

        assert_eq!(
            fields.len(),
            5,
            "should find 5 PHP properties, got {:?}",
            fields
        );
        assert_eq!(fields[0].name, "status");
        assert_eq!(fields[0].field_type, "string");
        assert!(fields[0].is_public, "status should be public");
        assert_eq!(fields[1].name, "data");
        assert_eq!(fields[1].field_type, "?array");
        assert_eq!(fields[2].name, "code");
        assert_eq!(fields[2].field_type, "int");
        assert!(
            !fields[2].is_public,
            "code should not be public (protected)"
        );
        assert_eq!(fields[4].name, "success");
        assert!(fields[4].is_public, "success should be public");
    }

    #[test]
    fn test_enrich_assertion_replaces_todo_with_field_hints() {
        let mut type_registry = HashMap::new();
        type_registry.insert(
            "ValidationResult".to_string(),
            TypeDefinition {
                name: "ValidationResult".to_string(),
                kind: "struct".to_string(),
                file: "src/test.rs".to_string(),
                line: 1,
                fields: vec![
                    FieldDef {
                        name: "success".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "command".to_string(),
                        field_type: "Option<String>".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "rolled_back".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                ],
                is_public: true,
            },
        );

        let assertion = "        let inner = result.unwrap();\n        // Branch returns Ok(skipped)\n        let _ = inner; // TODO: assert specific value for \"skipped\"";
        let return_type = ReturnShape::ResultType {
            ok_type: "ValidationResult".to_string(),
            err_type: "Error".to_string(),
        };
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("skipped".to_string()),
        };

        let type_defaults = sample_type_defaults();
        let enriched = enrich_assertion_with_fields(
            assertion,
            &returns,
            &return_type,
            &type_registry,
            &type_defaults,
            "Default::default()",
            Some("{indent}assert_eq!(inner.{field_name}, {expected_value});"),
        );

        // Should generate real assert_eq! statements, not comments
        assert!(
            enriched.contains("assert_eq!(inner.success, false)"),
            "should assert success field equals false, got:\n{}",
            enriched
        );
        assert!(
            enriched.contains("assert_eq!(inner.command, None)"),
            "should assert command field equals None, got:\n{}",
            enriched
        );
        assert!(
            enriched.contains("assert_eq!(inner.rolled_back, false)"),
            "should assert rolled_back field equals false, got:\n{}",
            enriched
        );
        assert!(
            !enriched.contains("TODO:"),
            "should replace the TODO placeholder, got:\n{}",
            enriched
        );
        assert!(
            !enriched.contains("let _ = inner"),
            "should remove the let _ = inner placeholder, got:\n{}",
            enriched
        );
    }

    #[test]
    fn test_enrich_assertion_resolves_path_qualified_types() {
        let mut type_registry = HashMap::new();
        type_registry.insert(
            "ValidationResult".to_string(),
            TypeDefinition {
                name: "ValidationResult".to_string(),
                kind: "struct".to_string(),
                file: "src/test.rs".to_string(),
                line: 1,
                fields: vec![FieldDef {
                    name: "success".to_string(),
                    field_type: "bool".to_string(),
                    is_public: true,
                }],
                is_public: true,
            },
        );

        let assertion = "        let inner = result.unwrap();\n        let _ = inner; // TODO: assert something";
        // Return type uses a path-qualified name
        let return_type = ReturnShape::ResultType {
            ok_type: "crate::engine::ValidationResult".to_string(),
            err_type: "Error".to_string(),
        };
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("val".to_string()),
        };

        let enriched = enrich_assertion_with_fields(
            assertion,
            &returns,
            &return_type,
            &type_registry,
            &sample_type_defaults(),
            "Default::default()",
            Some("{indent}assert_eq!(inner.{field_name}, {expected_value});"),
        );

        assert!(
            enriched.contains("assert_eq!(inner.success, false)"),
            "should resolve crate::engine::ValidationResult to ValidationResult, got:\n{}",
            enriched
        );
        assert!(
            !enriched.contains("TODO:"),
            "should replace TODO placeholder, got:\n{}",
            enriched
        );
    }

    #[test]
    fn test_enrich_assertion_skips_when_no_type_in_registry() {
        let type_registry = HashMap::new();

        let assertion = "        let _ = inner; // TODO: assert something";
        let return_type = ReturnShape::ResultType {
            ok_type: "UnknownType".to_string(),
            err_type: "Error".to_string(),
        };
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("val".to_string()),
        };

        let enriched = enrich_assertion_with_fields(
            assertion,
            &returns,
            &return_type,
            &type_registry,
            &[],
            "Default::default()",
            Some("{indent}assert_eq!(inner.{field_name}, {expected_value});"),
        );

        assert_eq!(
            enriched, assertion,
            "should return assertion unchanged when type is not in registry"
        );
    }
}
