//! Test scaffold — generate test stubs from source file conventions.
//!
//! Reads a source file, extracts its public API (methods, functions),
//! and generates a test file with one stub per public method. The output
//! follows project conventions for test file naming, base classes, and
//! assertion style.
//!
//! Supports two extraction modes:
//! - Grammar-based: uses extension-provided grammar.toml (preferred)
//! - Legacy regex: hardcoded patterns as fallback

use std::collections::HashSet;

use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::utils::{grammar, io};

// ============================================================================
// Models
// ============================================================================

/// A public method/function extracted from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedMethod {
    /// Method/function name.
    pub name: String,
    /// Visibility (public, protected, pub, pub(crate)).
    pub visibility: String,
    /// Whether it's static.
    pub is_static: bool,
    /// Line number in source file.
    pub line: usize,
    /// Parameters (raw string, not parsed).
    pub params: String,
}

/// Extracted class/struct info from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedClass {
    /// Class/struct/trait name.
    pub name: String,
    /// Namespace (PHP) or module path (Rust).
    pub namespace: String,
    /// Kind: class, trait, interface, struct, enum.
    pub kind: String,
    /// Public methods.
    pub methods: Vec<ExtractedMethod>,
}

/// Configuration for scaffold generation.
#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    /// Base test class to extend (e.g., "WP_UnitTestCase", "TestCase").
    pub base_class: String,
    /// Base test class import.
    pub base_class_import: String,
    /// Test method prefix (e.g., "test_").
    pub test_prefix: String,
    /// Body for incomplete tests.
    pub incomplete_body: String,
    /// Language: "php" or "rust".
    pub language: String,
}

impl ScaffoldConfig {
    /// WordPress/PHPUnit defaults.
    pub fn php() -> Self {
        Self {
            base_class: "WP_UnitTestCase".to_string(),
            base_class_import: "WP_UnitTestCase".to_string(),
            test_prefix: "test_".to_string(),
            incomplete_body: "$this->markTestIncomplete('TODO: implement');".to_string(),
            language: "php".to_string(),
        }
    }

    /// Rust defaults.
    pub fn rust() -> Self {
        Self {
            base_class: String::new(),
            base_class_import: String::new(),
            test_prefix: "test_".to_string(),
            incomplete_body: "todo!(\"implement test\");".to_string(),
            language: "rust".to_string(),
        }
    }
}

/// Result of scaffold generation.
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldResult {
    /// Source file analyzed.
    pub source_file: String,
    /// Generated test file path.
    pub test_file: String,
    /// Number of test stubs generated.
    pub stub_count: usize,
    /// The generated test file content.
    pub content: String,
    /// Whether the file was written to disk.
    pub written: bool,
    /// Whether a test file already existed (skipped).
    pub skipped: bool,
    /// Classes/structs found in source.
    pub classes: Vec<ExtractedClass>,
}

/// Result of scaffolding multiple files.
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldBatchResult {
    /// Individual results per source file.
    pub results: Vec<ScaffoldResult>,
    /// Total test stubs generated.
    pub total_stubs: usize,
    /// Total files written.
    pub total_written: usize,
    /// Total files skipped (test already exists).
    pub total_skipped: usize,
}

fn is_low_signal_test_name(name: &str) -> bool {
    matches!(name, "test_run" | "test_new" | "test_validate")
}

const MAX_AUTO_SCAFFOLD_STUBS: usize = 12;

fn generated_test_names(classes: &[ExtractedClass], config: &ScaffoldConfig) -> Vec<String> {
    let mut emitted = HashSet::new();

    classes
        .iter()
        .flat_map(|class| {
            class
                .methods
                .iter()
                .filter(|method| method.name != "__construct")
                .map(|method| {
                    if config.language == "rust" {
                        if class.name.is_empty() {
                            format!("test_{}", to_snake_case(&method.name))
                        } else {
                            format!(
                                "test_{}_{}",
                                to_snake_case(&class.name),
                                to_snake_case(&method.name)
                            )
                        }
                    } else {
                        format!("{}{}", config.test_prefix, to_snake_case(&method.name))
                    }
                })
        })
        .filter(|name| emitted.insert(name.clone()))
        .collect()
}

fn passes_scaffold_quality_gate(test_names: &[String]) -> bool {
    if test_names.is_empty() {
        return false;
    }

    if test_names.len() > MAX_AUTO_SCAFFOLD_STUBS {
        return false;
    }

    let low_signal = test_names
        .iter()
        .filter(|name| is_low_signal_test_name(name))
        .count();
    let meaningful = test_names.len().saturating_sub(low_signal);

    if meaningful == 0 {
        return false;
    }

    if test_names.len() >= 3 && low_signal > meaningful {
        return false;
    }

    true
}

