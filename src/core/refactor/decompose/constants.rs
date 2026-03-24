//! constants — extracted from decompose.rs.

use super::super::*;


/// Maximum items per group before we attempt to split further.
pub(crate) const MAX_GROUP_SIZE: usize = 15;

/// Groups below this size get merged into the nearest related group.
pub(crate) const MERGE_THRESHOLD: usize = 2;

/// Minimum number of items sharing a word to form a name-based cluster.
pub(crate) const MIN_CLUSTER_SIZE: usize = 2;

/// Maximum number of meaningful words (non-stop-words) in a module name.
///
/// Decompose generates module names from section headers, function names, and
/// cluster labels. Without truncation, verbose source names produce filenames
/// like `structural_parser_context_aware_iteration_over_source_text.rs`.
/// This limit keeps names concise (e.g., `structural_parser.rs`).
pub(crate) const MAX_MODULE_NAME_WORDS: usize = 3;

/// Maximum number of callees before a function is considered a "hub".
///
/// Hub functions (orchestrators that call many others) are excluded from
/// union-find clustering to prevent mega-groups. They stay in the parent
/// module while their callees form focused sub-clusters.
pub(crate) const HUB_THRESHOLD: usize = 4;
