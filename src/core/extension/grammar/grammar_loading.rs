//! grammar_loading — extracted from grammar.rs.

use std::path::Path;
use crate::engine::local_files;
use crate::error::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use super::Grammar;


/// Load a grammar from a TOML file.
pub fn load_grammar(path: &Path) -> Result<Grammar> {
    let content = local_files::read_file(path, "read grammar file")?;
    toml::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse grammar {}: {}", path.display(), e),
            Some("grammar.load".to_string()),
        )
    })
}

/// Load a grammar from a JSON file.
pub fn load_grammar_json(path: &Path) -> Result<Grammar> {
    let content = local_files::read_file(path, "read grammar file")?;
    serde_json::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse grammar {}: {}", path.display(), e),
            Some("grammar.load".to_string()),
        )
    })
}
