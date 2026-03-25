//! split — extracted from text.rs.

use crate::error::{Error, Result};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;


/// Parse line by splitting on whitespace, returning parts if expected count met.
pub fn split_whitespace(line: &str, min_parts: usize) -> Option<Vec<&str>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= min_parts {
        Some(parts)
    } else {
        None
    }
}

/// Parse a potentially combined project:subtarget identifier.
pub fn split_identifier(identifier: &str) -> (&str, Option<&str>) {
    match identifier.split_once(':') {
        Some((project, subtarget)) => {
            let project = project.trim();
            let subtarget = subtarget.trim();
            if subtarget.is_empty() {
                (project, None)
            } else {
                (project, Some(subtarget))
            }
        }
        None => (identifier.trim(), None),
    }
}
