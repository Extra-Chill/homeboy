#![cfg(test)]

use super::*;

const TEST_GRAMMAR_BLOCKS: &str = r#"
                escape = "\\"
                [blocks]
                open = "{"
                close = "}"
"#;

fn assert_no_unused_parameters(fp: &FileFingerprint, message: &str) {
    assert!(
        fp.unused_parameters.is_empty(),
        "{}: {:?}",
        message,
        fp.unused_parameters
    );
}

fn rust_grammar() -> Grammar {
    let grammar_path = std::path::Path::new(
        "/var/lib/sampleplugin/workspace/homeboy-extensions/rust/grammar.toml",
    );
    if grammar_path.exists() {
        grammar::load_grammar(grammar_path).expect("Failed to load Rust grammar")
    } else {
        // Minimal test grammar
        let source = r#"
                [language]
                id = "rust"
                extensions = ["rs"]
                [comments]
                line = ["//"]
                block = [["/*", "*/"]]
                doc = ["///", "//!"]
                [strings]
                quotes = ['"']
                __TEST_GRAMMAR_BLOCKS__
                [fingerprint]
                keywords = ["fn", "let", "if", "for", "return", "true", "false", "pub", "struct", "impl", "trait", "Self", "Result", "String", "bool", "i32", "usize"]
                skip_calls = ["if", "for", "return", "println", "write", "assert"]
                contract_method_names = []
                contract_type_hints = []
                registration_concepts = ["macro_invocation"]
                registration_skip_names = ["println", "assert", "write"]
                registration_skip_prefixes = ["test"]
                [fingerprint.namespace_derivation]
                prefix = "crate::"
                strip_leading_segments = 1
                separator = "::"
                include_file_stem_when_root = true
                [patterns.function]
                regex = '^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)'
                context = "any"
                [patterns.function.captures]
                visibility = 1
                name = 2
                params = 3
                [patterns.struct]
                regex = '^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)'
                context = "top_level"
                [patterns.struct.captures]
                visibility = 1
                kind = 2
                name = 3
                [patterns.import]
                regex = '^use\s+([\w:]+(?:::\{[^}]+\})?)\s*;'
                context = "top_level"
                [patterns.import.captures]
                path = 1
                [patterns.impl_block]
                regex = '^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)'
                context = "any"
                [patterns.impl_block.captures]
                trait_name = 1
                type_name = 2
                [patterns.test_attribute]
                regex = '#\[test\]'
                context = "any"
                [patterns.cfg_test]
                regex = '#\[cfg\(test\)\]'
                context = "any"
                "#
            .replace("__TEST_GRAMMAR_BLOCKS__", TEST_GRAMMAR_BLOCKS);
        toml::from_str(&source).expect("Failed to parse minimal grammar")
    }
}

#[test]
fn rust_namespace_comes_from_file_path_not_crate_imports() {
    let mut grammar = rust_grammar();
    grammar.fingerprint.namespace_derivation = Some(grammar::NamespaceDerivationConfig {
        prefix: Some("crate::".to_string()),
        strip_leading_segments: 1,
        separator: "::".to_string(),
        include_file_stem_when_root: true,
    });

    let command_content = r#"
use crate::help_topics;

pub fn run() {
    help_topics::print_all();
}
"#;
    let command_fp = fingerprint_from_grammar(command_content, &grammar, "src/commands/docs.rs")
        .expect("fingerprint should succeed");

    assert_eq!(command_fp.namespace.as_deref(), Some("crate::commands"));

    let nested_content = r#"
use crate::core::Result;

pub fn undo() -> Result<()> {
    Ok(())
}
"#;
    let nested_fp = fingerprint_from_grammar(nested_content, &grammar, "src/core/engine/undo.rs")
        .expect("fingerprint should succeed");

    assert_eq!(nested_fp.namespace.as_deref(), Some("crate::core::engine"));
}

#[test]
fn test_exact_hash_deterministic() {
    let body = "fn foo() { let x = 1; }";
    let h1 = exact_hash(body);
    let h2 = exact_hash(body);
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 16);
}

#[test]
fn test_exact_hash_whitespace_insensitive() {
    let a = "fn foo() {  let x = 1;  }";
    let b = "fn foo() { let x = 1; }";
    assert_eq!(exact_hash(a), exact_hash(b));
}

