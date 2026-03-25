//! lines — extracted from text.rs.

use crate::error::{Error, Result};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;


/// Parse output into non-empty lines.
pub fn lines(output: &str) -> impl Iterator<Item = &str> {
    output.lines().filter(|line| !line.is_empty())
}

/// Parse output into lines with custom filter.
pub fn lines_filtered<'a, F>(output: &'a str, filter: F) -> impl Iterator<Item = &'a str>
where
    F: Fn(&str) -> bool + 'a,
{
    output
        .lines()
        .filter(move |line| !line.is_empty() && filter(line))
}
