//! like — extracted from contract_testgen.rs.

use super::super::contract::*;
use super::super::*;


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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_path_like_default_path() {

        let _result = is_path_like();
    }

    #[test]
    fn test_is_numeric_like_default_path() {

        let _result = is_numeric_like();
    }

}
