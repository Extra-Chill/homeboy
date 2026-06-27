//! Shared low-level IO helpers used across core subsystems.
//!
//! These helpers exist to consolidate genuinely duplicated workflows (recursive
//! directory copy, etc.) so callers can reuse a single implementation while
//! keeping their own error-context labels. Single-file IO (e.g. observation
//! artifact persistence) lives with its caller — these helpers are only for
//! workflows that recurse or loop in the same shape.

pub(crate) mod copy_tree;
pub mod output_file;

pub(crate) use copy_tree::{copy_tree, EntryPolicy};
pub use output_file::{
    write_output_file, write_output_file_atomically, OutputWriteOptions, TrailingNewline,
};