// ============================================================================
// Source extraction
// ============================================================================

/// Extract classes and their public methods from a PHP source file.
pub fn extract_php(content: &str) -> Vec<ExtractedClass> {
    let mut classes = Vec::new();

    // Match namespace
    let ns_re = Regex::new(r"(?m)^namespace\s+([\w\\]+);").unwrap();
    let namespace = ns_re
        .captures(content)
        .map(|c| c[1].to_string())
        .unwrap_or_default();

    // Match class/trait/interface declarations
    let class_re =
        Regex::new(r"(?m)^(?:abstract\s+)?(?:final\s+)?(class|trait|interface)\s+(\w+)").unwrap();

    for cap in class_re.captures_iter(content) {
        let kind = cap[1].to_string();
        let name = cap[2].to_string();

        // Extract methods for this class
        let methods = extract_php_methods(content);

        classes.push(ExtractedClass {
            name,
            namespace: namespace.clone(),
            kind,
            methods,
        });
    }

    // If no class found but there are functions, treat as procedural
    if classes.is_empty() {
        let methods = extract_php_functions(content);
        if !methods.is_empty() {
            classes.push(ExtractedClass {
                name: String::new(),
                namespace: namespace.clone(),
                kind: "procedural".to_string(),
                methods,
            });
        }
    }

    classes
}

/// Extract public/protected methods from PHP content.
fn extract_php_methods(content: &str) -> Vec<ExtractedMethod> {
    let method_re = Regex::new(
        r"(?m)^\s*((?:(?:public|protected|private|static|abstract|final)\s+)*)function\s+(\w+)\s*\(([^)]*)\)"
    ).unwrap();

    let mut methods = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(cap) = method_re.captures(line) {
            let modifiers = cap[1].to_string();
            let name = cap[2].to_string();
            let params = cap[3].trim().to_string();

            // Skip magic methods except __construct
            if name.starts_with("__") && name != "__construct" {
                continue;
            }

            let visibility = if modifiers.contains("private") {
                "private"
            } else if modifiers.contains("protected") {
                "protected"
            } else {
                "public"
            };

            // Only scaffold public and protected methods
            if visibility == "private" {
                continue;
            }

            let is_static = modifiers.contains("static");

            methods.push(ExtractedMethod {
                name,
                visibility: visibility.to_string(),
                is_static,
                line: i + 1,
                params,
            });
        }
    }

    methods
}

/// Extract top-level functions from PHP content (procedural files).
fn extract_php_functions(content: &str) -> Vec<ExtractedMethod> {
    let fn_re = Regex::new(r"(?m)^function\s+(\w+)\s*\(([^)]*)\)").unwrap();
    let mut methods = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(cap) = fn_re.captures(line) {
            methods.push(ExtractedMethod {
                name: cap[1].to_string(),
                visibility: "public".to_string(),
                is_static: false,
                line: i + 1,
                params: cap[2].trim().to_string(),
            });
        }
    }

    methods
}

/// Extract public functions/methods from a Rust source file.
pub fn extract_rust(content: &str) -> Vec<ExtractedClass> {
    let mut classes = Vec::new();

    // Match struct/enum/trait with impl blocks
    let struct_re =
        Regex::new(r"(?m)^(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+(\w+)").unwrap();

    for cap in struct_re.captures_iter(content) {
        let name = cap[1].to_string();
        let methods = extract_rust_impl_methods(content, &name);

        if !methods.is_empty() {
            classes.push(ExtractedClass {
                name,
                namespace: String::new(),
                kind: "struct".to_string(),
                methods,
            });
        }
    }

    // Also get free functions
    let free_fns = extract_rust_free_functions(content);
    if !free_fns.is_empty() {
        classes.push(ExtractedClass {
            name: String::new(),
            namespace: String::new(),
            kind: "module".to_string(),
            methods: free_fns,
        });
    }

    classes
}

/// Extract pub methods from an impl block for a specific type.
fn extract_rust_impl_methods(content: &str, type_name: &str) -> Vec<ExtractedMethod> {
    let impl_re = Regex::new(&format!(
        r"impl(?:<[^>]*>)?\s+{}\b",
        regex::escape(type_name)
    ))
    .unwrap();
    let fn_re = Regex::new(r"(?m)^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)")
        .unwrap();

    let mut methods = Vec::new();
    let mut in_impl = false;
    let mut brace_depth: i32 = 0;

    for (i, line) in content.lines().enumerate() {
        if !in_impl {
            if impl_re.is_match(line) {
                in_impl = true;
                brace_depth = 0;
            }
            if !in_impl {
                continue;
            }
        }

        // Track brace depth
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
            } else if ch == '}' {
                brace_depth -= 1;
                if brace_depth <= 0 {
                    in_impl = false;
                }
            }
        }

        // Match function declarations inside the impl block
        if let Some(cap) = fn_re.captures(line) {
            let vis = cap.get(1).map_or("", |m| m.as_str().trim());
            let name = cap[2].to_string();
            let params = cap[3].trim().to_string();

            // Only pub functions
            if !vis.starts_with("pub") {
                continue;
            }

            // Skip test functions
            if name.starts_with("test_") {
                continue;
            }

            methods.push(ExtractedMethod {
                name,
                visibility: vis.to_string(),
                is_static: !params.contains("self"),
                line: i + 1,
                params,
            });
        }
    }

    methods
}

