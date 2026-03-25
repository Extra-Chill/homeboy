//! symbol — extracted from grammar.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use regex::Regex;
use std::path::Path;
use crate::error::{Error, Result};


/// A symbol extracted from source code.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    /// What kind of concept this is (matches the pattern key in the grammar).
    /// e.g., "method", "class", "import", "namespace"
    pub concept: String,

    /// Named captures from the pattern match.
    /// e.g., {"name": "foo", "visibility": "pub", "params": "&self, key: &str"}
    pub captures: HashMap<String, String>,

    /// 1-indexed line number where the symbol was found.
    pub line: usize,

    /// Brace depth at the match location.
    pub depth: i32,

    /// The full matched text.
    pub matched_text: String,
}
