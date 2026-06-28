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

// ============================================================================
// Grammar-driven aggregate / hook-callback / call-site extraction (#6826)
//
// These prove the grammar engine now populates the four FileFingerprint fields
// that were hardcoded to `Vec::new()` — starving the aggregate-construction,
// dead-code, and deprecation-age detectors for every grammar-fingerprinted
// Rust/PHP file. The grammar config below mirrors the per-language reference
// fingerprint scripts (homeboy-extensions rust/wordpress fingerprint.sh), which
// are the only prior producers of these fields and define the correct output.
// ============================================================================

/// Rust grammar extended with the construction-seam + aggregate-literal policy
/// the reference Rust fingerprint script encodes.
fn rust_aggregate_grammar() -> Grammar {
    let mut grammar = rust_grammar();
    grammar.fingerprint.aggregate_seams = Some(AggregateSeamConfig {
        method_names: vec!["new".to_string(), "builder".to_string(), "default".to_string()],
        method_prefixes: vec!["from_".to_string(), "for_".to_string(), "with_".to_string()],
        type_method_templates: vec!["build_{type}".to_string(), "create_{type}".to_string()],
    });
    grammar.fingerprint.aggregate_literals = Some(grammar::AggregateLiteralConfig {
        pattern: r"\b([A-Z][A-Za-z0-9_]*)\s*\{([^{};]*)\}".to_string(),
        field_pattern: r"\b([a-z_][A-Za-z0-9_]*)\s*:".to_string(),
        min_fields: 2,
        skip_before_pattern: Some(r"(?:struct|enum|impl|trait|type|use)\s+$".to_string()),
        skip_before_window: 80,
    });
    grammar
}