/// Extract top-level pub functions (not in impl blocks).
fn extract_rust_free_functions(content: &str) -> Vec<ExtractedMethod> {
    let fn_re = Regex::new(r"(?m)^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)")
        .unwrap();

    let mut methods = Vec::new();
    let mut pending_test_attribute = false;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("#[") {
            if trimmed.contains("test") {
                pending_test_attribute = true;
            }
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        if let Some(cap) = fn_re.captures(line) {
            let visibility = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let is_test_function = pending_test_attribute;
            pending_test_attribute = false;

            // Keep existing behavior for public functions, but also include
            // test-annotated private functions so scaffold can mirror existing
            // inline test names into dedicated test files when needed.
            if !visibility.starts_with("pub") && !is_test_function {
                continue;
            }

            let name = cap[2].to_string();
            let params = cap[3].trim().to_string();

            // Skip test functions and main
            if name.starts_with("test_") || name == "main" {
                continue;
            }

            methods.push(ExtractedMethod {
                name,
                visibility: if visibility.is_empty() {
                    "private".to_string()
                } else {
                    visibility.trim().to_string()
                },
                is_static: true,
                line: i + 1,
                params,
            });
        } else {
            pending_test_attribute = false;
        }
    }

    methods
}

// ============================================================================
// Test file generation
// ============================================================================

/// Determine the test file path for a given source file.
pub fn test_file_path(source_path: &Path, root: &Path) -> PathBuf {
    let relative = source_path.strip_prefix(root).unwrap_or(source_path);
    let rel_str = relative.to_string_lossy();

    // PHP: src/Abilities/Foo.php → tests/Unit/Abilities/FooTest.php
    // PHP: inc/Abilities/Foo.php → tests/Unit/Abilities/FooTest.php
    if rel_str.ends_with(".php") {
        let stripped = rel_str
            .strip_prefix("src/")
            .or_else(|| rel_str.strip_prefix("inc/"))
            .or_else(|| rel_str.strip_prefix("lib/"))
            .unwrap_or(&rel_str);

        let without_ext = stripped.strip_suffix(".php").unwrap_or(stripped);
        return root.join(format!("tests/Unit/{}Test.php", without_ext));
    }

    // Rust: src/core/foo.rs → tests/core/foo_test.rs
    if rel_str.ends_with(".rs") {
        let stripped = rel_str.strip_prefix("src/").unwrap_or(&rel_str);
        let without_ext = stripped.strip_suffix(".rs").unwrap_or(stripped);
        return root.join(format!("tests/{}_test.rs", without_ext));
    }

    // Fallback
    root.join("tests").join(relative)
}

/// Generate PHP test file content.
pub fn generate_php_test(classes: &[ExtractedClass], config: &ScaffoldConfig) -> String {
    let mut out = String::new();
    let mut emitted = HashSet::new();
    out.push_str("<?php\n");

    for class in classes {
        if class.kind == "procedural" {
            // Procedural test file
            if !class.namespace.is_empty() {
                out.push_str(&format!("namespace {}\\Tests;\n\n", class.namespace));
            }
            out.push_str(&format!("use {};\n\n", config.base_class_import));

            out.push_str(&format!(
                "class FunctionsTest extends {} {{\n\n",
                config.base_class
            ));

            let test_names: Vec<String> = class
                .methods
                .iter()
                .map(|method| format!("{}{}", config.test_prefix, to_snake_case(&method.name)))
                .filter(|name| emitted.insert(name.clone()))
                .collect();

            for test_name in test_names {
                out.push_str(&format!(
                    "    public function {}() {{\n        {}\n    }}\n\n",
                    test_name, config.incomplete_body
                ));
            }

            out.push_str("}\n");
            continue;
        }

        // Class-based test file
        let test_namespace = if !class.namespace.is_empty() {
            format!("{}\\Tests\\Unit", namespace_root(&class.namespace))
        } else {
            String::new()
        };

        if !test_namespace.is_empty() {
            out.push_str(&format!("namespace {};\n\n", test_namespace));
        }

        // Imports
        out.push_str(&format!("use {};\n", config.base_class_import));
        if !class.namespace.is_empty() {
            out.push_str(&format!("use {}\\{};\n", class.namespace, class.name));
        }
        out.push('\n');

        // Class doc
        if !class.namespace.is_empty() {
            out.push_str(&format!(
                "/**\n * @covers \\{}\\{}\n */\n",
                class.namespace, class.name
            ));
        }

        // Class declaration
        out.push_str(&format!(
            "class {}Test extends {} {{\n\n",
            class.name, config.base_class
        ));

        // Test methods
        let test_names: Vec<String> = class
            .methods
            .iter()
            .filter(|method| method.name != "__construct")
            .map(|method| format!("{}{}", config.test_prefix, to_snake_case(&method.name)))
            .filter(|name| emitted.insert(name.clone()))
            .collect();

        for test_name in test_names {
            out.push_str(&format!(
                "    public function {}() {{\n        {}\n    }}\n\n",
                test_name, config.incomplete_body
            ));
        }

        out.push_str("}\n");
    }

    out
}

