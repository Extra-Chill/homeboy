//! normalize_identifier — extracted from text.rs.

use crate::error::{Error, Result};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;


pub fn normalize_identifier(input: &str) -> String {
    input.trim().to_lowercase()
}

pub fn identifier_eq(a: &str, b: &str) -> bool {
    normalize_identifier(a) == normalize_identifier(b)
}
