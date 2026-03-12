//! Generic utility primitives that have not yet been promoted into core domains.
//!
//! - `autofix` - Compatibility shim re-exporting code-factory plumbing from `refactor::auto`

pub mod autofix;

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
