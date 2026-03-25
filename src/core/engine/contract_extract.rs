//! Grammar-driven function contract extraction.
//!
//! Analyzes function bodies using patterns defined in `grammar.toml [contract]`
//! to produce `FunctionContract` structs. No language-specific logic — all
//! pattern knowledge comes from the grammar.
//!
//! This is the primary extraction path. The `scripts/contract.sh` extension
//! hook exists as a fallback for languages that need full AST parsing.

mod detect;
mod extract;
mod extract_generic_inner;
mod find_branch_condition;
mod helpers;
mod split_params;

pub use detect::*;
pub use extract::*;
pub use extract_generic_inner::*;
pub use find_branch_condition::*;
pub use helpers::*;
pub use split_params::*;


use regex::Regex;

use super::contract::*;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_contract_grammar() -> ContractGrammar {
        let mut effects = HashMap::new();
        effects.insert(
            "file_read".to_string(),
            vec![r"std::fs::read|fs::read_to_string|File::open".to_string()],
        );
        effects.insert(
            "file_write".to_string(),
            vec![r"std::fs::write|fs::write".to_string()],
        );
        effects.insert(
            "process_spawn".to_string(),
            vec![r"Command::new\((.+?)\)".to_string()],
        );

        let mut return_patterns = HashMap::new();
        return_patterns.insert("ok".to_string(), vec![r"Ok\((.+?)\)".to_string()]);
        return_patterns.insert("err".to_string(), vec![r"Err\((.+?)\)".to_string()]);
        return_patterns.insert("some".to_string(), vec![r"Some\((.+?)\)".to_string()]);
        return_patterns.insert("none".to_string(), vec![r"\breturn\s+None\b".to_string()]);

        let mut return_shapes = HashMap::new();
        return_shapes.insert("result".to_string(), vec![r"Result\s*<".to_string()]);
        return_shapes.insert("option".to_string(), vec![r"Option\s*<".to_string()]);
        return_shapes.insert("bool".to_string(), vec![r"^\s*bool\s*$".to_string()]);
        return_shapes.insert("collection".to_string(), vec![r"Vec\s*<".to_string()]);

        ContractGrammar {
            effects,
            guard_patterns: vec![
                r"if\s+.*\{\s*return\s+".to_string(),
                r"if\s+.*\.is_empty\(\)".to_string(),
            ],
            return_patterns,
            error_propagation: vec![r"\?\s*;".to_string()],
            return_shapes,
            panic_patterns: vec![
                r"panic!\s*\((.+?)\)".to_string(),
                r"unreachable!\s*\(".to_string(),
                r"\.unwrap\(\)".to_string(),
            ],
            return_type_separator: "->".to_string(),
            param_format: "name_colon_type".to_string(),
            test_templates: HashMap::new(),
            type_defaults: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn detect_return_shape_result() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("pub fn foo() -> Result<String, Error> {", &cg);
        assert!(matches!(shape, ReturnShape::ResultType { .. }));
        if let ReturnShape::ResultType { ok_type, err_type } = shape {
            assert_eq!(ok_type, "String");
            assert_eq!(err_type, "Error");
        }
    }

    #[test]
    fn detect_return_shape_option() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn bar() -> Option<usize> {", &cg);
        assert!(matches!(shape, ReturnShape::OptionType { .. }));
    }

    #[test]
    fn detect_return_shape_bool() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn baz() -> bool {", &cg);
        assert!(matches!(shape, ReturnShape::Bool));
    }

    #[test]
    fn detect_return_shape_unit() {
        let cg = make_contract_grammar();
        let shape = detect_return_shape("fn qux() {", &cg);
        assert!(matches!(shape, ReturnShape::Unit));
    }

    #[test]
    fn parse_params_basic() {
        let params = parse_params("root: &Path, files: &[PathBuf]", "name_colon_type");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "root");
        assert_eq!(params[0].param_type, "&Path");
        assert_eq!(params[1].name, "files");
    }

    #[test]
    fn parse_params_with_self() {
        let params = parse_params("&self, key: &str", "name_colon_type");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "key");
    }

    #[test]
    fn parse_params_empty() {
        let params = parse_params("", "name_colon_type");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_params_php_format() {
        let params = parse_params("string $name, ?int $count = 0", "type_dollar_name");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "name");
        assert_eq!(params[0].param_type, "string");
        assert!(!params[0].has_default);
        assert_eq!(params[1].name, "count");
        assert_eq!(params[1].param_type, "?int");
        assert!(params[1].has_default);
    }

    #[test]
    fn parse_params_php_untyped() {
        let params = parse_params("$request, $args", "type_dollar_name");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "request");
        assert_eq!(params[0].param_type, "mixed");
        assert_eq!(params[1].name, "args");
    }

    #[test]
    fn detect_receiver_ref() {
        assert!(matches!(
            detect_receiver("&self, key: &str"),
            Some(Receiver::Ref)
        ));
    }

    #[test]
    fn detect_receiver_mut_ref() {
        assert!(matches!(
            detect_receiver("&mut self"),
            Some(Receiver::MutRef)
        ));
    }

    #[test]
    fn detect_receiver_none() {
        assert!(detect_receiver("key: &str").is_none());
    }

    #[test]
    fn split_params_with_generics() {
        let parts = split_params("map: HashMap<String, Vec<u8>>, count: usize");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("HashMap"));
        assert!(parts[1].contains("usize"));
    }

    #[test]
    fn detect_effects_from_body() {
        let cg = make_contract_grammar();
        let body = vec![
            (10, "    let content = std::fs::read_to_string(path)?;"),
            (11, "    std::fs::write(output, &content)?;"),
        ];
        let effects = detect_effects(&body, &cg);
        assert!(effects.iter().any(|e| matches!(e, Effect::FileRead)));
        assert!(effects.iter().any(|e| matches!(e, Effect::FileWrite)));
    }

    #[test]
    fn extract_result_types_basic() {
        let (ok, err) = extract_result_types("Result<ValidationResult, Error>");
        assert_eq!(ok, "ValidationResult");
        assert_eq!(err, "Error");
    }

    #[test]
    fn extract_generic_inner_basic() {
        assert_eq!(extract_generic_inner("Option<String>"), "String");
        assert_eq!(extract_generic_inner("Vec<u8>"), "u8");
    }

    #[test]
    fn join_declaration_lines_single_line() {
        let lines = vec!["pub fn foo(x: u32) -> bool {"];
        assert_eq!(
            join_declaration_lines(&lines, 1, 1),
            "pub fn foo(x: u32) -> bool {"
        );
    }

    #[test]
    fn join_declaration_lines_multi_line_params() {
        let lines = vec![
            "pub fn complex(",
            "    root: &Path,",
            "    files: &[PathBuf],",
            "    config: &Config,",
            ") -> Result<(), Error> {",
        ];
        let decl = join_declaration_lines(&lines, 1, 5);
        assert!(decl.contains("root: &Path,"));
        assert!(decl.contains("config: &Config,"));
        assert!(decl.contains("-> Result<(), Error>"));
    }

    #[test]
    fn join_declaration_lines_return_type_on_next_line() {
        let lines = vec![
            "pub fn long_name(arg: Type)",
            "    -> Result<ValidationResult, Error>",
            "{",
        ];
        let decl = join_declaration_lines(&lines, 1, 3);
        assert!(decl.contains("-> Result<ValidationResult, Error>"));
    }

    #[test]
    fn extract_params_from_declaration_simple() {
        let decl = "pub fn foo(x: u32, y: &str) -> bool {";
        assert_eq!(
            extract_params_from_declaration(decl),
            Some("x: u32, y: &str".to_string())
        );
    }

    #[test]
    fn extract_params_from_declaration_nested_generics() {
        let decl = "pub fn bar(map: HashMap<String, Vec<u8>>, flag: bool) -> () {";
        assert_eq!(
            extract_params_from_declaration(decl),
            Some("map: HashMap<String, Vec<u8>>, flag: bool".to_string())
        );
    }

    #[test]
    fn extract_params_from_declaration_multi_line_joined() {
        let decl = "pub fn complex( root: &Path, files: &[PathBuf], config: &Config, ) -> Result<(), Error> {";
        let params = extract_params_from_declaration(decl).unwrap();
        assert!(params.contains("root: &Path"));
        assert!(params.contains("files: &[PathBuf]"));
        assert!(params.contains("config: &Config"));
    }

    #[test]
    fn extract_params_from_declaration_no_params() {
        let decl = "pub fn no_args() -> bool {";
        assert_eq!(extract_params_from_declaration(decl), None);
    }

    #[test]
    fn extract_params_from_declaration_self_receiver() {
        let decl = "pub fn method(&self, x: u32) -> bool {";
        let params = extract_params_from_declaration(decl).unwrap();
        assert!(params.contains("&self"));
        assert!(params.contains("x: u32"));
    }

    #[test]
    fn extract_propagation_call_method() {
        assert_eq!(
            extract_propagation_call("    let content = fs::read_to_string(path)?;"),
            "read_to_string"
        );
    }

    #[test]
    fn extract_propagation_call_function() {
        assert_eq!(
            extract_propagation_call("    let parsed = serde_json::from_str(&content)?;"),
            "from_str"
        );
    }

    #[test]
    fn extract_propagation_call_chained() {
        assert_eq!(
            extract_propagation_call("    config.validate()?;"),
            "validate"
        );
    }

    #[test]
    fn extract_propagation_call_no_match() {
        assert_eq!(extract_propagation_call("    let x = 42;"), "operation");
    }

    #[test]
    fn detect_error_propagation_adds_branch() {
        let body_lines = vec![
            (2, "    let content = fs::read_to_string(path)?;"),
            (3, "    let parsed = serde_json::from_str(&content)?;"),
            (4, "    Ok(parsed)"),
        ];

        let contract = ContractGrammar {
            error_propagation: vec![r"\?\s*;".to_string(), r"\?\s*$".to_string()],
            ..Default::default()
        };

        let mut branches = Vec::new();
        detect_error_propagation(&body_lines, &contract, &mut branches);

        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].returns.variant, "err");
        assert!(branches[0].condition.contains("error propagation via ?"));
        assert!(branches[0].condition.contains("read_to_string"));
        assert!(branches[0].condition.contains("from_str"));
    }

    #[test]
    fn detect_error_propagation_skips_when_explicit_err_exists() {
        let body_lines = vec![
            (2, "    let content = fs::read_to_string(path)?;"),
            (3, "    Ok(content)"),
        ];

        let contract = ContractGrammar {
            error_propagation: vec![r"\?\s*;".to_string()],
            ..Default::default()
        };

        // Pre-existing explicit Err branch
        let mut branches = vec![Branch {
            condition: "invalid input".to_string(),
            returns: ReturnValue {
                variant: "err".to_string(),
                value: None,
            },
            effects: vec![],
            line: Some(5),
        }];

        detect_error_propagation(&body_lines, &contract, &mut branches);

        // Should NOT add another err branch
        assert_eq!(branches.len(), 1);
    }
}
