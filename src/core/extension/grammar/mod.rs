//! Language grammar primitive — structure-aware regex matching.
//!
//! A generic engine for extracting structural information from source files.
//! The engine itself has zero language knowledge. Languages are defined by
//! grammar files shipped in extensions.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs   (this file)
//!   ├── Grammar       — loaded from extension TOML, defines patterns for a language
//!   ├── StructuralParser — brace-depth, string/comment-aware iteration
//!   └── Extractor     — applies Grammar patterns via StructuralParser
//!
//! Extension grammar.toml → Grammar → Extractor → Vec<Symbol>
//! ```
//!
//! # Design Principles
//!
//! - **Zero built-in language knowledge** — all patterns come from grammars
//! - **Structure-aware** — tracks brace depth, skips strings and comments
//! - **Composable** — features query for concepts ("give me methods") not languages
//! - **Same model as `utils/baseline.rs`** — dumb primitive, smart consumers

mod extract;
mod loading;
mod parser;
mod types;

pub use super::grammar_strings::find_unclosed_raw_string_on_line;
use super::grammar_strings::{line_closes_regular_string, line_has_unclosed_regular_string};

pub use extract::{extract, namespace, Symbol};
pub(crate) use extract::cached_regex;
pub use loading::{load_for_extension_path, load_grammar, load_grammar_json};
pub use parser::{ContextualLine, Region, StructuralContext};
pub use types::{
    AggregateLiteralConfig, AggregateSeamConfig, BlockSyntax, CallSiteConfig, CallSitePattern,
    CommentSyntax, ConceptPattern, ContractGrammar, FingerprintGrammar, Grammar, LanguageMeta,
    NamespaceDerivationConfig, StringSyntax, TypeConstructor, TypeDefault,
};

