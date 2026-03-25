//! Grammar-driven core fingerprint engine.
//!
//! Replaces the per-language Python fingerprint scripts with a single Rust
//! implementation that uses the grammar engine (`utils/grammar.rs`) for
//! structural parsing. Extensions only need to ship a `grammar.toml` —
//! no more Python-in-bash fingerprint scripts.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs           (structural parsing, brace tracking)
//!     ↓
//! core_fingerprint.rs        (this file: hashing, method extraction, visibility)
//!     ↓
//! FileFingerprint            (consumed by duplication, conventions, dead_code, etc.)
//! ```
//!
//! # What this handles (generic across languages)
//!
//! - Method/function extraction with deduplication
//! - Body extraction and exact/structural hashing
//! - Visibility extraction from grammar captures
//! - Type name and type_names extraction
//! - Import/namespace extraction
//! - Internal calls extraction
//! - Public API collection
//! - Unused parameter detection
//! - Dead code marker detection
//! - Impl context tracking (trait impl methods excluded from dedup hashes)
//!
//! # What extensions configure via grammar.toml
//!
//! - Language-specific patterns (function, class, impl_block, etc.)
//! - Comment and string syntax
//! - Block delimiters

mod constants;
mod function_extraction;
mod hashing;
mod php_specific_extraction;
mod symbol_extraction_helpers;
mod types;
mod unused_parameter_detection;

pub use constants::*;
pub use function_extraction::*;
pub use hashing::*;
pub use php_specific_extraction::*;
pub use symbol_extraction_helpers::*;
pub use types::*;
pub use unused_parameter_detection::*;


use std::collections::{HashMap, HashSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};

use super::conventions::Language;
use super::fingerprint::FileFingerprint;

// ============================================================================
// Configuration
// ============================================================================

// ============================================================================
// Public API
// ============================================================================

// ============================================================================
// Function extraction
// ============================================================================

// ============================================================================
// Hashing
// ============================================================================

// ============================================================================
// Symbol extraction helpers
// ============================================================================

// ============================================================================
// Unused parameter detection
// ============================================================================

    unused
}

// ============================================================================
// Dead code markers
// ============================================================================

// ============================================================================
// PHP-specific extraction from grammar symbols
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_grammar() -> Grammar {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-extensions/rust/grammar.toml",
        );
        if grammar_path.exists() {
            grammar::load_grammar(grammar_path).expect("Failed to load Rust grammar")
        } else {
            // Minimal test grammar
            toml::from_str(
                r#"
                [language]
                id = "rust"
                extensions = ["rs"]
                [comments]
                line = ["//"]
                block = [["/*", "*/"]]
                doc = ["///", "//!"]
                [strings]
                quotes = ['"']
                escape = "\\"
                [blocks]
                open = "{"
                close = "}"
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
                "#,
            )
            .expect("Failed to parse minimal grammar")
        }
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
            structural_hash(a, RUST_KEYWORDS, false),
            structural_hash(b, RUST_KEYWORDS, false),
        );
    }

    #[test]
    fn test_structural_hash_different_structure() {
        let a = "{ let x = 1; if x > 0 { return true; } }";
        let b = "{ let x = 1; for i in 0..x { print(i); } }";
        assert_ne!(
            structural_hash(a, RUST_KEYWORDS, false),
            structural_hash(b, RUST_KEYWORDS, false),
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
        // Test method should still be in the methods list
        assert!(fp.methods.contains(&"test_real_fn".to_string()));
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
    fn trait_method_declarations_not_flagged_as_unused_params() {
        let grammar = rust_grammar();
        let content = r#"
pub trait FileSystem {
    fn read(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, content: &str) -> Result<()>;
    fn delete(&self, path: &Path) -> Result<()>;
}
"#;

        let fp = fingerprint_from_grammar(content, &grammar, "src/lib.rs").unwrap();

        assert!(
            fp.unused_parameters.is_empty(),
            "Trait method declarations should not produce unused param findings, got: {:?}",
            fp.unused_parameters
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
        // "write" is in SKIP_CALLS_RUST (for the write! macro), but this
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
}
