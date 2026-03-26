//! public — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use crate::extension::{self, ParsedItem};
use serde::{Deserialize, Serialize};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use super::DecomposeGroup;
use super::DecomposePlan;


pub(crate) fn is_public_item_source(source: &str) -> bool {
    let trimmed = source.trim_start();
    trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate) ")
}

pub(crate) fn public_items_for_group(plan: &DecomposePlan, group: &DecomposeGroup) -> Vec<String> {
    let source_path = Path::new(&plan.file);
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("rs");

    let root = Path::new(".");
    let content = std::fs::read_to_string(source_path).unwrap_or_default();
    let items = parse_items_for_group_export(ext, &content, &plan.file).unwrap_or_default();

    let group_names: HashSet<&str> = group.item_names.iter().map(|name| name.as_str()).collect();
    let mut pub_items: Vec<String> = items
        .into_iter()
        .filter(|item| group_names.contains(item.name.as_str()))
        .filter(|item| is_public_item_source(&item.source))
        .filter_map(|item| export_name_for_item(&item))
        .collect();

    pub_items.sort();
    pub_items.dedup();

    // Fallback for cases where the source file has already been rewritten and
    // we can't recover the exact public names yet. Better to re-export the named
    // group items than spray a glob import.
    if pub_items.is_empty() {
        pub_items = group
            .item_names
            .iter()
            .filter(|name| !name.contains(" for "))
            .cloned()
            .collect();
    }

    let _ = root;
    pub_items
}
