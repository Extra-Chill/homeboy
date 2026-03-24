//! complement_hint — extracted from contract_testgen.rs.

use std::collections::HashMap;
use super::NON_EMPTY;
use super::TRUE;
use super::EXISTENT_PATH;
use super::SOME_DEFAULT;
use super::POSITIVE;
use super::ZERO;
use super::NONE;
use super::FALSE;
use super::EMPTY;
use super::NONEXISTENT_PATH;
use super::super::contract::*;
use super::super::*;


/// Build complement hints for a branch by examining what other branches require.
///
/// If branch 1 says param X needs hint `"empty"`, then branches that don't
/// mention param X should use `"non_empty"` to reach a different code path.
/// This ensures each branch's test uses inputs that actually trigger that branch.
pub(crate) fn build_complement_hints(
    branch_index: usize,
    all_branch_hints: &[HashMap<String, String>],
) -> HashMap<String, String> {
    let mut complements = HashMap::new();
    let my_hints = &all_branch_hints[branch_index];

    for (j, other_hints) in all_branch_hints.iter().enumerate() {
        if j == branch_index {
            continue;
        }
        for (param_name, hint) in other_hints {
            // Only provide a complement if this branch doesn't have its own hint
            if my_hints.contains_key(param_name) || complements.contains_key(param_name) {
                continue;
            }
            if let Some(complement) = complement_hint(hint) {
                complements.insert(param_name.clone(), complement);
            }
        }
    }

    complements
}

/// Return the opposite hint for a given hint.
///
/// This allows branches that don't mention a param to use the inverse
/// of what another branch requires, ensuring the test reaches the right path.
pub(crate) fn complement_hint(hint: &str) -> Option<String> {
    // Split compound hints like "contains:foo"
    let base = hint.split(':').next().unwrap_or(hint);

    match base {
        "empty" => Some(hints::NON_EMPTY.to_string()),
        "non_empty" => Some(hints::EMPTY.to_string()),
        "none" => Some(hints::SOME_DEFAULT.to_string()),
        "some_default" => Some(hints::NONE.to_string()),
        "true" => Some(hints::FALSE.to_string()),
        "false" => Some(hints::TRUE.to_string()),
        "zero" => Some(hints::POSITIVE.to_string()),
        "positive" => Some(hints::ZERO.to_string()),
        "nonexistent_path" => Some(hints::EXISTENT_PATH.to_string()),
        "existent_path" => Some(hints::NONEXISTENT_PATH.to_string()),
        _ => None,
    }
}
