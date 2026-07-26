//! Extension declarations for core-owned runtime helpers.

use serde::{Deserialize, Serialize};

/// A helper capability an extension requires for one execution capability.
///
/// `id` is resolved by Homeboy's core registry. `revision`, when supplied,
/// pins the declaration to the helper content revision the extension supports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHelperRequirement {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}