/// Generate Rust test file content.
pub fn generate_rust_test(classes: &[ExtractedClass], _config: &ScaffoldConfig) -> String {
    let mut out = String::new();
    let mut emitted = HashSet::new();

    // Module-level test block
    out.push_str("#[cfg(test)]\nmod tests {\n    use super::*;\n\n");

    for class in classes {
        if !class.name.is_empty() {
            out.push_str(&format!("    // Tests for {}\n\n", class.name));
        }

        let test_names: Vec<String> = class
            .methods
            .iter()
            .map(|method| {
                if class.name.is_empty() {
                    format!("test_{}", to_snake_case(&method.name))
                } else {
                    format!(
                        "test_{}_{}",
                        to_snake_case(&class.name),
                        to_snake_case(&method.name)
                    )
                }
            })
            .filter(|name| emitted.insert(name.clone()))
            .collect();

        for test_name in test_names {
            out.push_str(&format!(
                "    #[test]\n    fn {}() {{\n        todo!(\"implement test\");\n    }}\n\n",
                test_name
            ));
        }
    }

    out.push_str("}\n");
    out
}

/// Scaffold tests for a single source file.
pub fn scaffold_file(
    source_path: &Path,
    root: &Path,
    config: &ScaffoldConfig,
    write: bool,
) -> Result<ScaffoldResult> {
    let relative = source_path
        .strip_prefix(root)
        .unwrap_or(source_path)
        .to_string_lossy()
        .to_string();

    let content = io::read_file(source_path, "read source file")?;

    let classes = if config.language == "rust" {
        extract_rust(&content)
    } else {
        extract_php(&content)
    };

    let test_path = test_file_path(source_path, root);
    let test_relative = test_path
        .strip_prefix(root)
        .unwrap_or(&test_path)
        .to_string_lossy()
        .to_string();

    // Check if test file already exists
    if test_path.exists() {
        return Ok(ScaffoldResult {
            source_file: relative,
            test_file: test_relative,
            stub_count: 0,
            content: String::new(),
            written: false,
            skipped: true,
            classes,
        });
    }

    let generated_names = generated_test_names(&classes, config);
    let stub_count = generated_names.len();

    if !passes_scaffold_quality_gate(&generated_names) {
        return Ok(ScaffoldResult {
            source_file: relative,
            test_file: test_relative,
            stub_count: 0,
            content: String::new(),
            written: false,
            skipped: false,
            classes,
        });
    }

    let generated = if config.language == "rust" {
        generate_rust_test(&classes, config)
    } else {
        generate_php_test(&classes, config)
    };

    if write && !generated.is_empty() {
        // Create directory if needed
        if let Some(parent) = test_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    format!("Failed to create test directory: {}", e),
                    Some("scaffold.write".to_string()),
                )
            })?;
        }
        io::write_file(&test_path, &generated, "write test scaffold")?;
    }

    Ok(ScaffoldResult {
        source_file: relative,
        test_file: test_relative,
        stub_count,
        content: generated,
        written: write,
        skipped: false,
        classes,
    })
}

