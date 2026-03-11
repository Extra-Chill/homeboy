//! Generic utility primitives that have not yet been promoted into core domains.
//!
//! - `args` - CLI argument normalization
//! - `autofix` - Compatibility shim re-exporting code-factory plumbing from `refactor::auto`
//! - `base_path` - Remote path joining utilities
//! - `entity_suggest` - Entity suggestion for unrecognized CLI subcommands
//! - `io` - File I/O with consistent error handling
//! - `parser` - Text extraction and manipulation
//! - `resolve` - Project/component argument resolution
//! - `slugify` - String slug generation
//! - `token` - String comparison and normalization

pub mod args;
pub mod artifact;
pub mod autofix;
pub mod base_path;
pub mod entity_suggest;
pub mod io;
pub mod parser;
pub mod resolve;
pub mod slugify;
pub mod token;

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
