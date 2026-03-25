//! constants — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// Minimum number of locations for a function to count as duplicated.
pub(crate) const MIN_DUPLICATE_LOCATIONS: usize = 2;

/// Names that are too generic to flag as near-duplicates.
/// These appear in many files with completely unrelated implementations.
pub(crate) const GENERIC_NAMES: &[&str] = &[
    "run", "new", "default", "build", "list", "show", "set", "get", "delete", "remove", "clear",
    "create", "update", "status", "search", "find", "read", "write", "rename", "init", "test",
    "fmt", "from", "into", "clone", "drop", "display", "parse", "validate", "execute", "handle",
    "process", "merge", "resolve", "pin", "plan",
];

/// Minimum body line count — skip trivial functions (1-2 line bodies).
/// Functions like `fn default_true() -> bool { true }` are too small
/// to meaningfully refactor into shared code with a parameter.
pub(crate) const MIN_BODY_LINES: usize = 3;

/// Minimum number of non-blank, non-comment lines for a block to be
/// considered meaningful. Blocks shorter than this are too trivial to flag.
pub(crate) const MIN_INTRA_BLOCK_LINES: usize = 5;

/// Minimum number of function calls in a method body to consider it for
/// parallel implementation detection. Trivial methods (< 4 calls) are
/// too simple to meaningfully abstract.
pub(crate) const MIN_CALL_COUNT: usize = 4;

/// Minimum Jaccard similarity (|intersection| / |union|) between two
/// call sets to flag as a parallel implementation.
pub(crate) const MIN_JACCARD_SIMILARITY: f64 = 0.5;

/// Minimum longest-common-subsequence ratio to flag as parallel.
/// This captures sequential ordering — two methods that call helpers
/// in the same order score higher than ones that share calls but in
/// a different order.
pub(crate) const MIN_LCS_RATIO: f64 = 0.5;

/// Minimum number of shared (intersecting) calls between two methods
/// to flag as a parallel implementation. This prevents false positives
/// from methods that share only 1-2 trivial calls like `to_string`.
pub(crate) const MIN_SHARED_CALLS: usize = 3;

/// Ubiquitous stdlib/trait method calls that appear in almost every function
/// and carry no signal for parallel implementation detection. Two functions
/// both calling `.to_string()` does not mean they implement the same workflow.
pub(crate) const TRIVIAL_CALLS: &[&str] = &[
    "to_string",
    "to_owned",
    "to_lowercase",
    "to_uppercase",
    "clone",
    "default",
    "new",
    "len",
    "is_empty",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "expect",
    "as_str",
    "as_ref",
    "as_deref",
    "into",
    "from",
    "iter",
    "into_iter",
    "collect",
    "map",
    "filter",
    "any",
    "all",
    "find",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "extend",
    "join",
    "split",
    "trim",
    "starts_with",
    "ends_with",
    "strip_prefix",
    "strip_suffix",
    "replace",
    "display",
    "write",
    "read",
    "flush",
    "ok",
    "err",
    "map_err",
    "and_then",
    "or_else",
    "flatten",
    "take",
    "skip",
    "chain",
    "zip",
    "enumerate",
    "cloned",
    "copied",
    "rev",
    "sort",
    "sort_by",
    "dedup",
    "retain",
    "get",
    "set",
    "entry",
    "or_insert",
    "or_insert_with",
    "keys",
    "values",
    "exists",
    "parent",
    "file_name",
    "extension",
    "with_extension",
];