/// Scaffold tests for all untested files under a root directory.
pub fn scaffold_untested(
    root: &Path,
    config: &ScaffoldConfig,
    write: bool,
) -> Result<ScaffoldBatchResult> {
    let source_dirs = if config.language == "rust" {
        vec!["src"]
    } else {
        vec!["src", "inc", "lib"]
    };

    let ext = if config.language == "rust" {
        "rs"
    } else {
        "php"
    };

    let mut source_files = Vec::new();
    for dir in &source_dirs {
        let dir_path = root.join(dir);
        if dir_path.exists() {
            collect_source_files(&dir_path, ext, &mut source_files);
        }
    }

    let mut results = Vec::new();
    let mut total_stubs = 0;
    let mut total_written = 0;
    let mut total_skipped = 0;

    for source_file in &source_files {
        let result = scaffold_file(source_file, root, config, write)?;
        if result.skipped {
            total_skipped += 1;
        } else {
            total_stubs += result.stub_count;
            if result.written {
                total_written += 1;
            }
        }
        results.push(result);
    }

    Ok(ScaffoldBatchResult {
        results,
        total_stubs,
        total_written,
        total_skipped,
    })
}

fn collect_source_files(dir: &Path, ext: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == ".git" || name == "vendor" || name == "node_modules" || name == "target" {
                continue;
            }
            collect_source_files(&path, ext, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            files.push(path);
        }
    }
}

// ============================================================================
// Utilities
// ============================================================================

/// Convert CamelCase or camelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            // Don't insert underscore between consecutive capitals
            let prev = s.chars().nth(i - 1).unwrap_or('_');
            if prev.is_lowercase() || prev.is_ascii_digit() {
                result.push('_');
            }
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

/// Get the root namespace (first segment) from a PHP namespace.
fn namespace_root(ns: &str) -> &str {
    ns.split('\\').next().unwrap_or(ns)
}

// ============================================================================
// Grammar-based extraction (preferred path)
// ============================================================================

/// Extract classes and methods using a grammar file.
///
/// This is the preferred extraction path. It delegates to `utils/grammar.rs`
/// which applies the extension-provided grammar patterns with structural
/// awareness (brace depth, comment/string skipping).
pub fn extract_with_grammar(content: &str, grammar_def: &grammar::Grammar) -> Vec<ExtractedClass> {
    let symbols = grammar::extract(content, grammar_def);

    // Get namespace
    let ns = grammar::namespace(&symbols).unwrap_or_default();

    // Get classes/structs/traits
    let type_symbols: Vec<_> = symbols
        .iter()
        .filter(|s| {
            s.concept == "class"
                || s.concept == "struct"
                || s.concept == "trait"
                || s.concept == "interface"
                || s.concept == "type"
        })
        .collect();

    // Get methods/functions
    let method_symbols: Vec<_> = symbols
        .iter()
        .filter(|s| {
            s.concept == "method" || s.concept == "function" || s.concept == "free_function"
        })
        .collect();

    let mut classes = Vec::new();

    if !type_symbols.is_empty() {
        for ts in &type_symbols {
            let name = ts.name().unwrap_or("").to_string();
            let kind = ts.get("kind").unwrap_or(ts.concept.as_str()).to_string();

            // Collect methods that belong to this type (inside its block)
            // For now, associate all methods with each class (same as legacy behavior)
            let methods: Vec<ExtractedMethod> = method_symbols
                .iter()
                .filter(|m| {
                    let mname = m.name().unwrap_or("");
                    // Skip magic methods except __construct
                    if mname.starts_with("__") && mname != "__construct" {
                        return false;
                    }
                    // Skip private methods for PHP
                    if let Some(mods) = m.get("modifiers") {
                        if mods.contains("private") {
                            return false;
                        }
                    }
                    true
                })
                .map(|m| {
                    let mname = m.name().unwrap_or("").to_string();
                    let vis = if let Some(mods) = m.get("modifiers") {
                        if mods.contains("private") {
                            "private"
                        } else if mods.contains("protected") {
                            "protected"
                        } else {
                            "public"
                        }
                    } else if let Some(v) = m.visibility() {
                        if v.contains("pub") {
                            "pub"
                        } else {
                            "private"
                        }
                    } else {
                        "public"
                    };

                    ExtractedMethod {
                        name: mname,
                        visibility: vis.to_string(),
                        is_static: m
                            .get("modifiers")
                            .map_or(false, |mods| mods.contains("static"))
                            || m.get("params").map_or(false, |p| !p.contains("self")),
                        line: m.line,
                        params: m.get("params").unwrap_or("").to_string(),
                    }
                })
                .collect();

            classes.push(ExtractedClass {
                name,
                namespace: ns.clone(),
                kind,
                methods,
            });
        }
    } else if !method_symbols.is_empty() {
        // No classes — procedural/module level
        let kind = if grammar_def.language.id == "rust" {
            "module"
        } else {
            "procedural"
        };
        let methods: Vec<ExtractedMethod> = method_symbols
            .iter()
            .map(|m| {
                let mname = m.name().unwrap_or("").to_string();
                ExtractedMethod {
                    name: mname,
                    visibility: m.visibility().unwrap_or("public").to_string(),
                    is_static: true,
                    line: m.line,
                    params: m.get("params").unwrap_or("").to_string(),
                }
            })
            .collect();

        classes.push(ExtractedClass {
            name: String::new(),
            namespace: ns,
            kind: kind.to_string(),
            methods,
        });
    }

    classes
}

