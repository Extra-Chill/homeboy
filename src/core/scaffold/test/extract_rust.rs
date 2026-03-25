//! extract_rust — extracted from test.rs.

use regex::Regex;
use std::collections::HashSet;
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use super::ExtractedClass;
use super::ExtractedMethod;


pub fn extract_rust(content: &str) -> Vec<ExtractedClass> {
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

pub(crate) fn extract_rust_impl_methods(content: &str, type_name: &str) -> Vec<ExtractedMethod> {
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

pub(crate) fn extract_rust_free_functions(content: &str) -> Vec<ExtractedMethod> {
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
