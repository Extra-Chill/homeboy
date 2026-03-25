//! cross_separator_join — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use serde::Serialize;
use std::collections::HashMap;
use crate::core::refactor::rename::RenameResult;
use crate::core::refactor::rename::RenameScope;


/// Split a term into its constituent words, regardless of naming convention.
///
/// Handles:
/// - `kebab-case` → `["kebab", "case"]`
/// - `snake_case` → `["snake", "case"]`
/// - `camelCase` → `["camel", "case"]`
/// - `PascalCase` → `["pascal", "case"]`
/// - `UPPER_SNAKE` → `["upper", "snake"]`
/// - `WPAgent` → `["wp", "agent"]` (consecutive uppercase → separate word)
/// - `XMLParser` → `["xml", "parser"]`
/// - `data-machine-agent` → `["data", "machine", "agent"]`
/// - Mixed: `my_WPAgent-thing` → `["my", "wp", "agent", "thing"]`
///
/// All returned words are lowercase.
pub(crate) fn split_words(term: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = term.chars().collect();
    let len = chars.len();

    for i in 0..len {
        let c = chars[i];

        // Separators: hyphens, underscores, spaces, dots
        if c == '-' || c == '_' || c == ' ' || c == '.' {
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
            continue;
        }

        if c.is_uppercase() && !current.is_empty() {
            let prev = chars[i - 1];
            // Split on camelCase boundary (lowercase/digit → uppercase)
            // or consecutive-uppercase boundary (uppercase → uppercase+lowercase)
            let is_camel_boundary = prev.is_lowercase() || prev.is_ascii_digit();
            let is_acronym_boundary =
                prev.is_uppercase() && i + 1 < len && chars[i + 1].is_lowercase();

            if is_camel_boundary || is_acronym_boundary {
                words.push(current.to_lowercase());
                current.clear();
            }
        }

        current.push(c);
    }

/// Join words as kebab-case: `["data", "machine", "agent"]` → `"data-machine-agent"`
pub(crate) fn join_kebab(words: &[String]) -> String {
    words.join("-")
}

/// Join words as snake_case: `["data", "machine", "agent"]` → `"data_machine_agent"`
pub(crate) fn join_snake(words: &[String]) -> String {
    words.join("_")
}

/// Join words as UPPER_SNAKE: `["data", "machine", "agent"]` → `"DATA_MACHINE_AGENT"`
pub(crate) fn join_upper_snake(words: &[String]) -> String {
    words
        .iter()
        .map(|w| w.to_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Join words as PascalCase: `["data", "machine", "agent"]` → `"DataMachineAgent"`
pub(crate) fn join_pascal(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join("")
}

/// Join words as camelCase: `["data", "machine", "agent"]` → `"dataMachineAgent"`
pub(crate) fn join_camel(words: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            parts.push(w.to_lowercase());
        } else {
            parts.push(capitalize(w));
        }
    }
    parts.join("")
}

/// Join words as display name: `["data", "machine", "agent"]` → `"Data Machine Agent"`
pub(crate) fn join_display(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a ScanConfig appropriate for rename operations.
pub(crate) fn scan_config_for_scope(scope: &RenameScope) -> ScanConfig {
    let extensions = match scope {
        RenameScope::Code => ExtensionFilter::Except(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::Config => ExtensionFilter::Only(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::All => ExtensionFilter::SourceDefaults,
    };

    ScanConfig {
        extensions,
        ..ScanConfig::default()
    }
}

/// Apply rename edits and file renames to disk.
pub fn apply_renames(result: &mut RenameResult, root: &Path) -> Result<()> {
    // Apply content edits first
    for edit in &result.edits {
        let path = root.join(&edit.file);
        std::fs::write(&path, &edit.new_content).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
    }

    // Apply file renames (sort by path depth descending so children rename before parents)
    let mut renames = result.file_renames.clone();
    renames.sort_by(|a, b| {
        b.from
            .matches('/')
            .count()
            .cmp(&a.from.matches('/').count())
    });

    for rename in &renames {
        let from = root.join(&rename.from);
        let to = root.join(&rename.to);

        // Create parent dirs if needed
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if from.exists() {
            std::fs::rename(&from, &to).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("rename {} → {}", from.display(), to.display())),
                )
            })?;
        }
    }

    result.applied = true;
    Ok(())
}
