//! block_syntax — extracted from grammar.rs.

use serde::{Deserialize, Serialize};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use crate::error::{Error, Result};


/// Block (scope) delimiters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSyntax {
    /// Opening delimiter (default: "{").
    #[serde(default = "default_open")]
    pub open: String,

    /// Closing delimiter (default: "}").
    #[serde(default = "default_close")]
    pub close: String,
}

impl Default for BlockSyntax {
    fn default() -> Self {
        Self {
            open: "{".to_string(),
            close: "}".to_string(),
        }
    }
}
