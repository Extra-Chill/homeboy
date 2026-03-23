//! structural_context — extracted from grammar.rs.

use super::super::*;
use super::default;
use super::new;
use super::Region;

/// Tracks structural context while parsing source text.
#[derive(Debug, Clone)]
pub struct StructuralContext {
    /// Current brace nesting depth.
    pub depth: i32,

    /// Current region (code, comment, string).
    pub region: Region,

    /// Stack of block contexts: (kind_label, depth_when_entered).
    /// Features can push/pop to track impl blocks, test modules, etc.
    pub block_stack: Vec<(String, i32)>,
}

impl Default for StructuralContext {
    fn default() -> Self {
        Self::new()
    }
}
