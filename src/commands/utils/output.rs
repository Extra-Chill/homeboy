//! Thin re-export of atomic output-file helpers now owned by `core::io`.
//!
//! The atomic-write infrastructure lives in [`crate::core::io::output_file`];
//! the command layer keeps this re-export so existing
//! `commands::utils::output::` call sites continue to resolve.

pub use crate::core::io::output_file::{
    write_output_file, write_output_file_atomically, OutputWriteOptions, TrailingNewline,
};
