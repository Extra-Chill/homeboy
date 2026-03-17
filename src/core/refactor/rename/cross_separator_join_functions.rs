//! cross_separator_join_functions — extracted from mod.rs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::core::refactor::rename::rename_scope::RenameScope;
use crate::core::refactor::rename::rename_targeting::RenameTargeting;
use crate::core::refactor::rename::find_references_with_targeting;
use crate::core::refactor::rename::leading_spaces;
use crate::core::refactor::rename::types::RenameWarning;
use crate::core::refactor::rename::default;
use crate::core::refactor::rename::rename_spec::RenameSpec;
use crate::core::refactor::rename::types::Reference;
use crate::core::refactor::rename::extract_field_identifier;
use crate::core::refactor::rename::new;
use crate::core::refactor::*;


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

    if !current.is_empty() {
        words.push(current.to_lowercase());
    }

    words
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

/// Find all references to the rename term across the codebase.
///
/// After the initial pass, discovers additional case variants that exist in the
/// codebase but weren't generated (e.g., `WPAgent` when `WpAgent` was generated).
pub fn find_references(spec: &RenameSpec, root: &Path) -> Vec<Reference> {
    find_references_with_targeting(spec, root, &RenameTargeting::default())
}

/// Scan edited content for lines at the same indentation that introduce
/// duplicate field/identifier names. This catches the case where renaming
/// `modules` → `extensions` creates a collision with an existing `extensions` field.
pub(crate) fn detect_duplicate_identifiers(file: &str, content: &str, warnings: &mut Vec<RenameWarning>) {
    let lines: Vec<&str> = content.lines().collect();

    // Group lines by indentation level, looking for struct-like blocks
    // (lines with the same leading whitespace that contain identifier patterns)
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Look for struct/enum/block openers
        if trimmed.ends_with('{') || trimmed.ends_with("{{") {
            let block_indent = leading_spaces(lines.get(i + 1).unwrap_or(&""));
            if block_indent == 0 {
                i += 1;
                continue;
            }

            // Collect identifiers at this indent level until block closes
            let mut seen: HashMap<String, usize> = HashMap::new();
            let mut j = i + 1;

            while j < lines.len() {
                let block_line = lines[j];
                let block_trimmed = block_line.trim();

                // Block ended
                if block_trimmed == "}" || block_trimmed == "}," {
                    break;
                }

                // Only check lines at this exact indent level
                if leading_spaces(block_line) == block_indent {
                    if let Some(ident) = extract_field_identifier(block_trimmed) {
                        if let Some(&first_line) = seen.get(&ident) {
                            warnings.push(RenameWarning {
                                kind: "duplicate_identifier".to_string(),
                                file: file.to_string(),
                                line: Some(j + 1),
                                message: format!(
                                    "Duplicate identifier '{}' at line {} (first at line {})",
                                    ident,
                                    j + 1,
                                    first_line
                                ),
                            });
                        } else {
                            seen.insert(ident, j + 1);
                        }
                    }
                }

                j += 1;
            }

            i = j;
        } else {
            i += 1;
        }
    }
}
