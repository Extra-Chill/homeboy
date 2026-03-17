//! rename_spec — extracted from mod.rs.

use crate::core::refactor::rename::types::CaseVariant;
use crate::core::refactor::rename::literal;
use crate::core::refactor::*;


/// A rename specification with all generated case variants.
#[derive(Debug, Clone)]
pub struct RenameSpec {
    pub from: String,
    pub to: String,
    pub scope: RenameScope,
    pub variants: Vec<CaseVariant>,
    /// When true, use exact string matching (no boundary detection).
    pub literal: bool,
}