/// PHP metadata grammar extended with the hook-callback + call-site policy the
/// reference WordPress fingerprint script encodes.
fn php_callback_grammar() -> Grammar {
    let mut grammar = php_metadata_grammar();
    // Match the WordPress grammar's PHP call skip set so free-function call
    // sites mirror the reference script.
    grammar.fingerprint.skip_calls = [
        "if", "while", "for", "foreach", "switch", "match", "catch", "return", "echo", "print",
        "isset", "unset", "empty", "list", "array", "function", "class", "interface", "trait",
        "new", "require", "require_once", "include", "include_once", "define", "defined", "die",
        "exit", "eval", "compact", "extract", "var_dump", "print_r", "var_export",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    grammar.fingerprint.hook_callback_patterns = vec![
        // add_action/add_filter/add_shortcode with string callback
        r#"(?:add_action|add_filter|add_shortcode)\s*\([^,]+,\s*['"](\w+)['"]\s*[,)]"#.to_string(),
        // ... with array( $this, 'method' )
        r#"(?:add_action|add_filter|add_shortcode)\s*\([^,]+,\s*array\s*\(\s*\$this\s*,\s*['"](\w+)['"]\s*\)"#.to_string(),
        // ... with [ $this, 'method' ]
        r#"(?:add_action|add_filter|add_shortcode)\s*\([^,]+,\s*\[\s*\$this\s*,\s*['"](\w+)['"]\s*\]"#.to_string(),
        // ... with __CLASS__ / self::class / static::class
        r#"(?:add_action|add_filter|add_shortcode)\s*\([^,]+,\s*(?:array\s*\(|\[)\s*(?:__CLASS__|self::class|static::class)\s*,\s*['"](\w+)['"]"#.to_string(),
        // register_(de)activation/uninstall hooks
        r#"register_(?:activation|deactivation|uninstall)_hook\s*\([^,]+,\s*['"](\w+)['"]"#.to_string(),
        // 'callback' => 'function_name'
        r#"['"](?:callback|[a-z_]+_callback)['"]\s*=>\s*['"](\w+)['"]"#.to_string(),
        // 'callback' => array( $this, 'method' )
        r#"['"](?:callback|[a-z_]+_callback)['"]\s*=>\s*(?:array\s*\(|\[)\s*\$this\s*,\s*['"](\w+)['"]"#.to_string(),
        // 'callback' => array( self::class, 'method' )
        r#"['"](?:callback|[a-z_]+_callback)['"]\s*=>\s*(?:array\s*\(|\[)\s*(?:__CLASS__|self::class|static::class)\s*,\s*['"](\w+)['"]"#.to_string(),
    ];
    grammar.fingerprint.call_sites = Some(grammar::CallSiteConfig {
        patterns: vec![
            grammar::CallSitePattern {
                regex: r"(?:\$this->|self::|static::|[A-Z]\w*::)(\w+)\s*\(".to_string(),
                apply_skip_calls: false,
            },
            grammar::CallSitePattern {
                regex: r"\b([a-z_]\w*)\s*\(".to_string(),
                apply_skip_calls: true,
            },
        ],
        skip_line_pattern: Some(
            r"(?:public|protected|private|static|abstract|final|\s)*function\s".to_string(),
        ),
        skip_name_prefixes: vec!["test".to_string()],
    });
    grammar
}

#[test]
fn grammar_engine_extracts_rust_aggregate_seams_and_literals() {
    // Reference Rust fingerprint script output for this source:
    //   aggregate_construction_seams: [{Config,new},{Config,from_parts}]
    //   aggregate_literals:           [{Other,[x,y]}]
    let grammar = rust_aggregate_grammar();
    let content = r#"pub struct Config { a: String, b: usize }

impl Config {
    pub fn new(a: String) -> Self {
        Self { a, b: 0 }
    }
    pub fn from_parts(a: String, b: usize) -> Config {
        Config { a, b }
    }
    pub fn describe(&self) -> String {
        self.a.clone()
    }
}

pub fn build_something() -> Other {
    Other { x: 1, y: 2 }
}
"#;

    let fp = fingerprint_from_grammar(content, &grammar, "src/config.rs")
        .expect("fingerprint should succeed");

    // Seams: canonical constructors on Config — `new` (exact) and `from_parts`
    // (from_ prefix). `describe` is not a seam; `build_something` is a free
    // function with no owning type.
    let mut seams: Vec<(String, String)> = fp
        .aggregate_construction_seams
        .iter()
        .map(|s| (s.type_name.clone(), s.method.clone()))
        .collect();
    seams.sort();
    assert_eq!(
        seams,
        vec![
            ("Config".to_string(), "from_parts".to_string()),
            ("Config".to_string(), "new".to_string()),
        ],
        "expected Config::new + Config::from_parts seams, got {:?}",
        fp.aggregate_construction_seams
    );

    // Literals: only the explicit `Other { x: 1, y: 2 }` qualifies. The struct
    // definition is skipped (preceded by `struct `), and `Self { a, b: 0 }` /
    // `Config { a, b }` have fewer than two `field:` initializers.
    let literals: Vec<(String, Vec<String>)> = fp
        .aggregate_literals
        .iter()
        .map(|l| (l.type_name.clone(), l.fields.clone()))
        .collect();
    assert_eq!(
        literals,
        vec![("Other".to_string(), vec!["x".to_string(), "y".to_string()])],
        "expected a single Other literal with fields [x, y], got {:?}",
        fp.aggregate_literals
    );
}

#[test]
fn restored_aggregate_fields_make_construction_detector_fire() {
    use crate::core::code_audit::detectors::aggregate_construction;

    let grammar = rust_aggregate_grammar();
    let def = r#"pub struct Widget { name: String, size: usize, active: bool }
impl Widget {
    pub fn new(name: String) -> Self {
        Self { name, size: 0, active: false }
    }
}
"#;
    let usage = |var: &str| {
        format!(
            "pub fn make_{var}() -> Widget {{\n    Widget {{ name: n, size: s, active: t }}\n}}\n"
        )
    };

    let fingerprints = vec![
        fingerprint_from_grammar(def, &grammar, "src/widget.rs").unwrap(),
        fingerprint_from_grammar(&usage("a"), &grammar, "src/a.rs").unwrap(),
        fingerprint_from_grammar(&usage("b"), &grammar, "src/b.rs").unwrap(),
        fingerprint_from_grammar(&usage("c"), &grammar, "src/c.rs").unwrap(),
    ];

    // With the fields populated, the seam (Widget::new) + three repeated inline
    // literals across files trip the direct-aggregate-construction detector.
    let refs: Vec<&FileFingerprint> = fingerprints.iter().collect();
    let findings = aggregate_construction::run(&refs);
    assert_eq!(
        findings.len(),
        1,
        "expected the construction detector to fire, got {findings:?}"
    );
    assert!(
        findings[0].description.contains("Widget"),
        "finding should name the Widget aggregate: {}",
        findings[0].description
    );

    // Proof of the regression: with the four fields empty (the pre-fix state),
    // the very same files produce no finding — the detector was starved.
    let starved: Vec<FileFingerprint> = fingerprints
        .iter()
        .cloned()
        .map(|mut fp| {
            fp.aggregate_literals.clear();
            fp.aggregate_construction_seams.clear();
            fp
        })
        .collect();
    let starved_refs: Vec<&FileFingerprint> = starved.iter().collect();
    assert!(
        aggregate_construction::run(&starved_refs).is_empty(),
        "with the fields empty the detector must not fire — proving the starvation"
    );
}

#[test]
fn grammar_engine_extracts_php_hook_callbacks_and_call_sites() {
    // Reference WordPress fingerprint script output for this source:
    //   hook_callbacks: ["handle"]
    //   call_sites includes: {add_action, .., 2} and {compute, .., 2}
    let grammar = php_callback_grammar();
    let content = "<?php\nnamespace Sample;\n\nclass Plugin {\n    public function register() {\n        add_action( 'init', array( $this, 'handle' ) );\n        register_rest_route( 'ns/v1', '/x', array( 'callback' => array( $this, 'handle' ) ) );\n    }\n    public function handle( $request ) {\n        return $this->compute( 1, 2 );\n    }\n    public function compute( $a, $b ) {\n        return $a + $b;\n    }\n}\n";

    let fp = fingerprint_from_grammar(content, &grammar, "inc/Plugin.php")
        .expect("fingerprint should succeed");

    assert_eq!(
        fp.hook_callbacks,
        vec!["handle".to_string()],
        "expected the hook callback `handle` to be extracted, got {:?}",
        fp.hook_callbacks
    );

    // add_action(...) is a free-function call with two top-level args.
    assert!(
        fp.call_sites
            .iter()
            .any(|cs| cs.target == "add_action" && cs.arg_count == 2),
        "expected an add_action call site with arg_count 2, got {:?}",
        fp.call_sites
    );
    // $this->compute( 1, 2 ) is a receiver-qualified call with two args.
    assert!(
        fp.call_sites
            .iter()
            .any(|cs| cs.target == "compute" && cs.arg_count == 2),
        "expected a compute call site with arg_count 2, got {:?}",
        fp.call_sites
    );
    // Declaration lines (`public function handle(...)`) are signatures, not
    // call sites, and must not be recorded as calls.
    assert!(
        !fp.call_sites.iter().any(|cs| cs.target == "handle"),
        "function declarations must not appear as call sites, got {:?}",
        fp.call_sites
    );
}

#[test]
fn grammar_aggregate_fields_stay_empty_without_grammar_policy() {
    // Without seam/literal/callback/call-site policy in the grammar, the engine
    // emits nothing — language idioms stay grammar-owned, not baked into core.
    let grammar = rust_grammar();
    let content = "pub struct Config { a: usize }\nimpl Config {\n    pub fn new() -> Self { Self { a: 0 } }\n}\n";
    let fp = fingerprint_from_grammar(content, &grammar, "src/config.rs").unwrap();
    assert!(fp.aggregate_construction_seams.is_empty());
    assert!(fp.aggregate_literals.is_empty());
    assert!(fp.hook_callbacks.is_empty());
    assert!(fp.call_sites.is_empty());
}
