//! condition_contains — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};


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
