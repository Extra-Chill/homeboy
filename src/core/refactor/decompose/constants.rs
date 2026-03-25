//! constants — extracted from decompose.rs.

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