/// Try to load a grammar from the extension path.
/// Returns None if the grammar file doesn't exist.
pub fn load_extension_grammar(extension_path: &Path, language: &str) -> Option<grammar::Grammar> {
    // Try TOML first, then JSON
    let toml_path = extension_path.join("grammar.toml");
    if toml_path.exists() {
        return grammar::load_grammar(&toml_path).ok();
    }

    let json_path = extension_path.join("grammar.json");
    if json_path.exists() {
        return grammar::load_grammar_json(&json_path).ok();
    }

    // Try language-specific subdirectory
    let lang_toml = extension_path.join(language).join("grammar.toml");
    if lang_toml.exists() {
        return grammar::load_grammar(&lang_toml).ok();
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn extract_php_class_methods() {
        let content = r#"<?php
namespace DataMachine\Abilities;

class PipelineAbilities {
    public function register() {}
    public function executeCreate($config) {}
    protected function validate($input) {}
    private function internal() {}
    public static function getInstance() {}
}
"#;
        let classes = extract_php(content);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "PipelineAbilities");
        assert_eq!(classes[0].namespace, "DataMachine\\Abilities");

        let names: Vec<&str> = classes[0].methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"register"));
        assert!(names.contains(&"executeCreate"));
        assert!(names.contains(&"validate")); // protected included
        assert!(!names.contains(&"internal")); // private excluded
        assert!(names.contains(&"getInstance"));
    }

    #[test]
    fn extract_php_magic_methods_skipped() {
        let content = r#"<?php
class Foo {
    public function __construct() {}
    public function __toString() {}
    public function __get($name) {}
    public function realMethod() {}
}
"#;
        let classes = extract_php(content);
        let names: Vec<&str> = classes[0].methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"__construct")); // constructor kept
        assert!(!names.contains(&"__toString")); // magic skipped
        assert!(!names.contains(&"__get")); // magic skipped
        assert!(names.contains(&"realMethod"));
    }

    #[test]
    fn extract_php_procedural() {
        let content = r#"<?php
function datamachine_init() {}
function datamachine_activate() {}
"#;
        let classes = extract_php(content);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].kind, "procedural");
        assert_eq!(classes[0].methods.len(), 2);
    }

    #[test]
    fn extract_rust_struct_methods() {
        let content = r#"
pub struct Config {
    data: HashMap<String, String>,
}

impl Config {
    pub fn new() -> Self { Self { data: HashMap::new() } }
    pub fn get(&self, key: &str) -> Option<&str> { None }
    fn private_method(&self) {}
    pub async fn load(path: &Path) -> Result<Self> { todo!() }
}
"#;
        let classes = extract_rust(content);
        let config = classes.iter().find(|c| c.name == "Config").unwrap();
        let names: Vec<&str> = config.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"get"));
        assert!(names.contains(&"load"));
        assert!(!names.contains(&"private_method"));
    }

    #[test]
    fn extract_rust_free_functions() {
        let content = r#"
pub fn parse_config(path: &Path) -> Config { todo!() }
pub fn validate(config: &Config) -> bool { true }
fn internal_helper() {}
"#;
        let classes = extract_rust(content);
        let module = classes.iter().find(|c| c.kind == "module").unwrap();
        let names: Vec<&str> = module.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"parse_config"));
        assert!(names.contains(&"validate"));
        assert!(!names.contains(&"internal_helper"));
    }

    #[test]
    fn extract_rust_includes_test_annotated_private_functions() {
        let content = r#"
#[test]
fn high_item_count_detected() {}

fn helper_not_a_test() {}
"#;

        let classes = extract_rust(content);
        let module = classes.iter().find(|c| c.kind == "module").unwrap();
        let names: Vec<&str> = module.methods.iter().map(|m| m.name.as_str()).collect();

        assert!(names.contains(&"high_item_count_detected"));
        assert!(!names.contains(&"helper_not_a_test"));
    }

    #[test]
    fn test_file_path_php() {
        let root = Path::new("/project");
        assert_eq!(
            test_file_path(Path::new("/project/src/Abilities/Foo.php"), root),
            PathBuf::from("/project/tests/Unit/Abilities/FooTest.php")
        );
        assert_eq!(
            test_file_path(Path::new("/project/inc/Core/Bar.php"), root),
            PathBuf::from("/project/tests/Unit/Core/BarTest.php")
        );
    }

    #[test]
    fn test_file_path_rust() {
        let root = Path::new("/project");
        assert_eq!(
            test_file_path(Path::new("/project/src/core/config.rs"), root),
            PathBuf::from("/project/tests/core/config_test.rs")
        );
    }

    #[test]
    fn generate_php_test_output() {
        let classes = vec![ExtractedClass {
            name: "FooAbilities".to_string(),
            namespace: "DataMachine\\Abilities".to_string(),
            kind: "class".to_string(),
            methods: vec![
                ExtractedMethod {
                    name: "register".to_string(),
                    visibility: "public".to_string(),
                    is_static: false,
                    line: 5,
                    params: String::new(),
                },
                ExtractedMethod {
                    name: "executeCreate".to_string(),
                    visibility: "public".to_string(),
                    is_static: false,
                    line: 10,
                    params: "$config".to_string(),
                },
            ],
        }];

        let config = ScaffoldConfig::php();
        let output = generate_php_test(&classes, &config);

        assert!(output.contains("class FooAbilitiesTest extends WP_UnitTestCase"));
        assert!(output.contains("@covers \\DataMachine\\Abilities\\FooAbilities"));
        assert!(output.contains("function test_register()"));
        assert!(output.contains("function test_execute_create()"));
        assert!(output.contains("markTestIncomplete"));
        assert!(output.contains("use DataMachine\\Abilities\\FooAbilities;"));
    }

    #[test]
    fn generate_rust_test_output() {
        let classes = vec![ExtractedClass {
            name: "Config".to_string(),
            namespace: String::new(),
            kind: "struct".to_string(),
            methods: vec![
                ExtractedMethod {
                    name: "new".to_string(),
                    visibility: "pub".to_string(),
                    is_static: true,
                    line: 5,
                    params: String::new(),
                },
                ExtractedMethod {
                    name: "load".to_string(),
                    visibility: "pub".to_string(),
                    is_static: true,
                    line: 10,
                    params: "path: &Path".to_string(),
                },
            ],
        }];

        let config = ScaffoldConfig::rust();
        let output = generate_rust_test(&classes, &config);

        assert!(output.contains("#[cfg(test)]"));
        assert!(output.contains("mod tests"));
        assert!(output.contains("fn test_config_new()"));
        assert!(output.contains("fn test_config_load()"));
        assert!(output.contains("todo!(\"implement test\")"));
    }

    #[test]
    fn generate_rust_test_dedupes_duplicate_names() {
        let classes = vec![
            ExtractedClass {
                name: "Config".to_string(),
                namespace: String::new(),
                kind: "struct".to_string(),
                methods: vec![ExtractedMethod {
                    name: "load".to_string(),
                    visibility: "pub".to_string(),
                    is_static: true,
                    line: 1,
                    params: String::new(),
                }],
            },
            ExtractedClass {
                name: "Config".to_string(),
                namespace: String::new(),
                kind: "struct".to_string(),
                methods: vec![ExtractedMethod {
                    name: "load".to_string(),
                    visibility: "pub".to_string(),
                    is_static: true,
                    line: 2,
                    params: String::new(),
                }],
            },
        ];

        let config = ScaffoldConfig::rust();
        let output = generate_rust_test(&classes, &config);

        assert_eq!(output.matches("fn test_config_load()").count(), 1);
    }

    #[test]
    fn scaffold_file_skips_low_signal_single_run_stub() {
        let dir = std::env::temp_dir().join("homeboy_test_scaffold_low_signal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/commands")).unwrap();
        std::fs::write(dir.join("src/commands/api.rs"), "pub fn run() {}\n").unwrap();

        let result = scaffold_file(
            &dir.join("src/commands/api.rs"),
            &dir,
            &ScaffoldConfig::rust(),
            false,
        )
        .unwrap();

        assert_eq!(result.stub_count, 0);
        assert!(result.content.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passes_scaffold_quality_gate_rejects_low_signal_dominated_set() {
        let names = vec![
            "test_run".to_string(),
            "test_new".to_string(),
            "test_validate".to_string(),
        ];

        assert!(!passes_scaffold_quality_gate(&names));
    }

    #[test]
    fn passes_scaffold_quality_gate_accepts_meaningful_mix() {
        let names = vec![
            "test_run".to_string(),
            "test_component_args_load".to_string(),
            "test_component_args_resolve".to_string(),
        ];

        assert!(passes_scaffold_quality_gate(&names));
    }

    #[test]
    fn passes_scaffold_quality_gate_rejects_oversized_scaffold() {
        let names: Vec<String> = (0..=MAX_AUTO_SCAFFOLD_STUBS)
            .map(|i| format!("test_meaningful_case_{}", i))
            .collect();

        assert!(!passes_scaffold_quality_gate(&names));
    }

    #[test]
    fn to_snake_case_works() {
        assert_eq!(to_snake_case("executeCreate"), "execute_create");
        assert_eq!(to_snake_case("getInstance"), "get_instance");
        assert_eq!(to_snake_case("register"), "register");
        assert_eq!(to_snake_case("HTMLParser"), "htmlparser"); // consecutive caps
        assert_eq!(to_snake_case("loadConfig"), "load_config");
    }

    #[test]
    fn extract_with_grammar_php() {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/wordpress/grammar.toml",
        );
        if !grammar_path.exists() {
            return; // Skip if not in dev environment
        }
        let grammar_def = grammar::load_grammar(grammar_path).unwrap();

        let content = r#"<?php
namespace App\Abilities;

class FooAbilities {
    public function register() {}
    public function executeCreate($config) {}
    protected function validate($input) {}
    private function internal() {}
}
"#;

        let classes = extract_with_grammar(content, &grammar_def);
        assert!(!classes.is_empty(), "Should extract at least one class");

        let foo = &classes[0];
        assert_eq!(foo.name, "FooAbilities");
        assert_eq!(foo.namespace, "App\\Abilities");

        // Private methods should be filtered
        let names: Vec<&str> = foo.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"register"));
        assert!(names.contains(&"executeCreate"));
        assert!(names.contains(&"validate"));
        assert!(!names.contains(&"internal"));
    }

    #[test]
    fn extract_with_grammar_rust() {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/rust/grammar.toml",
        );
        if !grammar_path.exists() {
            return;
        }
        let grammar_def = grammar::load_grammar(grammar_path).unwrap();

        let content = r#"
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

    fn private_method(&self) {}
}
"#;

        let classes = extract_with_grammar(content, &grammar_def);
        assert!(!classes.is_empty());

        let config = classes.iter().find(|c| c.name == "Config").unwrap();
        let names: Vec<&str> = config.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"load"));
    }

    #[test]
    fn scaffold_file_creates_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let src_dir = root.join("src/Abilities");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("FooAbilities.php"),
            r#"<?php
