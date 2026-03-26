//! declaration — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};


pub(crate) fn starts_like_extractable_declaration(source: &str, kind: &str) -> bool {
    let mut saw_attribute = false;

    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") || line.starts_with("///") {
            continue;
        }

        if line.starts_with("#") {
            saw_attribute = true;
            continue;
        }

        let decl = matches_declaration_prefix(line, kind);
        if decl {
            return true;
        }

        // If we saw attributes/doc comments, the next real line must be the item
        // declaration itself — not a method chain/body fragment.
        if saw_attribute {
            return false;
        }

        // First meaningful non-attribute line is not a declaration: this is a
        // partial body fragment and must not be extracted.
        return false;
    }

    false
}

pub(crate) fn matches_declaration_prefix(line: &str, kind: &str) -> bool {
    match kind {
        "function" => {
            line.starts_with("fn ")
                || line.starts_with("pub fn ")
                || line.starts_with("pub(crate) fn ")
                || line.starts_with("pub(super) fn ")
                || line.starts_with("pub async fn ")
                || line.starts_with("pub(crate) async fn ")
                || line.starts_with("async fn ")
        }
        "struct" => {
            line.starts_with("struct ")
                || line.starts_with("pub struct ")
                || line.starts_with("pub(crate) struct ")
        }
        "enum" => {
            line.starts_with("enum ")
                || line.starts_with("pub enum ")
                || line.starts_with("pub(crate) enum ")
        }
        "trait" => {
            line.starts_with("trait ")
                || line.starts_with("pub trait ")
                || line.starts_with("pub(crate) trait ")
        }
        "type_alias" => {
            line.starts_with("type ")
                || line.starts_with("pub type ")
                || line.starts_with("pub(crate) type ")
        }
        "impl" => line.starts_with("impl "),
        "const" => {
            line.starts_with("const ")
                || line.starts_with("pub const ")
                || line.starts_with("pub(crate) const ")
        }
        "static" => {
            line.starts_with("static ")
                || line.starts_with("pub static ")
                || line.starts_with("pub(crate) static ")
        }
        _ => false,
    }
}
