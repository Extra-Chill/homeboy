//! like — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};


/// Check if a type looks like a filesystem path (language-agnostic heuristic).
pub(crate) fn is_path_like(ptype: &str) -> bool {
    let t = ptype.trim().to_lowercase();
    t.contains("path")
}

/// Check if a type looks like a numeric type (language-agnostic heuristic).
pub(crate) fn is_numeric_like(ptype: &str) -> bool {
    let t = ptype.trim();
    // Common numeric type patterns across languages
    matches!(
        t,
        "usize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "isize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "int"
            | "float"
            | "double"
            | "number"
    )
}
