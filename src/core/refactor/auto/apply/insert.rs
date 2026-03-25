//! insert — extracted from apply.rs.

use crate::code_audit::conventions::Language;
use crate::core::refactor::plan::generate::primary_type_name_from_declaration;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use regex::Regex;
use crate::engine::undo::InMemoryRollback;
use std::path::Path;


pub fn insert_into_constructor(
    content: &str,
    stubs: &[&String],
    language: &Language,
) -> String {
    let constructor_pattern = match language {
        Language::Php => r"function\s+__construct\s*\([^)]*\)\s*\{",
        Language::Rust => r"fn\s+new\s*\([^)]*\)\s*(?:->[^{]*)?\{",
        Language::JavaScript | Language::TypeScript => r"constructor\s*\([^)]*\)\s*\{",
        Language::Unknown => return content.to_string(),
    };

    let re = match Regex::new(constructor_pattern) {
        Ok(re) => re,
        Err(_) => return content.to_string(),
    };

    if let Some(m) = re.find(content) {
        let insert_pos = m.end();
        let insert_text: String = stubs.iter().map(|s| format!("\n{}", s)).collect();

        let mut result = String::with_capacity(content.len() + insert_text.len());
        result.push_str(&content[..insert_pos]);
        result.push_str(&insert_text);
        result.push_str(&content[insert_pos..]);
        result
    } else {
        content.to_string()
    }
}

pub fn insert_trait_uses(content: &str, stubs: &[&String], language: &Language) -> String {
    match language {
        Language::Php => {
            static CLASS_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                Regex::new(r"(?:class|trait|interface)\s+\w+[^\{]*\{").unwrap()
            });
            if let Some(m) = CLASS_RE.find(content) {
                let insert_pos = m.end();
                let mut result = String::with_capacity(content.len() + stubs.len() * 40);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                for stub in stubs {
                    result.push_str(stub.trim_end());
                    result.push('\n');
                }
                result.push_str(&content[insert_pos..]);
                result
            } else {
                content.to_string()
            }
        }
        _ => {
            let combined: String = stubs
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            insert_before_closing_brace(content, &combined, language)
        }
    }
}

pub fn insert_namespace_declaration(
    content: &str,
    declaration: &str,
    language: &Language,
) -> String {
    match language {
        Language::Php => {
            let namespace_re = Regex::new(r"(?m)^\s*namespace\s+[^;]+;").unwrap();
            if namespace_re.is_match(content) {
                return namespace_re.replace(content, declaration).to_string();
            }

            if let Some(open_tag_pos) = content.find("<?php") {
                let insert_pos = open_tag_pos + 5;
                let mut result = String::with_capacity(content.len() + declaration.len() + 2);
                result.push_str(&content[..insert_pos]);
                result.push_str("\n\n");
                result.push_str(declaration);
                result.push_str(&content[insert_pos..]);
                return result;
            }

            format!("{}\n{}", declaration, content)
        }
        _ => content.to_string(),
    }
}

pub fn insert_type_conformance(
    content: &str,
    declarations: &[&String],
    language: &Language,
) -> String {
    let Some(declaration) = declarations.first() else {
        return content.to_string();
    };

    match language {
        Language::Php | Language::TypeScript => {
            insert_inline_type_conformance(content, declaration, language)
        }
        Language::Rust => {
            if content.contains(declaration.as_str()) {
                content.to_string()
            } else if content.ends_with('\n') {
                format!("{}{}", content, declaration)
            } else {
                format!("{}\n{}", content, declaration)
            }
        }
        Language::JavaScript | Language::Unknown => content.to_string(),
    }
}

pub(crate) fn insert_inline_type_conformance(content: &str, declaration: &str, language: &Language) -> String {
    let conformance = declaration.trim();
    let keyword = match language {
        Language::Php | Language::TypeScript => "implements",
        _ => return content.to_string(),
    };

    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    for line in &mut lines {
        if primary_type_name_from_declaration(line, language).is_none() {
            continue;
        }

        if line.contains(conformance) {
            break;
        }

        if line.contains(keyword) {
            if let Some(pos) = line.find('{') {
                let before = &line[..pos].trim_end();
                let after = &line[pos..];
                *line = format!("{}, {} {}", before, conformance, after);
            } else {
                *line = format!("{}, {}", line.trim_end(), conformance);
            }
        } else if let Some(pos) = line.find('{') {
            let before = line[..pos].trim_end();
            let after = &line[pos..];
            *line = format!("{} {} {} {}", before, keyword, conformance, after);
        } else {
            *line = format!("{} {} {}", line.trim_end(), keyword, conformance);
        }

        break;
    }

    let mut result = lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

pub fn insert_before_closing_brace(
    content: &str,
    code: &str,
    _language: &Language,
) -> String {
    if let Some(last_brace) = content.rfind('}') {
        let mut result = String::with_capacity(content.len() + code.len());
        result.push_str(&content[..last_brace]);
        result.push_str(code);
        result.push_str(&content[last_brace..]);
        result
    } else {
        format!("{}{}", content, code)
    }
}
