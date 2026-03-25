//! snake_case — extracted from test.rs.

use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use super::ExtractedClass;
use super::ScaffoldConfig;


pub(crate) fn generated_test_names(classes: &[ExtractedClass], config: &ScaffoldConfig) -> Vec<String> {
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

pub fn generate_php_test(classes: &[ExtractedClass], config: &ScaffoldConfig) -> String {
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

pub(crate) fn to_snake_case(s: &str) -> String {
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

pub(crate) fn namespace_root(ns: &str) -> &str {
    ns.split('\\').next().unwrap_or(ns)
}
