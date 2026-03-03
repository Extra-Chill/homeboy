//! Structural refactoring — rename, add, move, and transform code across a codebase.
//!
//! Walks source files, finds all references to a term (with word-boundary matching
//! and case-variant awareness), generates edits, and optionally applies them.

pub mod add;
pub mod move_items;
mod rename;

pub use add::{add_import, fixes_from_audit, AddResult};
pub use move_items::{move_items, ImportRewrite, ItemKind, MoveResult, MovedItem};
pub use rename::{
    apply_renames, find_references, generate_renames, CaseVariant, FileEdit, FileRename, Reference,
    RenameResult, RenameScope, RenameSpec, RenameWarning,
};
