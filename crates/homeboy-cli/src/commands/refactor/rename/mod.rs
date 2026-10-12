//! Rename engine — find and replace terms across a codebase with case awareness.
//!
//! Given a `RenameSpec` (from → to), this module:
//! 1. Generates all case variants (snake, camel, Pascal, UPPER, plural)
//! 2. Walks the codebase finding word-boundary matches
//! 3. Generates file content edits and file renames
//! 4. Applies changes to disk all-or-nothing (or returns a dry-run preview)
//!
//! Submodules:
//! - [`types`] — specs, scopes, contexts, and result types
//! - [`casing`] — case/word utilities and cross-separator join functions
//! - [`matching`] — boundary-aware, literal, and case-insensitive term matchers
//! - [`engine`] — reference finding, edit/rename generation, and application
//! - [`safety`] — collision and duplicate-identifier detection

mod casing;
mod engine;
mod matching;
mod safety;
mod types;

#[cfg(test)]
mod tests;

pub use engine::{apply_renames, generate_renames_with_targeting};
pub use types::{RenameContext, RenameScope, RenameSpec, RenameTargeting};

// Internal items pulled into scope for the test module (`tests.rs` uses
// `super::*`).
#[cfg(test)]
use casing::{capitalize, pluralize, split_words};
#[cfg(test)]
use engine::{find_references, find_references_with_targeting, generate_renames};
#[cfg(test)]
use matching::{find_boundary_matches, find_literal_matches};
#[cfg(test)]
use safety::{detect_collisions, detect_duplicate_identifiers, extract_field_identifier};
#[cfg(test)]
use types::FileRename;
