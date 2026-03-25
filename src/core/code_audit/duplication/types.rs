//! types — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// A group of files containing an identical function.
///
/// The fixer uses this to keep the canonical copy and remove the rest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    /// The duplicated function name.
    pub function_name: String,
    /// File chosen to keep the function (canonical location).
    pub canonical_file: String,
    /// Files where the duplicate should be removed and replaced with an import.
    pub remove_from: Vec<String>,
}

/// Per-method call sequence extracted from file content.
#[derive(Debug)]
pub(crate) struct MethodCallSequence {
    file: String,
    method: String,
    /// Ordered list of function/method calls made in the body.
    calls: Vec<String>,
}
