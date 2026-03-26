//! export — extracted from decompose.rs.

use crate::extension::{self, ParsedItem};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};


pub(crate) fn parse_items_for_group_export(ext: &str, content: &str, file: &str) -> Option<Vec<ParsedItem>> {
    let manifest = crate::extension::find_extension_for_file_ext(ext, "refactor")?;
    crate::core::refactor::move_items::ext_parse_items(&manifest, content, file)
        .or_else(|| crate::core::refactor::move_items::core_parse_items(&manifest, content))
}

pub(crate) fn export_name_for_item(item: &ParsedItem) -> Option<String> {
    match item.kind.as_str() {
        "function" | "struct" | "enum" | "trait" | "type_alias" | "const" | "static" => {
            Some(item.name.clone())
        }
        _ => None,
    }
}
