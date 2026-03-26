//! surface — extracted from decompose.rs.

use std::collections::{BTreeMap, HashSet};
use crate::extension::{self, ParsedItem};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};


pub(crate) fn has_established_module_surface(content: &str) -> bool {
    let mut public_decl_count = 0usize;

    for line in content.lines().map(str::trim) {
        if line.starts_with("pub use ") || line.starts_with("pub mod ") || line.starts_with("mod ")
        {
            return true;
        }

        if line.starts_with("pub fn ")
            || line.starts_with("pub(crate) fn ")
            || line.starts_with("pub struct ")
            || line.starts_with("pub(crate) struct ")
            || line.starts_with("pub enum ")
            || line.starts_with("pub(crate) enum ")
            || line.starts_with("pub trait ")
            || line.starts_with("pub(crate) trait ")
            || line.starts_with("pub type ")
            || line.starts_with("pub(crate) type ")
            || line.starts_with("pub const ")
            || line.starts_with("pub(crate) const ")
        {
            public_decl_count += 1;
            if public_decl_count >= 4 {
                return true;
            }
        }
    }

    false
}

pub(crate) fn rebalance_for_viable_parent_surface(
    file: &str,
    content: &str,
    fn_items: &[&ParsedItem],
    mut buckets: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    if !is_effective_module_root(file, content) {
        return buckets;
    }

    let parent_surface: HashSet<String> = fn_items
        .iter()
        .filter(|item| is_root_orchestrator_name(&item.name) || is_public_item_source(&item.source))
        .map(|item| item.name.clone())
        .collect();

    if parent_surface.is_empty() {
        return buckets;
    }

    let extracted_surface_count = buckets
        .values()
        .flat_map(|names| names.iter())
        .filter(|name| parent_surface.contains(*name))
        .count();

    if extracted_surface_count >= parent_surface.len().saturating_sub(1) {
        let mut preserved = Vec::new();
        for names in buckets.values_mut() {
            let mut i = 0usize;
            while i < names.len() {
                if parent_surface.contains(&names[i]) {
                    preserved.push(names.remove(i));
                } else {
                    i += 1;
                }
            }
        }

        buckets.retain(|_, names| !names.is_empty());
        if !preserved.is_empty() {
            buckets
                .entry("parent_surface".to_string())
                .or_default()
                .extend(preserved);
        }
    }

    buckets
}
