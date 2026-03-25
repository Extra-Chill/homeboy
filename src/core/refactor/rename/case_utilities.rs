//! case_utilities — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

pub(crate) fn pluralize(s: &str) -> String {
    if s.ends_with('s') || s.ends_with('x') || s.ends_with("sh") || s.ends_with("ch") {
        format!("{}es", s)
    } else if s.ends_with('y') && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("ay") {