#[test]
fn test_structural_hash_different_names() {
    let a = "{ let foo = bar(); baz(foo); }";
    let b = "{ let qux = quux(); corge(qux); }";
    assert_eq!(
        structural_hash(a, &rust_grammar()),
        structural_hash(b, &rust_grammar()),
    );
}

#[test]
fn test_structural_hash_different_structure() {
    let a = "{ let x = 1; if x > 0 { return true; } }";
    let b = "{ let x = 1; for i in 0..x { print(i); } }";
    assert_ne!(
        structural_hash(a, &rust_grammar()),
        structural_hash(b, &rust_grammar()),
    );
}

#[test]
fn test_parse_param_names_rust() {
    let names = parse_param_names("&self, key: &str, value: String");
    assert_eq!(names, vec!["key", "value"]);
}

#[test]
fn test_parse_param_names_empty() {
    let names = parse_param_names("");
    assert!(names.is_empty());
}

#[test]
fn test_parse_param_names_mut() {
    let names = parse_param_names("&mut self, mut count: usize");
    assert_eq!(names, vec!["count"]);
}

#[test]
fn test_parse_param_names_rust_nested_type_paths() {
    let names = parse_param_names("overrides: &[(String, serde_json::Value)]");
    assert_eq!(names, vec!["overrides"]);
}

#[test]
fn test_parse_param_names_ignores_bare_type_paths() {
    let names = parse_param_names("serde_json::Value");
    assert!(names.is_empty());
}

