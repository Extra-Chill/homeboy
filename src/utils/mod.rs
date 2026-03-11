//! Generic utility primitives with zero domain knowledge.
//!
//! - `args` - CLI argument normalization
//! - `autofix` - Compatibility shim re-exporting code-factory plumbing from `refactor::auto`
//! - `base_path` - Remote path joining utilities
//! - `codebase_scan` - File walking and content search across codebases
//! - `command` - Command execution with error handling
//! - `entity_suggest` - Entity suggestion for unrecognized CLI subcommands
//! - `io` - File I/O with consistent error handling
//! - `parser` - Text extraction and manipulation
//! - `resolve` - Project/component argument resolution
//! - `shell` - Shell escaping and quoting
//! - `slugify` - String slug generation
//! - `template` - String template rendering
//! - `token` - String comparison and normalization
//! - `validation` - Input validation helpers

pub mod args;
pub mod artifact;
pub mod autofix;
pub mod base_path;
pub mod codebase_scan;
pub mod command;
pub mod entity_suggest;
pub mod io;
pub mod output_parse;
pub mod parser;
pub mod resolve;
pub mod shell;
pub mod slugify;
pub(crate) mod template;
pub mod token;
pub mod validation;

// ============================================================================
// Serde helpers
// ============================================================================

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