#[cfg(test)]
pub(crate) use extract::{
    extract_block_body, extract_concept, import_paths, method_names, public_symbols,
    regex_cache_has_for_tests, type_names,
};
#[cfg(test)]
pub(crate) use parser::walk_lines;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn captures(entries: &[(&str, usize)]) -> HashMap<String, usize> {
        entries
            .iter()
            .map(|(name, index)| ((*name).to_string(), *index))
            .collect()
    }

    fn rust_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
                import_parser: None,
            },
            comments: CommentSyntax {
                line: vec!["//".to_string()],
                block: vec![("/*".to_string(), "*/".to_string())],
                doc: vec!["///".to_string(), "//!".to_string()],
            },
            strings: StringSyntax {
                quotes: vec!["\"".to_string()],
                escape: "\\".to_string(),
                multiline: vec![],
            },
            blocks: BlockSyntax::default(),
            contract: None,
            fingerprint: FingerprintGrammar::default(),
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "function".to_string(),
                    ConceptPattern {
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)"
                            .to_string(),
                        captures: captures(&[("name", 1), ("params", 2)]),
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "struct".to_string(),
                    ConceptPattern {
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+(\w+)"
                            .to_string(),
                        captures: captures(&[("name", 1)]),
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "import".to_string(),
                    ConceptPattern {
                        regex: r"use\s+([\w:]+(?:::\{[^}]+\})?);".to_string(),
                        captures: captures(&[("path", 1)]),
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p
            },
        }
    }

    fn php_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "php".to_string(),
                extensions: vec!["php".to_string()],
                import_parser: None,
            },
            comments: CommentSyntax {
                line: vec!["//".to_string(), "#".to_string()],
                block: vec![("/*".to_string(), "*/".to_string())],
                doc: vec![],
            },
            strings: StringSyntax {
                quotes: vec!["\"".to_string(), "'".to_string()],
                escape: "\\".to_string(),
                multiline: vec![],
            },
            blocks: BlockSyntax::default(),
            contract: None,
            fingerprint: FingerprintGrammar::default(),
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "method".to_string(),
                    ConceptPattern {
                        regex: r"(?:(?:public|protected|private|static|abstract|final)\s+)*function\s+(\w+)\s*\(([^)]*)\)".to_string(),
                        captures: captures(&[("name", 1), ("params", 2)]),
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "class".to_string(),
                    ConceptPattern {
                        regex: r"(?:abstract\s+)?(?:final\s+)?(class|trait|interface)\s+(\w+)"
                            .to_string(),
                        captures: captures(&[("kind", 1), ("name", 2)]),
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "namespace".to_string(),
                    ConceptPattern {
                        regex: r"namespace\s+([\w\\]+);".to_string(),
                        captures: captures(&[("name", 1)]),
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p
            },
        }
    }

    #[test]
    fn contract_grammar_has_no_language_behavior_without_manifest_values() {
        let grammar: Grammar = toml::from_str(
            r#"
                [language]
                id = "toy"
                extensions = ["toy"]

                [comments]
                line = ["--"]
                block = []
                doc = []

                [strings]
                quotes = ["`"]
                escape = "\\"

                [blocks]
                open = "["
                close = "]"

                [contract]

                [patterns.function]
                regex = '^def\s+(\w+)\s*\(([^)]*)\)'
                context = "any"

                [patterns.function.captures]
                name = 1
                params = 2
            "#,
        )
        .expect("synthetic grammar should parse");

        let contract = grammar
            .contract
            .as_ref()
            .expect("contract section should deserialize");
        assert_eq!(contract.return_type_separator, "");
        assert_eq!(contract.param_format, "");
        assert_eq!(contract.fallback_default, "");

        let symbols = extract("def cook(value)\n", &grammar);
        assert_eq!(method_names(&symbols), vec!["cook".to_string()]);
    }

    #[test]
    fn rust_contract_behavior_is_owned_by_explicit_grammar_values() {
        let contract: ContractGrammar = serde_json::from_value(serde_json::json!({
            "return_type_separator": "->",
            "param_format": "name_colon_type",
            "fallback_default": "Default::default()"
        }))
        .expect("explicit Rust contract values should deserialize");

        assert_eq!(contract.return_type_separator, "->");
        assert_eq!(contract.param_format, "name_colon_type");
        assert_eq!(contract.fallback_default, "Default::default()");
    }

    #[test]
    fn namespace_separator_has_no_core_language_default() {
        let config: NamespaceDerivationConfig = toml::from_str(
            r#"
                strip_leading_segments = 1
                include_file_stem_when_root = true
            "#,
        )
        .expect("namespace derivation slot should deserialize without profile defaults");

        assert_eq!(config.separator, "");
    }

    #[test]
    fn namespace_separator_is_owned_by_explicit_grammar_profile() {
        let config: NamespaceDerivationConfig = toml::from_str(
            r#"
                prefix = "profile_scope::"
                strip_leading_segments = 1
                separator = "::"
                include_file_stem_when_root = true
            "#,
        )
        .expect("explicit namespace derivation profile should deserialize");

        assert_eq!(config.prefix.as_deref(), Some("profile_scope::"));
        assert_eq!(config.separator, "::");
    }

    // ---- Structural parser tests ----

    #[test]
    fn walk_lines_tracks_depth() {
        let content = "fn main() {\n    let x = 1;\n    if true {\n        foo();\n    }\n}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].depth, 0); // fn main() {
        assert_eq!(lines[1].depth, 1); // let x = 1;
        assert_eq!(lines[2].depth, 1); // if true {
        assert_eq!(lines[3].depth, 2); // foo();
        assert_eq!(lines[4].depth, 2); // }
        assert_eq!(lines[5].depth, 1); // }
    }

    #[test]
    fn walk_lines_detects_line_comments() {
        let content = "let x = 1;\n// this is a comment\nlet y = 2;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code);
        assert_eq!(lines[1].region, Region::LineComment);
        assert_eq!(lines[2].region, Region::Code);
    }

    #[test]
    fn walk_lines_detects_block_comments() {
        let content = "let x = 1;\n/* multi\nline\ncomment */\nlet y = 2;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code);
        assert_eq!(lines[1].region, Region::BlockComment);
        assert_eq!(lines[2].region, Region::BlockComment);
        assert_eq!(lines[3].region, Region::BlockComment);
        assert_eq!(lines[4].region, Region::Code);
    }

    #[test]
    fn depth_skips_braces_in_strings() {
        let content = "let x = \"{ not a block }\";\nlet y = 1;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        // Braces inside string should NOT change depth
        assert_eq!(lines[0].depth, 0);
        assert_eq!(lines[1].depth, 0);
    }

    #[test]
    fn php_hash_comments() {
        let content = "<?php\n# this is a comment\n$x = 1;\n";
        let grammar = php_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[1].region, Region::LineComment);
        assert_eq!(lines[2].region, Region::Code);
    }

    // ---- Extraction tests ----

    #[test]
    fn extract_rust_functions() {
        let content = "pub fn parse_config(path: &Path) -> Config {\n    todo!()\n}\n\nfn internal() {}\n\npub(crate) fn helper() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);

        let fns: Vec<_> = symbols.iter().filter(|s| s.concept == "function").collect();
        assert_eq!(fns.len(), 3);
        assert_eq!(fns[0].name(), Some("parse_config"));
        assert_eq!(fns[1].name(), Some("internal"));
        assert_eq!(fns[2].name(), Some("helper"));
    }

    #[test]
    fn extract_rust_structs() {
        let content = "pub struct Config {\n    data: String,\n}\n\nenum State {\n    Running,\n    Stopped,\n}\n";
        let grammar = rust_grammar();
        let symbols = extract_concept(content, &grammar, "struct");

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name(), Some("Config"));
        assert_eq!(symbols[1].name(), Some("State"));
    }

    #[test]
    fn extract_rust_imports() {
        let content = "use std::path::Path;\nuse crate::core::error::Result;\n\nfn foo() {}\n";
        let grammar = rust_grammar();
        let paths = import_paths(&extract(content, &grammar));

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "std::path::Path");
        assert_eq!(paths[1], "crate::core::error::Result");
    }

    #[test]
    fn extract_caches_compiled_regex_patterns() {
        let content = "pub fn cached_regex_probe() {}\n";
        let grammar = rust_grammar();
        let function_pattern = grammar.patterns.get("function").unwrap().regex.clone();

        let first = extract(content, &grammar);
        let second = extract(content, &grammar);

        assert_eq!(first.len(), second.len());
        assert!(regex_cache_has_for_tests(&function_pattern));
    }

    #[test]
    fn extract_php_methods() {
        let content = "<?php\nclass Foo {\n    public function bar() {}\n    protected function baz($x) {}\n    private function internal() {}\n}\n";
        let grammar = php_grammar();
        let methods = extract_concept(content, &grammar, "method");

        assert_eq!(methods.len(), 3);
        assert_eq!(methods[0].name(), Some("bar"));
        assert_eq!(methods[1].name(), Some("baz"));
        assert_eq!(methods[2].name(), Some("internal"));
    }

    #[test]
    fn extract_php_class() {
        let content =
            "<?php\nnamespace App\\Models;\n\nclass User {\n    public function save() {}\n}\n";
        let grammar = php_grammar();
        let symbols = extract(content, &grammar);

        let ns = namespace(&symbols);
        assert_eq!(ns, Some("App\\Models".to_string()));

        let types = type_names(&symbols);
        assert_eq!(types, vec!["User"]);
    }

    #[test]
    fn skip_comments_in_extraction() {
        let content = "// pub fn commented_out() {}\npub fn real_fn() {}\n";
        let grammar = rust_grammar();
        let symbols = extract_concept(content, &grammar, "function");

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name(), Some("real_fn"));
    }

    #[test]
    fn top_level_context_filter() {
        let content = "pub struct Outer {\n    inner: Inner,\n}\n\nimpl Outer {\n    pub struct NotTopLevel {}\n}\n";
        let grammar = rust_grammar();
        // struct pattern has context: "top_level"
        let symbols = extract_concept(content, &grammar, "struct");

        // Should only find Outer (depth 0), not NotTopLevel (depth > 0)
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name(), Some("Outer"));
    }

    #[test]
    fn method_names_helper() {
        let content = "pub fn alpha() {}\nfn beta() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);
        let names = method_names(&symbols);
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn extract_block_body_basic() {
        let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);
        let body = extract_block_body(&lines, 0, &grammar);
        assert!(body.is_some());
        let body = body.unwrap();
        assert_eq!(body.len(), 4); // All 4 lines (fn { ... })
    }

    #[test]
    fn grammar_roundtrip_toml() {
        let grammar = rust_grammar();
        let toml_str = toml::to_string_pretty(&grammar).unwrap();
        let parsed: Grammar = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.language.id, "rust");
        assert_eq!(parsed.patterns.len(), 3);
    }

    #[test]
    fn grammar_roundtrip_json() {
        let grammar = rust_grammar();
        let json_str = serde_json::to_string_pretty(&grammar).unwrap();
        let parsed: Grammar = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.language.id, "rust");
        assert_eq!(parsed.patterns.len(), 3);
    }

    #[test]
    fn structural_context_block_tracking() {
        let mut ctx = StructuralContext::new();
        ctx.depth = 1;
        ctx.push_block("impl".to_string());
        assert!(ctx.is_inside("impl"));
        assert_eq!(ctx.current_block_label(), Some("impl"));

        ctx.depth = 0;
        ctx.pop_exited_blocks();
        assert!(!ctx.is_inside("impl"));
        assert_eq!(ctx.current_block_label(), None);
    }

    #[test]
    fn public_symbols_filter() {
        let content = "pub fn visible() {}\nfn hidden() {}\npub(crate) fn semi() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);

        // All three are extracted, but public_symbols includes those without
        // visibility info (no visibility capture group → defaults to included)
        // since our simple grammar doesn't capture visibility
        let pub_syms = public_symbols(&symbols);
        assert_eq!(pub_syms.len(), 3); // All pass because no "visibility" capture
    }

    // ── Raw string region detection tests ─────────────────────────────

    #[test]
    fn walk_lines_detects_raw_string_regions() {
        let content = "fn real() {}\nlet s = r#\"\nfn fake_inside_string() {}\nanother line\n\"#;\nfn also_real() {}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code, "fn real()");
        assert_eq!(lines[1].region, Region::Code, "opening line has real code");
        assert_eq!(lines[2].region, Region::StringLiteral, "inside raw string");
        assert_eq!(lines[3].region, Region::StringLiteral, "inside raw string");
        assert_eq!(lines[4].region, Region::StringLiteral, "closing delimiter");
        assert_eq!(lines[5].region, Region::Code, "fn also_real()");
    }

    #[test]
    fn walk_lines_detects_multiline_regular_string_regions() {
        let content = "fn real() {}\nlet s = \"\\\nfn fake_inside_string() {}\nanother line\n\";\nfn also_real() {}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code, "fn real()");
        assert_eq!(lines[1].region, Region::Code, "opening line has real code");
        assert_eq!(
            lines[2].region,
            Region::StringLiteral,
            "inside regular string"
        );
        assert_eq!(
            lines[3].region,
            Region::StringLiteral,
            "inside regular string"
        );
        assert_eq!(lines[4].region, Region::StringLiteral, "closing delimiter");
        assert_eq!(lines[5].region, Region::Code, "fn also_real()");
    }

    #[test]
    fn extract_skips_functions_inside_raw_strings() {
        // Regression: the grammar extracted fn declarations from inside raw
        // strings used as test fixtures, polluting the fingerprint with fake
        // source methods like "helper", "load", "write" etc.
        let content = "pub fn real_function() -> bool {\n    true\n}\nlet fixture = r#\"\npub fn fake_function() -> bool {\n    false\n}\nfn another_fake() {}\n\"#;\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.concept == "function")
            .filter_map(|s| s.name())
            .collect();

        assert!(
            fn_names.contains(&"real_function"),
            "Should find real_function. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"fake_function"),
            "Should NOT find fake_function inside raw string. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"another_fake"),
            "Should NOT find another_fake inside raw string. Found: {:?}",
            fn_names
        );
    }

    #[test]
    fn extract_skips_functions_inside_multiline_regular_strings() {
        let content = "pub fn real_function() -> bool {\n    true\n}\nlet fixture = \"\\\npub fn fake_function() -> bool {\n    false\n}\nfn another_fake() {}\n\";\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.concept == "function")
            .filter_map(|s| s.name())
            .collect();

        assert!(
            fn_names.contains(&"real_function"),
            "Should find real_function. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"fake_function"),
            "Should NOT find fake_function inside regular string. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"another_fake"),
            "Should NOT find another_fake inside regular string. Found: {:?}",
            fn_names
        );
    }
}