namespace App\Abilities;

class FooAbilities {
    public function register() {}
    public function execute($id) {}
}
"#,
        )
        .unwrap();

        let config = ScaffoldConfig::php();
        let result =
            scaffold_file(&src_dir.join("FooAbilities.php"), root, &config, false).unwrap();

        assert!(!result.skipped);
        assert_eq!(result.stub_count, 2);
        assert!(result.content.contains("class FooAbilitiesTest"));
        assert!(result.content.contains("test_register"));
        assert!(result.content.contains("test_execute"));
        assert!(!result.written);
    }

    #[test]
    fn scaffold_file_skips_existing_test() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let src_dir = root.join("src");
        let test_dir = root.join("tests/Unit");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(
            src_dir.join("Foo.php"),
            "<?php\nclass Foo {\n    public function bar() {}\n}\n",
        )
        .unwrap();
        // Pre-existing test file
        fs::write(test_dir.join("FooTest.php"), "<?php // existing").unwrap();

        let config = ScaffoldConfig::php();
        let result = scaffold_file(&src_dir.join("Foo.php"), root, &config, false).unwrap();
        assert!(result.skipped);
        assert_eq!(result.stub_count, 0);
    }

    #[test]
    fn scaffold_file_write_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("Bar.php"),
            "<?php\nclass Bar {\n    public function doThing() {}\n}\n",
        )
        .unwrap();

        let config = ScaffoldConfig::php();
        let result = scaffold_file(&src_dir.join("Bar.php"), root, &config, true).unwrap();

        assert!(result.written);
        assert!(root.join("tests/Unit/BarTest.php").exists());

        let written_content = fs::read_to_string(root.join("tests/Unit/BarTest.php")).unwrap();
        assert!(written_content.contains("class BarTest"));
    }

    #[test]
    fn scaffold_untested_batch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let src_dir = root.join("src");
        let test_dir = root.join("tests/Unit");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        // One file with tests, one without
        fs::write(
            src_dir.join("HasTests.php"),
            "<?php\nclass HasTests {\n    public function foo() {}\n}\n",
        )
        .unwrap();
        fs::write(test_dir.join("HasTestsTest.php"), "<?php // existing").unwrap();

        fs::write(
            src_dir.join("NoTests.php"),
            "<?php\nclass NoTests {\n    public function bar() {}\n    public function baz() {}\n}\n",
        )
        .unwrap();

        let config = ScaffoldConfig::php();
        let result = scaffold_untested(root, &config, false).unwrap();

        assert_eq!(result.total_skipped, 1); // HasTests already has test
        assert_eq!(result.total_stubs, 2); // bar + baz from NoTests
    }
}
