//! move_options — extracted from move_items.rs.


/// Behavioral options for move operations.
#[derive(Debug, Clone, Copy)]
pub struct MoveOptions {
    /// Whether related test functions should be moved alongside requested items.
    pub move_related_tests: bool,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            move_related_tests: true,
        }
    }
}
