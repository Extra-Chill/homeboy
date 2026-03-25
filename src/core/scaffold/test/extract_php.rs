//! extract_php — extracted from test.rs.

use regex::Regex;
use std::collections::HashSet;
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use super::ExtractedClass;
use super::ExtractedMethod;


pub fn extract_php(content: &str) -> Vec<ExtractedClass> {
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

pub(crate) fn extract_php_methods(content: &str) -> Vec<ExtractedMethod> {
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

pub(crate) fn extract_php_functions(content: &str) -> Vec<ExtractedMethod> {
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
