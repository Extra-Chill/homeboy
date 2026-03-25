//! normalize_import_line — extracted from apply.rs.

use crate::code_audit::conventions::Language;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use crate::engine::undo::InMemoryRollback;
use regex::Regex;
use std::path::Path;


pub fn insert_import(content: &str, import_line: &str, language: &Language) -> String {
    if import_already_present(content, import_line, language) {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();

    let import_prefix = match language {
        Language::Rust => "use ",
        Language::Php => "use ",
        Language::JavaScript | Language::TypeScript => "import ",
        Language::Unknown => "use ",
    };

    let rust_definition_starts = [
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "struct ",
        "pub struct ",
        "pub(crate) struct ",
        "enum ",
        "pub enum ",
        "pub(crate) enum ",
        "impl ",
        "impl<",
        "mod ",
        "pub mod ",
        "pub(crate) mod ",
        "trait ",
        "pub trait ",
        "pub(crate) trait ",
        "const ",
        "pub const ",
        "pub(crate) const ",
        "static ",
        "pub static ",
        "pub(crate) static ",
        "type ",
        "pub type ",
        "pub(crate) type ",
        "#[cfg(test)]",
    ];

    let mut last_import_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if *language == Language::Rust
            && rust_definition_starts
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        {
            break;
        }

        if trimmed.starts_with(import_prefix)
            || (trimmed.starts_with("use ") && *language == Language::Rust)
        {
            last_import_idx = Some(i);
        }
    }

    let insert_after = if let Some(idx) = last_import_idx {
        idx
    } else {
        let mut first_code = 0;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with('#')
                || trimmed == "<?php"
            {
                first_code = i + 1;
            } else {
                break;
            }
        }
        if first_code > 0 {
            first_code - 1
        } else {
            0
        }
    };

    let mut result = String::with_capacity(content.len() + import_line.len() + 2);
    for (i, line) in lines.iter().enumerate() {
        result.push_str(line);
        result.push('\n');
        if i == insert_after {
            result.push_str(import_line);
            result.push('\n');
        }
    }

    if !content.ends_with('\n') {
        result.pop();
    }

    result
}

pub(crate) fn import_already_present(content: &str, import_line: &str, language: &Language) -> bool {
    let normalized_candidate = normalize_import_line(import_line);
    if normalized_candidate.is_empty() {
        return true;
    }

    content.lines().any(|line| {
        let trimmed = line.trim();
        if !is_import_line(trimmed, language) {
            return false;
        }
        normalize_import_line(trimmed) == normalized_candidate
    })
}

pub(crate) fn is_import_line(line: &str, language: &Language) -> bool {
    match language {
        Language::Rust | Language::Php | Language::Unknown => line.starts_with("use "),
        Language::JavaScript | Language::TypeScript => line.starts_with("import "),
    }
}

pub(crate) fn normalize_import_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}
