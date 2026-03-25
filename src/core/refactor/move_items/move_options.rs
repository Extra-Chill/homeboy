//! move_options — extracted from move_items.rs.

/// Behavioral options for move operations.
#[derive(Debug, Clone, Copy)]
pub struct MoveOptions {
    /// Whether related test functions should be moved alongside requested items.
    pub move_related_tests: bool,
    /// Skip rewriting import paths in caller files across the codebase.
    ///
    /// Set this to `true` when the source file will generate `pub use *` re-exports
    /// (e.g., decompose operations), making caller rewrites unnecessary — the
    /// re-exports ensure callers can still find moved items via the original path.
    /// Without this, the rewriter incorrectly changes sibling imports to point at
    /// submodule paths that aren't directly accessible from the sibling's scope.
    pub skip_caller_rewrites: bool,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            move_related_tests: true,
            skip_caller_rewrites: false,
        }
    }
}
