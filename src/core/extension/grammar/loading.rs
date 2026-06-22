//! Grammar loading — read and parse grammar files from extensions.

use std::path::Path;

use crate::core::engine::local_files;
use crate::core::error::{Error, Result};

use super::types::Grammar;

// ============================================================================
// Grammar loading
// ============================================================================

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

/// Load a grammar from an extension directory.
///
/// Probes for `grammar.toml`, `grammar.json`, then `<language>/grammar.toml`
/// and returns the first match. Returns `None` if no grammar file is found
/// or parsing fails.
pub fn load_for_extension_path(extension_path: &Path, language: &str) -> Option<Grammar> {
    let toml_path = extension_path.join("grammar.toml");
    if toml_path.exists() {
        return load_grammar(&toml_path).ok();
    }

    let json_path = extension_path.join("grammar.json");
    if json_path.exists() {
        return load_grammar_json(&json_path).ok();
    }

    let lang_toml = extension_path.join(language).join("grammar.toml");
    if lang_toml.exists() {
        return load_grammar(&lang_toml).ok();
    }

    None
}