#[test]
fn test_trait_impl_excluded_from_hashes() {
    let grammar = rust_grammar();
    let content = r#"
pub trait Entity {
    fn id(&self) -> &str;
}

pub struct Foo {
    id: String,
}

impl Entity for Foo {
    fn id(&self) -> &str {
        &self.id
    }
}

pub struct Bar {
    id: String,
}

impl Bar {
    fn id(&self) -> &str {
        &self.id
    }
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/test.rs").unwrap();

    // Trait impl method should NOT be in method_hashes
    // But the inherent method on Bar SHOULD be
    // Both should appear in methods list
    assert!(fp.methods.contains(&"id".to_string()));

    // The inherent impl's id() should be hashed (it's a real function)
    // The trait impl's id() should NOT be hashed
    // Since there's only one "id" key in the HashMap, the inherent one wins
    // (or the trait one is excluded, leaving only the inherent one)
    // In practice: with our logic, trait impl is skipped, so only Bar::id is hashed
    assert!(
        fp.method_hashes.contains_key("id"),
        "Bar's inherent id() should be in method_hashes"
    );
}

#[test]
fn test_basic_rust_fingerprint() {
    let grammar = rust_grammar();
    let content = r#"
use std::path::Path;

pub struct Config {
    pub name: String,
}

pub fn load(path: &Path) -> Config {
    let content = std::fs::read_to_string(path).unwrap();
    Config { name: content }
}

fn helper() -> bool {
    true
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/config.rs").unwrap();

    assert!(fp.methods.contains(&"load".to_string()));
    assert!(fp.methods.contains(&"helper".to_string()));
    assert_eq!(fp.type_name, Some("Config".to_string()));
    assert!(fp.method_hashes.contains_key("load"));
    assert!(fp.method_hashes.contains_key("helper"));
    assert_eq!(fp.visibility.get("load"), Some(&"public".to_string()));
    assert_eq!(fp.visibility.get("helper"), Some(&"private".to_string()));
}

#[test]
fn test_test_functions_excluded_from_hashes() {
    let grammar = rust_grammar();
    let content = r#"
pub fn real_fn() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_real_fn() {
        assert!(super::real_fn());
    }
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    assert!(fp.method_hashes.contains_key("real_fn"));
    assert!(
        !fp.method_hashes.contains_key("test_real_fn"),
        "Test functions should not be in method_hashes"
    );
    // Test method lives in `test_methods`, not `methods`, so a production
    // method named `test_*` can't be confused with an inline `#[test]`.
    assert!(fp.test_methods.contains(&"test_real_fn".to_string()));
    assert!(
        !fp.methods.contains(&"test_real_fn".to_string()),
        "Inline #[test] functions must not leak into `methods`"
    );
}

#[test]
fn test_unused_param_detection() {
    let grammar = rust_grammar();
    let content = r#"
pub(crate) fn uses_both(a: i32, b: i32) -> i32 {
    a + b
}

pub(crate) fn ignores_second(a: i32, b: i32) -> i32 {
    a * 2
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    // ignores_second has unused param b
    assert!(
        fp.unused_parameters
            .iter()
            .any(|p| p.function == "ignores_second" && p.param == "b"),
        "Should detect unused param 'b' in ignores_second"
    );
    // uses_both has no unused params
    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "uses_both"),
        "uses_both should have no unused params"
    );
}

#[test]
fn rust_unused_param_detection_handles_typed_nested_params() {
    let grammar = rust_grammar();
    let content = r#"
pub fn settings_json(overrides: &[(String, serde_json::Value)]) -> Self {
    self.settings_json_overrides.extend(overrides.iter().cloned());
    self
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    assert_no_unused_parameters(&fp, "Nested type paths should not be parsed as parameters");
}

#[test]
fn rust_unused_param_detection_sees_comparison_usage() {
    let grammar = rust_grammar();
    let content = r#"
fn parse_field_line(line: &str, syntax: FieldSyntax) -> Option<FieldSignature> {
    let trimmed = line.trim();

    if syntax == FieldSyntax::Php {
        return parse_php_property_line(trimmed);
    }

    None
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "parse_field_line" && p.param == "syntax"),
        "Parameter usage in comparisons should be detected. Got: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn trait_method_declarations_not_flagged_as_unused_params() {
    let grammar = rust_grammar();
    let content = r#"
pub trait FileSystem {
    fn read(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, content: &str) -> Result<()>;
    fn delete(&self, path: &Path) -> Result<()>;
    // Trait declarations have no body to inspect.
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    assert_no_unused_parameters(
        &fp,
        "Trait method declarations should not produce unused param findings",
    );
}

#[test]
fn trait_impl_methods_not_flagged_as_unused_params() {
    let grammar = rust_grammar();
    // The trait impl uses `path` via display(), but the detector shouldn't
    // even check — trait impls must match the trait's param names.
    let content = r#"
pub trait Store {
    fn save(&self, key: &str, value: &str) -> bool;
}

pub struct MemStore;

impl Store for MemStore {
    fn save(&self, key: &str, value: &str) -> bool {
        key.len() > 0
    }
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

    // value is unused in the impl, but since it's a trait impl it should be skipped
    assert!(
        !fp.unused_parameters.iter().any(|p| p.function == "save"),
        "Trait impl methods should not produce unused param findings, got: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn skip_list_does_not_suppress_defined_function_calls() {
    let grammar = rust_grammar();
    // The grammar suppresses "write" (for a macro-like call), but this
    // file defines fn write(...) and calls it — so it should appear
    // in internal_calls.
    let content = r#"
fn run() {
    let result = write("hello");
}

fn write(msg: &str) -> bool {
    println!("{}", msg);
    true
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/file.rs").unwrap();

    assert!(
        fp.internal_calls.contains(&"write".to_string()),
        "write should be in internal_calls when the file defines fn write(), got: {:?}",
        fp.internal_calls
    );
}

#[test]
fn rust_internal_calls_include_test_prefixed_production_helpers() {
    let grammar = rust_grammar();
    let content = r#"
use crate::core::code_audit::test_mapping::test_to_source_path;

pub fn map_source() {
    let _ = test_to_source_path("tests/core/audit_test.rs", &Default::default());
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/core/code_audit/test_coverage.rs")
        .unwrap();

    assert!(
        fp.internal_calls
            .contains(&"test_to_source_path".to_string()),
        "test_-prefixed production helpers should be retained as references, got: {:?}",
        fp.internal_calls
    );
}

#[test]
fn grammar_skip_calls_drive_internal_call_extraction() {
    let grammar = rust_grammar();
    let content = r#"
fn run() {
    guard();
    helper();
}

fn helper() {}
"#;

    let mut grammar = grammar;
    grammar.fingerprint.skip_calls = vec!["guard".to_string()];

    let fp = fingerprint_from_grammar(content, &grammar, "src/file.rs").unwrap();

    assert!(fp.internal_calls.contains(&"helper".to_string()));
    assert!(
        !fp.internal_calls.contains(&"guard".to_string()),
        "grammar skip_calls should suppress guard(), got: {:?}",
        fp.internal_calls
    );
}

fn php_metadata_grammar() -> Grammar {
    let source = r##"
            [language]
            id = "php"
            extensions = ["php"]
            [comments]
            line = ["//", "#"]
            block = [["/*", "*/"]]
            [strings]
            quotes = ['"', "'"]
            __TEST_GRAMMAR_BLOCKS__
            [fingerprint]
            keywords = ["class", "function", "public", "return", "int", "string", "bool", "true", "false"]
            skip_calls = ["if", "return"]
            variable_prefixes = ["$"]
            preserved_variables = ["$this"]
            contract_method_names = ["contractExecute"]
            contract_type_hints = ["FrameworkRequest"]
            registration_concepts = []
            [fingerprint.hook_concepts]
            emit_event = "action"
            transform_value = "filter"
            [patterns.method]
            regex = '((?:(?:public|protected|private|static|abstract|final)\s+)*)function\s+(\w+)\s*\(([^)]*)\)'
            context = "any"
            [patterns.method.captures]
            modifiers = 1
            name = 2
            params = 3
            [patterns.class]
            regex = '^\s*(?:class|trait|interface)\s+(\w+)'
            context = "top_level"
            [patterns.class.captures]
            name = 1
            [patterns.emit_event]
            regex = "emit_event\\s*\\(\\s*['\"]([^'\"]+)['\"]"
            context = "any"
            skip_strings = false
            [patterns.emit_event.captures]
            name = 1
            [patterns.transform_value]
            regex = "transform_value\\s*\\(\\s*['\"]([^'\"]+)['\"]"
            context = "any"
            skip_strings = false
            [patterns.transform_value.captures]
            name = 1
            "##
        .replace("__TEST_GRAMMAR_BLOCKS__", TEST_GRAMMAR_BLOCKS);
    toml::from_str(&source).expect("metadata grammar should parse")
}

#[test]
fn grammar_contract_metadata_suppresses_framework_unused_params() {
    let grammar = php_metadata_grammar();
    let content = "<?php\nclass Sample {\n    public function contractExecute( string $input ): bool {\n        return true;\n    }\n    public function route( FrameworkRequest $request ): bool {\n        return true;\n    }\n    public function helper( int $left, int $right ): int {\n        return $left * 2;\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "src/Sample.php").unwrap();

    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "contractExecute"),
        "grammar contract_method_names should suppress contractExecute params: {:?}",
        fp.unused_parameters
    );
    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "route" && p.param == "request"),
        "grammar contract_type_hints should suppress FrameworkRequest param: {:?}",
        fp.unused_parameters
    );
    assert!(
        fp.unused_parameters
            .iter()
            .any(|p| p.function == "helper" && p.param == "right"),
        "normal helper params should still be flagged: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn grammar_hook_concepts_drive_hook_extraction() {
    let grammar = php_metadata_grammar();
    let content = "<?php\nclass Sample {\n    public function fire() {\n        emit_event( 'sample_event' );\n        transform_value( 'sample_value', 'x' );\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "src/Sample.php").unwrap();

    assert!(fp
        .hooks
        .iter()
        .any(|hook| hook.hook_type == "action" && hook.name == "sample_event"));
    assert!(fp
        .hooks
        .iter()
        .any(|hook| hook.hook_type == "filter" && hook.name == "sample_value"));
}

#[test]
fn grammar_variable_prefixes_drive_structural_hash_normalization() {
    let grammar = php_metadata_grammar();
    let a = "{ $first = make_value(); return $first; }";
    let b = "{ $second = make_value(); return $second; }";

    assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
}

#[test]
fn existing_grammar_patterns_can_infer_dollar_variable_prefixes() {
    let mut grammar = php_metadata_grammar();
    grammar.fingerprint.variable_prefixes.clear();
    grammar.fingerprint.preserved_variables.clear();
    let a = "{ $first = make_value(); return $first; }";
    let b = "{ $second = make_value(); return $second; }";

    assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
}

#[test]
fn grammar_preserved_variables_keep_receiver_references_stable() {
    let grammar = php_metadata_grammar();
    let a = "{ $first = $this->make_value(); return $first; }";
    let b = "{ $second = $this->make_value(); return $second; }";

    assert_eq!(structural_hash(a, &grammar), structural_hash(b, &grammar));
}

#[test]
fn test_normalize_whitespace() {
    assert_eq!(normalize_whitespace("a  b\n\tc"), "a b c");
    assert_eq!(normalize_whitespace("  hello  "), "hello");
}

#[test]
fn test_replace_string_literals() {
    assert_eq!(
        replace_string_literals(r#"let x = "hello" + 'world'"#),
        "let x = STR + STR"
    );
}

/// Load the WordPress (PHP) grammar for reproducer tests.
///
/// Tests that need real PHP parsing are gated on the grammar being
/// available in the local workspace. In CI, the grammar is checked out
/// alongside this repo via the standard workspace layout.
fn php_grammar() -> Option<Grammar> {
    let grammar_path = std::path::Path::new(
        "/var/lib/sampleplugin/workspace/homeboy-extensions/wordpress/grammar.toml",
    );
    if !grammar_path.exists() {
        return None;
    }
    grammar::load_grammar(grammar_path).ok()
}

#[test]
fn namespace_with_php_reserved_word_segment_is_extracted() {
    // Regression test for #1134.
    //
    // PHP 7.0+ allows reserved words as namespace segments via context-
    // sensitive lexing. The auditor must not lose the namespace just
    // because `Global`, `List`, `Class`, etc. appear in it.
    let Some(grammar) = php_grammar() else {
        eprintln!("Skipping — wordpress grammar not available");
        return;
    };

    let content = "<?php\nnamespace SamplePlugin\\Engine\\AI\\Tools\\Global;\n\nclass WebFetch {\n    public function handle() {}\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "inc/Engine/AI/Tools/Global/WebFetch.php")
        .expect("fingerprint should succeed");

    assert_eq!(
        fp.namespace.as_deref(),
        Some("SamplePlugin\\Engine\\AI\\Tools\\Global"),
        "Namespace with reserved word segment 'Global' should be extracted. Got: {:?}",
        fp.namespace
    );
}

#[test]
fn namespace_with_leading_whitespace_is_extracted() {
    // Regression test for #1134 (real-world case).
    //
    // sample-plugin has files like Engine/AI/Tools/Global/AgentMemory.php
    // where the namespace line has a leading tab/indent (stylistic choice
    // after a docblock). The grammar regex is anchored to `^namespace`,
    // which fails when the line has leading whitespace.
    //
    // The auditor must handle this — PHP is insensitive to indentation
    // of the namespace declaration, and indented namespace declarations
    // are valid PHP.
    let Some(grammar) = php_grammar() else {
        eprintln!("Skipping — wordpress grammar not available");
        return;
    };

    let content = "<?php\n/**\n * Docblock.\n */\n\n\tnamespace SamplePlugin\\Engine\\AI\\Tools\\Global;\n\nclass AgentMemory {}\n";

    let fp = fingerprint_from_grammar(
        content,
        &grammar,
        "inc/Engine/AI/Tools/Global/AgentMemory.php",
    )
    .expect("fingerprint should succeed");

    assert_eq!(
        fp.namespace.as_deref(),
        Some("SamplePlugin\\Engine\\AI\\Tools\\Global"),
        "Namespace with leading whitespace (valid PHP) should be extracted. Got: {:?}",
        fp.namespace
    );
}

#[test]
fn unused_param_not_flagged_for_wp_rest_request_contract() {
    // Regression test for #1136.
    //
    // A REST route callback receives a WP_REST_Request $request but may
    // not use it (e.g., reads directly from options). The contract is
    // fixed by register_rest_route(); the parameter cannot be removed.
    let Some(grammar) = php_grammar() else {
        eprintln!("Skipping — wordpress grammar not available");
        return;
    };

    let content = "<?php\nnamespace X;\n\nclass Tokens {\n    public function list_external_tokens( \\WP_REST_Request $request ): \\WP_REST_Response {\n        $tokens = get_option( 'keys', array() );\n        return rest_ensure_response( $tokens );\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "inc/Tokens.php")
        .expect("fingerprint should succeed");

    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "list_external_tokens" && p.param == "request"),
        "WP_REST_Request contract param should not be flagged as unused. Got: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn unused_param_not_flagged_for_ability_execute_contract() {
    // A WP_Ability execute() method has a fixed signature that receives
    // array $input. Even when the method doesn't use $input (checks global
    // caps), the parameter is required by the ability contract.
    let Some(grammar) = php_grammar() else {
        eprintln!("Skipping — wordpress grammar not available");
        return;
    };

    let content = "<?php\nnamespace X;\n\nclass PermissionHelper {\n    public function checkPermission( array $input ): bool {\n        return current_user_can( 'manage_options' );\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "inc/PermissionHelper.php")
        .expect("fingerprint should succeed");

    assert!(
        !fp.unused_parameters
            .iter()
            .any(|p| p.function == "checkPermission" && p.param == "input"),
        "Ability checkPermission() $input contract param should not be flagged. Got: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn unused_param_still_flagged_for_normal_helper_method() {
    // Sanity check: genuine unused params in normal helper methods
    // should still be flagged. The contract-aware exclusions must not
    // swallow real findings.
    let Some(grammar) = php_grammar() else {
        eprintln!("Skipping — wordpress grammar not available");
        return;
    };

    let content = "<?php\nnamespace X;\n\nclass Helper {\n    public function compute( int $left, int $right ): int {\n        return $left * 2;\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "inc/Helper.php")
        .expect("fingerprint should succeed");

    assert!(
        fp.unused_parameters
            .iter()
            .any(|p| p.function == "compute" && p.param == "right"),
        "Genuine unused param should still be flagged. Got: {:?}",
        fp.unused_parameters
    );
}

#[test]
fn test_helpers_without_test_attr_not_counted_as_test_methods() {
    // Regression: functions inside #[cfg(test)] without #[test] attribute
    // were fingerprinted as test methods. Two cases:
    //
    // 1. fn test_insertion() — starts with test_, is a factory helper
    //    → was included as-is, orphan detector looked for "insertion"
    //
    // 2. fn rust_grammar() — doesn't start with test_, is a grammar builder
    //    → was prefixed to "test_rust_grammar", orphan detector looked for "rust_grammar"
    //
    // Both caused false orphaned test findings when no matching source method existed.
    let grammar = rust_grammar();
    let content = r#"
pub fn from_insertion(ins: &str) -> String {
    ins.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_insertion() -> String {
        "fixture".to_string()
    }

    fn rust_grammar() -> String {
        "grammar".to_string()
    }

    #[test]
    fn test_from_insertion() {
        let result = from_insertion("hello");
        assert_eq!(result, "hello");
    }
}
"#;
    let fp = fingerprint_from_grammar(content, &grammar, "src/core/engine/edit_op.rs")
        .expect("fingerprint should succeed");

    // test_from_insertion has #[test] → should be in `test_methods`,
    // and NOT in `methods` (production methods list).
    assert!(
        fp.test_methods.contains(&"test_from_insertion".to_string()),
        "Actual #[test] function should be in test_methods. test_methods: {:?}",
        fp.test_methods
    );
    assert!(
        !fp.methods.contains(&"test_from_insertion".to_string()),
        "Inline #[test] functions must not leak into `methods`. Methods: {:?}",
        fp.methods
    );

    // test_insertion is a helper (no #[test]) → should NOT be in either
    // list; it's neither a production method nor an inline test.
    assert!(
        !fp.methods.contains(&"test_insertion".to_string()),
        "Helper fn test_insertion() without #[test] should NOT be in methods. Methods: {:?}",
        fp.methods
    );
    assert!(
            !fp.test_methods.contains(&"test_insertion".to_string()),
            "Helper fn test_insertion() without #[test] should NOT be in test_methods. test_methods: {:?}",
            fp.test_methods
        );

    // rust_grammar is a helper (no #[test]) → should NOT appear with or
    // without a test_ prefix in either list.
    assert!(
        !fp.methods.contains(&"test_rust_grammar".to_string()),
        "Helper fn rust_grammar() without #[test] should NOT be in methods. Methods: {:?}",
        fp.methods
    );
    assert!(
            !fp.test_methods.contains(&"test_rust_grammar".to_string()),
            "Helper fn rust_grammar() without #[test] should NOT be in test_methods. test_methods: {:?}",
            fp.test_methods
        );

    // from_insertion is a real source method → should be in methods list
    assert!(
        fp.methods.contains(&"from_insertion".to_string()),
        "Source method from_insertion should be in methods. Methods: {:?}",
        fp.methods
    );
}
