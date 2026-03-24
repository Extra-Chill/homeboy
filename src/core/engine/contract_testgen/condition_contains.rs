//! condition_contains — extracted from contract_testgen.rs.

use super::super::contract::*;
use super::super::*;


/// Check if condition contains `param.method()` (case-insensitive).
pub(crate) fn condition_contains_param_method(condition_lower: &str, param: &str, method: &str) -> bool {
    let pattern = format!("{}.{}(", param.to_lowercase(), method);
    condition_lower.contains(&pattern)
}

/// Check if condition contains a negated method call: `!param.method()`.
pub(crate) fn condition_contains_negated_method(condition: &str, param: &str, method: &str) -> bool {
    let pattern = format!("!{}.{}(", param, method);
    condition.contains(&pattern)
}
