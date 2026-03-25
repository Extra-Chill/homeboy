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

mod block_syntax;
mod convenience_helpers_feature;
mod extraction_apply_grammar;
mod grammar_definition_loaded;
mod grammar_loading;
mod structural_context;
mod structural_parser_context;
mod symbol;
mod types;

pub use block_syntax::*;
pub use convenience_helpers_feature::*;
pub use extraction_apply_grammar::*;
pub use grammar_definition_loaded::*;
pub use grammar_loading::*;
pub use structural_context::*;
pub use structural_parser_context::*;
pub use symbol::*;
pub use types::*;


use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::core::defaults::default_true;
use crate::engine::local_files;
use crate::error::{Error, Result};

// ============================================================================
// Grammar definition (loaded from extension TOML/JSON)
// ============================================================================

// ============================================================================
// Structural parser — context-aware iteration over source text
// ============================================================================

impl StructuralContext {
    pub fn new() -> Self {
        Self {
            depth: 0,
            region: Region::Code,
            block_stack: Vec::new(),
        }
    }

    /// Whether we're inside a block with the given label.
    #[cfg(test)]
    pub(crate) fn is_inside(&self, label: &str) -> bool {
        self.block_stack.iter().any(|(l, _)| l == label)
    }

    /// The label of the innermost block, if any.
    #[cfg(test)]
    pub(crate) fn current_block_label(&self) -> Option<&str> {
        self.block_stack.last().map(|(l, _)| l.as_str())
    }

    /// Push a labeled block at the current depth.
    #[cfg(test)]
    pub(crate) fn push_block(&mut self, label: String) {
        self.block_stack.push((label, self.depth));
    }

    /// Pop blocks that have been exited (depth dropped below entry depth).
    pub(crate) fn pop_exited_blocks(&mut self) {
        while let Some((_, entry_depth)) = self.block_stack.last() {
            if self.depth <= *entry_depth {
                self.block_stack.pop();
            } else {
                break;
            }
        }
    }
}

/// Iterate lines with structural context, tracking brace depth and regions.
///
/// This is the core primitive — it walks the file line-by-line, tracking
/// brace depth and whether we're inside comments or strings. Consumers
/// can then filter lines by depth, region, etc.
pub(crate) fn walk_lines<'a>(content: &'a str, grammar: &Grammar) -> Vec<ContextualLine<'a>> {
    let mut ctx = StructuralContext::new();
    let mut result = Vec::new();
    let mut in_block_comment = false;
    let mut block_comment_end = String::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let depth_at_start = ctx.depth;

        // Determine region for this line
        let region = if in_block_comment {
            // Check if block comment ends on this line
            if let Some(pos) = trimmed.find(block_comment_end.as_str()) {
                // Comment ends partway through this line
                in_block_comment = false;
                let after = &trimmed[pos + block_comment_end.len()..].trim();
                if after.is_empty() {
                    Region::BlockComment
                } else {
                    // Mixed line — treat as code (conservative)
                    Region::Code
                }
            } else {
                Region::BlockComment
            }
        } else if is_line_comment(trimmed, &grammar.comments) {
            Region::LineComment
        } else {
            // Check for block comment start
            for (open, close) in &grammar.comments.block {
                if trimmed.starts_with(open.as_str())
                    && (!trimmed.contains(close.as_str()) || trimmed.ends_with(open.as_str()))
                {
                    in_block_comment = true;
                    block_comment_end = close.clone();
                }
            }
            if in_block_comment {
                Region::BlockComment
            } else {
                Region::Code
            }
        };

        // Track brace depth for code lines
        if region == Region::Code {
            update_depth(line, &grammar.blocks, &grammar.strings, &mut ctx);
        }

        result.push(ContextualLine {
            text: line,
            line_num: i + 1,
            depth: depth_at_start,
            region,
        });

        // Pop exited blocks
        ctx.pop_exited_blocks();
    }

    result
}

// ============================================================================
// Extraction — apply grammar patterns to get symbols
// ============================================================================

impl Symbol {
    /// Get a named capture value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.captures.get(key).map(|s| s.as_str())
    }

    /// Get the "name" capture (most symbols have one).
    pub fn name(&self) -> Option<&str> {
        self.get("name")
    }

    /// Get the "visibility" capture.
    pub fn visibility(&self) -> Option<&str> {
        self.get("visibility")
    }
}

// ============================================================================
// Grammar loading
// ============================================================================

// ============================================================================
// Convenience helpers for feature consumers
// ============================================================================

// ============================================================================
// Block body extraction
// ============================================================================

/// Extract the body of a block starting from a given line.
///
/// Finds the opening brace on or after `start_line` (0-indexed into lines),
/// then returns all lines until the matching closing brace.
#[cfg(test)]
pub(crate) fn extract_block_body<'a>(
    lines: &[ContextualLine<'a>],
    start_line_idx: usize,
    grammar: &Grammar,
) -> Option<Vec<&'a str>> {
    let open = grammar.blocks.open.chars().next().unwrap_or('{');
    let close = grammar.blocks.close.chars().next().unwrap_or('}');

    // Find the opening brace
    let mut idx = start_line_idx;
    let mut found_open = false;
    let mut depth: i32 = 0;
    let mut body_lines = Vec::new();

    while idx < lines.len() {
        let line = lines[idx].text;
        for ch in line.chars() {
            if ch == open {
                depth += 1;
                found_open = true;
            } else if ch == close {
                depth -= 1;
                if found_open && depth == 0 {
                    body_lines.push(line);
                    return Some(body_lines);
                }
            }
        }
        if found_open {
            body_lines.push(line);
        }
        idx += 1;
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_grammar() -> Grammar {
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
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c.insert("params".to_string(), 2);
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
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+(\w+)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c
                        },
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
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("path".to_string(), 1);
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

    fn php_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "php".to_string(),
                extensions: vec!["php".to_string()],
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
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "method".to_string(),
                    ConceptPattern {
                        regex: r"(?:(?:public|protected|private|static|abstract|final)\s+)*function\s+(\w+)\s*\(([^)]*)\)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c.insert("params".to_string(), 2);
                            c
                        },
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
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("kind".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c
                        },
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
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
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
        let content = "use std::path::Path;\nuse crate::error::Result;\n\nfn foo() {}\n";
        let grammar = rust_grammar();
        let paths = import_paths(&extract(content, &grammar));

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "std::path::Path");
        assert_eq!(paths[1], "crate::error::Result");
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
    fn load_and_use_php_grammar() {
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
}
