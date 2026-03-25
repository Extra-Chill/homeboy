//! Grammar-driven item parsing — extract top-level items with full boundaries.
//!
//! This module builds on the grammar engine (`utils/grammar.rs`) to produce
//! `GrammarItem`s — complete items with start/end lines and source text.
//! It replaces the extension-side `parse_items` command for languages that
//! have a grammar.toml.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs       (patterns, symbols, walk_lines)
//!     ↓
//! utils/grammar_items.rs (this file: item boundaries, source extraction)
//!     ↓
//! core/refactor/         (decompose, move — consume GrammarItems)
//! ```

mod boundary_detection;
mod helpers;
mod types;

pub use boundary_detection::*;
pub use helpers::*;
pub use types::*;


use serde::{Deserialize, Serialize};

use super::grammar::{self, Grammar, Symbol};

// ============================================================================
// Types
// ============================================================================

// ============================================================================
// Core parse_items
// ============================================================================

// ============================================================================
// Boundary detection
// ============================================================================

// ============================================================================
// Grammar-aware brace matching
// ============================================================================

    lines.len() - 1
}

// ============================================================================
// Test module detection
// ============================================================================

// ============================================================================
// Helpers
// ============================================================================

    depth == 0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::extension::grammar::{
        BlockSyntax, CommentSyntax, ConceptPattern, Grammar, LanguageMeta, StringSyntax,
    };

    /// Build a full Rust grammar with all item-relevant patterns.
    fn full_rust_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
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
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "function".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c.insert("params".to_string(), 3);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "struct".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("kind".to_string(), 2);
                            c.insert("name".to_string(), 3);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "impl_block".to_string(),
                    ConceptPattern {
                        regex: r"^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("trait_name".to_string(), 1);
                            c.insert("type_name".to_string(), 2);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "const_static".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?(const|static)\s+(\w+)\s*:".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("kind".to_string(), 2);
                            c.insert("name".to_string(), 3);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "type_alias".to_string(),
                    ConceptPattern {
                        regex: r"^\s*(pub(?:\(crate\))?\s+)?type\s+(\w+)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("visibility".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c
                        },
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
    fn parse_items_basic() {
        let content = "\
pub fn hello() {
    println!(\"hi\");
}

struct Foo {
    x: i32,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].kind, "function");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(items[0].end_line, 3);

        assert_eq!(items[1].name, "Foo");
        assert_eq!(items[1].kind, "struct");
        assert_eq!(items[1].start_line, 5);
        assert_eq!(items[1].end_line, 7);
    }

    #[test]
    fn parse_items_with_doc_comments() {
        let content = "\
/// This function does stuff.
/// It's important.
pub fn documented() {
    todo!()
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "documented");
        assert_eq!(items[0].start_line, 1); // includes doc comments
        assert_eq!(items[0].end_line, 5);
        assert!(items[0].source.starts_with("/// This function"));
    }

    #[test]
    fn parse_items_with_attributes() {
        let content = "\
#[derive(Debug, Clone)]
#[serde(rename_all = \"camelCase\")]
pub struct Config {
    pub name: String,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Config");
        assert_eq!(items[0].start_line, 1); // includes attributes
        assert_eq!(items[0].end_line, 5);
    }

    #[test]
    fn parse_items_skips_test_module() {
        let content = "\
pub fn real_fn() {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_something() {
        assert!(true);
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "real_fn");
    }

    #[test]
    fn parse_items_impl_block() {
        let content = "\
pub struct Foo {}

impl Foo {
    pub fn new() -> Self {
        Foo {}
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Foo");
        assert_eq!(items[0].kind, "struct");
        assert_eq!(items[1].name, "Foo");
        assert_eq!(items[1].kind, "impl");
        assert_eq!(items[1].start_line, 3);
        assert_eq!(items[1].end_line, 7);
    }

    #[test]
    fn parse_items_trait_impl() {
        let content = "\
impl Display for Foo {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, \"Foo\")
    }
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Display for Foo");
        assert_eq!(items[0].kind, "impl");
    }

    #[test]
    fn parse_items_const_and_type_alias() {
        let content = "\
pub const MAX_SIZE: usize = 1024;

pub type Result<T> = std::result::Result<T, Error>;

pub fn process() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "MAX_SIZE");
        assert_eq!(items[0].kind, "const");
        assert_eq!(items[1].name, "Result");
        assert_eq!(items[1].kind, "type_alias");
        assert_eq!(items[2].name, "process");
        assert_eq!(items[2].kind, "function");
    }

    #[test]
    fn parse_items_const_array_multiline() {
        // Regression test for #841: const arrays with type annotations containing
        // semicolons (e.g., `[&str; 8]`) were terminated at the type annotation
        // instead of the actual closing `];`.
        let content = "\
const NOISY_DIRS: [&str; 4] = [
    \"node_modules\",
    \"dist\",
    \"vendor\",
    \"target\",
];

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(
            items.len(),
            2,
            "Should find const + function, got: {:?}",
            items
                .iter()
                .map(|i| (&i.name, &i.kind, i.start_line, i.end_line))
                .collect::<Vec<_>>()
        );
        assert_eq!(items[0].name, "NOISY_DIRS");
        assert_eq!(items[0].kind, "const");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(
            items[0].end_line, 6,
            "const array should end at `];` line (6), not at type annotation line (1)"
        );
        assert!(
            items[0].source.contains("\"target\""),
            "source should include all array elements"
        );
        assert!(
            items[0].source.ends_with("];"),
            "source should end with `];`, got: ...{}",
            &items[0].source[items[0].source.len().saturating_sub(20)..]
        );
        assert_eq!(items[1].name, "after");
        assert_eq!(items[1].kind, "function");
    }

    #[test]
    fn parse_items_const_with_braces() {
        // Const with brace-delimited initializer (e.g., HashMap literal via macro)
        let content = "\
pub static DEFAULTS: phf::Map<&str, i32> = phf::phf_map! {
    \"a\" => 1,
    \"b\" => 2,
};

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "DEFAULTS");
        assert_eq!(items[0].kind, "static");
        assert_eq!(
            items[0].end_line, 4,
            "static with braces should end at closing line"
        );
    }

    #[test]
    fn parse_items_braces_in_string() {
        let test_content =
            "pub fn string_test() {\n    let s = \"{ not a brace }\";\n    do_stuff();\n}\n\npub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(test_content, &grammar);

        assert_eq!(
            items.len(),
            2,
            "Should find 2 functions despite string braces"
        );
        assert_eq!(items[0].name, "string_test");
        assert_eq!(items[0].end_line, 4);
        assert_eq!(items[1].name, "after");
    }

    #[test]
    fn parse_items_enum_variants() {
        let content = "\
pub enum Color {
    Red,
    Green,
    Blue,
}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Color");
        assert_eq!(items[0].kind, "enum");
        assert_eq!(items[0].start_line, 1);
        assert_eq!(items[0].end_line, 5);
    }

    #[test]
    fn parse_items_unit_struct() {
        let content = "\
pub struct Marker;

pub fn after() {}";
        let grammar = full_rust_grammar();
        let items = parse_items(content, &grammar);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Marker");
        assert_eq!(items[0].kind, "struct");
        assert_eq!(items[0].end_line, 1);
        assert_eq!(items[1].name, "after");
    }

    #[test]
    fn validate_brace_balance_works() {
        let grammar = full_rust_grammar();
        assert!(validate_brace_balance("fn foo() { bar() }", &grammar));
        assert!(validate_brace_balance(
            "fn foo() {\n    if true {\n        bar()\n    }\n}",
            &grammar
        ));
        assert!(!validate_brace_balance("fn foo() {", &grammar));
        assert!(!validate_brace_balance("fn foo() { { }", &grammar));
    }

    #[test]
    fn validate_brace_balance_char_literals() {
        let grammar = full_rust_grammar();
        // Char literal containing close brace — should NOT count as a real brace
        assert!(validate_brace_balance(
            "fn foo() { let c = '}'; }",
            &grammar
        ));
        // Char literal containing open brace
        assert!(validate_brace_balance(
            "fn foo() { let c = '{'; }",
            &grammar
        ));
        // Escaped char literal (backslash)
        assert!(validate_brace_balance(
            "fn foo() { let c = '\\\\'; }",
            &grammar
        ));
        // Escaped single quote char literal
        assert!(validate_brace_balance(
            "fn foo() { let c = '\\''; }",
            &grammar
        ));
        // rfind pattern that triggered the original bug
        assert!(validate_brace_balance(
            "fn insert_before_closing_brace(content: &str) {\n    content.rfind('}');\n}",
            &grammar
        ));
    }

    #[test]
    fn validate_brace_balance_raw_strings() {
        let grammar = full_rust_grammar();
        // Multi-line raw string containing braces — should NOT count as real braces
        assert!(validate_brace_balance(
            "fn foo() {\n    let s = r#\"\npub struct Bar {}\n\"#;\n}",
            &grammar
        ));
        // Single-line raw string with braces
        assert!(validate_brace_balance(
            "fn foo() { let s = r#\"{ not a brace }\"#; }",
            &grammar
        ));
        // Raw string with unbalanced braces inside (should still be balanced overall)
        assert!(validate_brace_balance(
            "fn foo() {\n    let s = r#\"{\n{\n{\"#;\n}",
            &grammar
        ));
    }

    #[test]
    fn find_matching_brace_skips_raw_strings() {
        let grammar = full_rust_grammar();
        // mod tests block with raw strings containing braces inside
        let content = "\
mod tests {
    fn test_something() {
        let s = r#\"
pub struct Fake {}
fn inner() { }
\"#;
        assert!(true);
    }
}

fn after() {}";
        let lines: Vec<&str> = content.lines().collect();
        let end = find_matching_brace(&lines, 0, &grammar);
        // The closing brace of `mod tests` is line 8 (0-indexed)
        // Without raw string handling, the braces inside r#"..."# would corrupt depth
        assert_eq!(
            end, 8,
            "Should find closing brace of mod tests, not be confused by raw string braces"
        );
    }
}