// ============================================================================
// Integration tests — load real grammar files from extensions
// ============================================================================

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Load the Rust grammar from the extensions workspace and validate it
    /// against real Rust source code (this file!).
    #[test]
    fn load_and_use_rust_grammar() {
        // Try to find the Rust grammar in the extensions workspace
        let grammar_path = std::path::Path::new(
            "/var/lib/sampleplugin/workspace/homeboy-modules/rust/grammar.toml",
        );
        if !grammar_path.exists() {
            // Skip if not in development environment
            eprintln!("Skipping: Rust grammar not found at {:?}", grammar_path);
            return;
        }

        let grammar = load_grammar(grammar_path).expect("Failed to load Rust grammar");
        assert_eq!(grammar.language.id, "rust");
        assert!(grammar.patterns.contains_key("function"));
        assert!(grammar.patterns.contains_key("struct"));
        assert!(grammar.patterns.contains_key("import"));

        // Test against a sample of Rust code
        let sample = r#"
use std::path::Path;
use crate::core::error::Result;

pub struct Config {
    data: String,
}

impl Config {
    pub fn new() -> Self {
        Self { data: String::new() }
    }

    pub fn load(path: &Path) -> Result<Self> {
        todo!()
    }

    fn private_helper(&self) {}
}

pub fn standalone(x: i32) -> bool {
    x > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
"#;

        let symbols = extract(sample, &grammar);

        // Should find functions
        let fns: Vec<_> = symbols.iter().filter(|s| s.concept == "function").collect();
        assert!(
            fns.len() >= 3,
            "Expected at least 3 functions, got {}: {:?}",
            fns.len(),
            fns.iter().map(|f| f.name()).collect::<Vec<_>>()
        );

        // Should find struct
        let structs: Vec<_> = symbols.iter().filter(|s| s.concept == "struct").collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name(), Some("Config"));

        // Should find imports
        let imports: Vec<_> = symbols.iter().filter(|s| s.concept == "import").collect();
        assert_eq!(imports.len(), 2);

        // Should find impl block
        let impls: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "impl_block")
            .collect();
        assert_eq!(impls.len(), 1);

        // Should find cfg(test)
        let cfg_tests: Vec<_> = symbols.iter().filter(|s| s.concept == "cfg_test").collect();
        assert_eq!(cfg_tests.len(), 1);

        // Should find test attribute
        let test_attrs: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "test_attribute")
            .collect();
        assert_eq!(test_attrs.len(), 1);
    }

    /// Load the PHP grammar and validate it against sample PHP code.
    #[test]
    fn load_and_use_php_grammar() {
        let grammar_path = std::path::Path::new(
            "/var/lib/sampleplugin/workspace/homeboy-modules/wordpress/grammar.toml",
        );
        if !grammar_path.exists() {
            eprintln!("Skipping: PHP grammar not found at {:?}", grammar_path);
            return;
        }

        let grammar = load_grammar(grammar_path).expect("Failed to load PHP grammar");
        assert_eq!(grammar.language.id, "php");
        assert!(grammar.patterns.contains_key("method"));
        assert!(grammar.patterns.contains_key("class"));
        assert!(grammar.patterns.contains_key("namespace"));

        let sample = r#"<?php
namespace SamplePlugin\Abilities;

use WP_UnitTestCase;
use SamplePlugin\Core\Pipeline;

class PipelineAbilities extends BaseAbilities {
    public function register() {
        add_action('init', [$this, 'setup']);
    }

    public function executeCreate($config) {
        return new Pipeline($config);
    }

    protected function validate($input) {
        return true;
    }

    private function internal() {}

    public static function getInstance() {
        return new static();
    }
}
"#;

        let symbols = extract(sample, &grammar);

        // Should find namespace
        let ns = namespace(&symbols);
        assert_eq!(ns, Some("SamplePlugin\\Abilities".to_string()));

        // Should find class
        let classes: Vec<_> = symbols.iter().filter(|s| s.concept == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), Some("PipelineAbilities"));
        assert_eq!(classes[0].get("extends"), Some("BaseAbilities"));

        // Should find methods
        let methods: Vec<_> = symbols.iter().filter(|s| s.concept == "method").collect();
        assert!(
            methods.len() >= 4,
            "Expected at least 4 methods, got {}",
            methods.len()
        );

        // Should find imports
        let imports: Vec<_> = symbols.iter().filter(|s| s.concept == "import").collect();
        assert_eq!(imports.len(), 2);

        // Should find add_action
        let actions: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "add_action")
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name(), Some("init"));
    }
}
