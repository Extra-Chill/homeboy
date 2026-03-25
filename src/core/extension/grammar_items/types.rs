//! types — extracted from grammar_items.rs.

use serde::{Deserialize, Serialize};
use super::super::grammar::{self, Grammar, Symbol};


/// A parsed top-level item with full boundaries and source text.
///
/// This is the core equivalent of `extension::ParsedItem`, produced entirely
/// from grammar patterns without calling extension scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item: function, struct, enum, trait, impl, const, static, type_alias.
    pub kind: String,
    /// Start line (1-indexed, includes doc comments and attributes).
    pub start_line: usize,
    /// End line (1-indexed, inclusive).
    pub end_line: usize,
    /// The extracted source code (including doc comments and attributes).
    pub source: String,
    /// Visibility: "pub", "pub(crate)", "pub(super)", or "" for private.
    #[serde(default)]
    pub visibility: String,
}
