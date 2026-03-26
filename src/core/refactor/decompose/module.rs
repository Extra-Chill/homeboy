//! module — extracted from decompose.rs.

use std::path::{Path, PathBuf};
use std::collections::{BTreeMap, HashSet};
use serde::{Deserialize, Serialize};
use crate::extension::{self, ParsedItem};
use crate::Result;
use super::super::move_items::{MoveOptions, MoveResult};
use super::has_established_module_surface;


pub(crate) fn is_module_root(file: &str) -> bool {
    let path = Path::new(file);
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| matches!(name, "mod.rs" | "lib.rs" | "main.rs" | "config.rs"))
        .unwrap_or(false)
}

pub(crate) fn is_effective_module_root(file: &str, content: &str) -> bool {
    is_module_root(file) || has_established_module_surface(content)
}
