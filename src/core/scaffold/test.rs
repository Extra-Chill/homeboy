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

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::core::engine::contract_testgen::GeneratedTestOutput;
use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::extension::grammar;
use crate::extension::grammar_items;

/// A public method/function extracted from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedMethod {
    pub name: String,
    pub visibility: String,
    pub is_static: bool,
    pub line: usize,
    pub params: String,
}

/// Extracted class/struct info from a source file.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedClass {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub methods: Vec<ExtractedMethod>,
}

/// Configuration for scaffold generation.
#[derive(Debug, Clone)]
pub struct ScaffoldConfig {
    pub base_class: String,
    pub base_class_import: String,
    pub test_prefix: String,
    pub incomplete_body: String,
    pub language: String,
}

impl ScaffoldConfig {
    pub fn php() -> Self {
        Self {
            base_class: "WP_UnitTestCase".to_string(),
            base_class_import: "WP_UnitTestCase".to_string(),
            test_prefix: "test_".to_string(),
            incomplete_body: "$this->markTestIncomplete('TODO: implement');".to_string(),
            language: "php".to_string(),
        }
    }

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

#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldResult {
    pub source_file: String,
    pub test_file: String,
    pub stub_count: usize,
    pub content: String,
    pub written: bool,
    pub skipped: bool,
    pub classes: Vec<ExtractedClass>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldBatchResult {
    pub results: Vec<ScaffoldResult>,
    pub total_stubs: usize,
    pub total_written: usize,
    pub total_skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestLocation {
    SeparateFile(PathBuf),
    InlineModule,
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

pub(crate) fn extract_php(content: &str) -> Vec<ExtractedClass> {
    let mut classes = Vec::new();
    let ns_re = Regex::new(r"(?m)^namespace\s+([\w\\]+);").unwrap();
    let namespace = ns_re
        .captures(content)
        .map(|captures| captures[1].to_string())
        .unwrap_or_default();

    let class_re =
        Regex::new(r"(?m)^(?:abstract\s+)?(?:final\s+)?(class|trait|interface)\s+(\w+)").unwrap();

    for cap in class_re.captures_iter(content) {
        let kind = cap[1].to_string();
        let name = cap[2].to_string();
        let methods = extract_php_methods(content);

        classes.push(ExtractedClass {
            name,
            namespace: namespace.clone(),
            kind,
            methods,
        });
    }

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

            if visibility == "private" {
                continue;
            }

            methods.push(ExtractedMethod {
                name,
                visibility: visibility.to_string(),
                is_static: modifiers.contains("static"),
                line: i + 1,
                params,
            });
        }
    }

    methods
}

fn extract_php_functions(content: &str) -> Vec<ExtractedMethod> {
    static FN_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?m)^function\s+(\w+)\s*\(([^)]*)\)").unwrap());
    let mut methods = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(cap) = FN_RE.captures(line) {
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

pub(crate) fn extract_rust(content: &str) -> Vec<ExtractedClass> {
    let mut classes = Vec::new();
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

        if let Some(cap) = fn_re.captures(line) {
            let vis = cap.get(1).map_or("", |m| m.as_str().trim());
            let name = cap[2].to_string();
            let params = cap[3].trim().to_string();

            if !vis.starts_with("pub") || name.starts_with("test_") {
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

            if !visibility.starts_with("pub") && !is_test_function {
                continue;
            }

            let name = cap[2].to_string();
            let params = cap[3].trim().to_string();
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

pub fn test_file_path(source_path: &Path, root: &Path) -> PathBuf {
    let relative = source_path.strip_prefix(root).unwrap_or(source_path);
    let rel_str = relative.to_string_lossy();

    if rel_str.ends_with(".php") {
        let stripped = rel_str
            .strip_prefix("src/")
            .or_else(|| rel_str.strip_prefix("inc/"))
            .or_else(|| rel_str.strip_prefix("lib/"))
            .unwrap_or(&rel_str);
        let without_ext = stripped.strip_suffix(".php").unwrap_or(stripped);
        return root.join(format!("tests/Unit/{}Test.php", without_ext));
    }

    if rel_str.ends_with(".rs") {
        let stripped = rel_str.strip_prefix("src/").unwrap_or(&rel_str);
        let without_ext = stripped.strip_suffix(".rs").unwrap_or(stripped);
        return root.join(format!("tests/{}_test.rs", without_ext));
    }

    root.join("tests").join(relative)
}

pub fn find_test_location(source_file: &str, root: &Path, ext: &str) -> Option<TestLocation> {
    let source_path = root.join(source_file);
    let test_path = test_file_path(&source_path, root);

    if test_path.exists() {
        let relative = test_path
            .strip_prefix(root)
            .unwrap_or(&test_path)
            .to_path_buf();
        return Some(TestLocation::SeparateFile(relative));
    }

    if let Ok(content) = std::fs::read_to_string(&source_path) {
        if ext == "rs" && content.contains("#[cfg(test)]") {
            return Some(TestLocation::InlineModule);
        }
    }

    None
}

pub fn generated_test_uses_unresolved_types(content: &str) -> bool {
    content.contains("Default::default()") || content.contains("::default()")
}

pub fn render_generated_test_scaffold(
    generated: &GeneratedTestOutput,
    ext: &str,
) -> Option<String> {
    let mut content = String::new();
    content.push('\n');

    match ext {
        "rs" => {
            if let Some(grammar) = load_grammar_for_ext("rs") {
                if !grammar_items::validate_brace_balance(&generated.test_source, &grammar) {
                    return None;
                }
            }

            content.push_str("#[cfg(test)]\nmod tests {\n");
            content.push_str("    use super::*;\n");

            for imp in &generated.extra_imports {
                content.push_str(&format!("    {}\n", imp.trim()));
            }

            content.push('\n');
            content.push_str(&generated.test_source);
            content.push_str("}\n");
        }
        "php" => {
            for imp in &generated.extra_imports {
                content.push_str(imp);
                content.push('\n');
            }
            if !generated.extra_imports.is_empty() {
                content.push('\n');
            }
            content.push_str(&generated.test_source);
        }
        _ => {
            for imp in &generated.extra_imports {
                content.push_str(imp);
                content.push('\n');
            }
            if !generated.extra_imports.is_empty() {
                content.push('\n');
            }
            content.push_str(&generated.test_source);
        }
    }

    Some(content)
}

pub fn render_generated_test_append(generated: &GeneratedTestOutput, ext: &str) -> Option<String> {
    if ext == "rs" {
        if let Some(grammar) = load_grammar_for_ext("rs") {
            if !grammar_items::validate_brace_balance(&generated.test_source, &grammar) {
                return None;
            }
        }
    }

    let mut content = String::new();
    content.push('\n');
    content.push_str(&generated.test_source);
    Some(content)
}

pub(crate) fn generate_php_test(classes: &[ExtractedClass], config: &ScaffoldConfig) -> String {
    let mut out = String::new();
    let mut emitted = HashSet::new();
    out.push_str("<?php\n");

    for class in classes {
        if class.kind == "procedural" {
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

        let test_namespace = if !class.namespace.is_empty() {
            format!("{}\\Tests\\Unit", namespace_root(&class.namespace))
        } else {
            String::new()
        };

        if !test_namespace.is_empty() {
            out.push_str(&format!("namespace {};\n\n", test_namespace));
        }

        out.push_str(&format!("use {};\n", config.base_class_import));
        if !class.namespace.is_empty() {
            out.push_str(&format!("use {}\\{};\n", class.namespace, class.name));
        }
        out.push('\n');

        if !class.namespace.is_empty() {
            out.push_str(&format!(
                "/**\n * @covers \\{}\\{}\n */\n",
                class.namespace, class.name
            ));
        }

        out.push_str(&format!(
            "class {}Test extends {} {{\n\n",
            class.name, config.base_class
        ));

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

pub fn generate_rust_test(classes: &[ExtractedClass], _config: &ScaffoldConfig) -> String {
    let mut out = String::new();
    let mut emitted = HashSet::new();
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

    let content = local_files::read_file(source_path, "read source file")?;
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
        if let Some(parent) = test_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    format!("Failed to create test directory: {}", e),
                    Some("scaffold.write".to_string()),
                )
            })?;
        }
        local_files::write_file(&test_path, &generated, "write test scaffold")?;
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
            source_files.extend(collect_source_files(&dir_path, ext));
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

fn collect_source_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec![ext.to_string()]),
        ..Default::default()
    };
    codebase_scan::walk_files(dir, &config)
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            let prev = s.chars().nth(i - 1).unwrap_or('_');
            if prev.is_lowercase() || prev.is_ascii_digit() {
                result.push('_');
            }
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

fn namespace_root(ns: &str) -> &str {
    ns.split('\\').next().unwrap_or(ns)
}

pub fn extract_with_grammar(content: &str, grammar_def: &grammar::Grammar) -> Vec<ExtractedClass> {
    let symbols = grammar::extract(content, grammar_def);
    let ns = grammar::namespace(&symbols).unwrap_or_default();

    let type_symbols: Vec<_> = symbols
        .iter()
        .filter(|symbol| {
            symbol.concept == "class"
                || symbol.concept == "struct"
                || symbol.concept == "trait"
                || symbol.concept == "interface"
                || symbol.concept == "type"
        })
        .collect();

    let method_symbols: Vec<_> = symbols
        .iter()
        .filter(|symbol| {
            symbol.concept == "method"
                || symbol.concept == "function"
                || symbol.concept == "free_function"
        })
        .collect();

    let mut classes = Vec::new();

    if !type_symbols.is_empty() {
        for symbol in &type_symbols {
            let name = symbol.name().unwrap_or("").to_string();
            let kind = symbol
                .get("kind")
                .unwrap_or(symbol.concept.as_str())
                .to_string();

            let methods: Vec<ExtractedMethod> = method_symbols
                .iter()
                .filter(|method| {
                    let name = method.name().unwrap_or("");
                    if name.starts_with("__") && name != "__construct" {
                        return false;
                    }
                    if let Some(modifiers) = method.get("modifiers") {
                        if modifiers.contains("private") {
                            return false;
                        }
                    }
                    true
                })
                .map(|method| {
                    let name = method.name().unwrap_or("").to_string();
                    let visibility = if let Some(modifiers) = method.get("modifiers") {
                        if modifiers.contains("private") {
                            "private"
                        } else if modifiers.contains("protected") {
                            "protected"
                        } else {
                            "public"
                        }
                    } else if let Some(vis) = method.visibility() {
                        if vis.contains("pub") {
                            "pub"
                        } else {
                            "private"
                        }
                    } else {
                        "public"
                    };

                    ExtractedMethod {
                        name,
                        visibility: visibility.to_string(),
                        is_static: method
                            .get("modifiers")
                            .is_some_and(|mods| mods.contains("static"))
                            || method
                                .get("params")
                                .is_some_and(|params| !params.contains("self")),
                        line: method.line,
                        params: method.get("params").unwrap_or("").to_string(),
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
        let kind = if grammar_def.language.id == "rust" {
            "module"
        } else {
            "procedural"
        };
        let methods: Vec<ExtractedMethod> = method_symbols
            .iter()
            .map(|method| ExtractedMethod {
                name: method.name().unwrap_or("").to_string(),
                visibility: method.visibility().unwrap_or("public").to_string(),
                is_static: true,
                line: method.line,
                params: method.get("params").unwrap_or("").to_string(),
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

pub fn load_extension_grammar(extension_path: &Path, language: &str) -> Option<grammar::Grammar> {
    let toml_path = extension_path.join("grammar.toml");
    if toml_path.exists() {
        return grammar::load_grammar(&toml_path).ok();
    }

    let json_path = extension_path.join("grammar.json");
    if json_path.exists() {
        return grammar::load_grammar_json(&json_path).ok();
    }

    let lang_toml = extension_path.join(language).join("grammar.toml");
    if lang_toml.exists() {
        return grammar::load_grammar(&lang_toml).ok();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_generated_test_scaffold_wraps_rust_output() {
        let rendered = render_generated_test_scaffold(
            &GeneratedTestOutput {
                test_source: "    #[test]\n    fn test_thing() {\n        assert!(true);\n    }\n"
                    .to_string(),
                extra_imports: vec!["use std::path::PathBuf;".to_string()],
                tested_functions: vec!["thing".to_string()],
            },
            "rs",
        )
        .expect("expected scaffold output");

        assert!(rendered.contains("#[cfg(test)]"));
        assert!(rendered.contains("use super::*;"));
        assert!(rendered.contains("use std::path::PathBuf;"));
        assert!(rendered.contains("fn test_thing()"));
    }

    #[test]
    fn generated_test_uses_unresolved_types_detects_default_fallbacks() {
        assert!(generated_test_uses_unresolved_types(
            "let value = Default::default();"
        ));
        assert!(generated_test_uses_unresolved_types(
            "let value = Foo::default();"
        ));
        assert!(!generated_test_uses_unresolved_types(
            "let value = Foo::new();"
        ));
    }

    #[test]
    fn render_generated_test_append_wraps_without_mutating_source() {
        let rendered = render_generated_test_append(
            &GeneratedTestOutput {
                test_source: "    #[test]\n    fn test_more() {\n        assert!(true);\n    }\n"
                    .to_string(),
                extra_imports: vec![],
                tested_functions: vec!["more".to_string()],
            },
            "rs",
        )
        .expect("expected append output");

        assert!(rendered.starts_with('\n'));
        assert!(rendered.contains("fn test_more()"));
        assert!(!rendered.contains("mod tests"));
    }
}
