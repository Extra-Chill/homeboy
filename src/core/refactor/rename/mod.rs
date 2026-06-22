//! Rename engine — find and replace terms across a codebase with case awareness.
//!
//! Given a `RenameSpec` (from → to), this extension:
//! 1. Generates all case variants (snake, camel, Pascal, UPPER, plural)
//! 2. Walks the codebase finding word-boundary matches
//! 3. Generates file content edits and file/directory renames
//! 4. Applies changes to disk (or returns a dry-run preview)
//!
//! This module is a thin coordinator. The implementation lives in focused
//! submodules:
//! - [`types`] — specs, scopes, contexts, and result types
//! - [`casing`] — case/word utilities and cross-separator join functions
//! - [`engine`] — reference finding, edit/rename generation, and application
//! - [`safety`] — collision and duplicate-identifier detection

mod casing;
mod engine;
mod safety;
mod types;

#[cfg(test)]
mod tests;

// Public API re-exports — external `use crate::core::refactor::rename::X` paths
// keep working unchanged.
pub use engine::{
    apply_renames, find_references, find_references_with_targeting, generate_renames,
    generate_renames_with_targeting,
};
pub use types::{
    CaseVariant, FileEdit, FileRename, Reference, RenameContext, RenameResult, RenameScope,
    RenameSpec, RenameTargeting, RenameWarning,
};

// Internal items pulled into scope for the test module (`tests.rs` uses
// `super::*`). These are not part of the public API.
#[cfg(test)]
use crate::core::engine::codebase_scan::{find_boundary_matches, find_literal_matches};
#[cfg(test)]
use casing::{capitalize, pluralize, split_words};
#[cfg(test)]
use safety::{detect_collisions, detect_duplicate_identifiers, extract_field_identifier};
