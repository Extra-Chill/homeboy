//! integration_tests_load — extracted from grammar.rs.

use std::path::Path;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::error::{Error, Result};


    /// Load the Rust grammar from the extensions workspace and validate it
    /// against real Rust source code (this file!).
    #[test]
    pub(crate) fn load_and_use_rust_grammar() {
        // Try to find the Rust grammar in the extensions workspace
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/rust/grammar.toml",
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
use crate::error::Result;

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
    pub(crate) fn load_and_use_php_grammar() {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/wordpress/grammar.toml",
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
namespace DataMachine\Abilities;

use WP_UnitTestCase;
use DataMachine\Core\Pipeline;

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
        assert_eq!(ns, Some("DataMachine\\Abilities".to_string()));

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
