//! collision_detection — extracted from mod.rs.

use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::error::{Error, Result};
use serde::Serialize;
use crate::core::refactor::rename::RenameWarning;
use crate::core::refactor::rename::FileRename;
use crate::core::refactor::rename::FileEdit;


/// Detect potential collisions in rename results.
///
/// Checks for:
/// 1. File rename targets that already exist on disk
/// 2. Duplicate identifiers within the same indentation block in edited files
///    (e.g., two struct fields both named `extensions` after rename)
pub(crate) fn detect_collisions(
    edits: &[FileEdit],
    file_renames: &[FileRename],
    root: &Path,
) -> Vec<RenameWarning> {
    let mut warnings = Vec::new();

    // Check file rename collisions — target already exists
    for rename in file_renames {
        let target = root.join(&rename.to);
        if target.exists() {
            warnings.push(RenameWarning {
                kind: "file_collision".to_string(),
                file: rename.to.clone(),
                line: None,
                message: format!(
                    "Rename target '{}' already exists on disk (from '{}')",
                    rename.to, rename.from
                ),
            });
        }
    }

    // Check content collisions — duplicate identifiers at same indentation
    for edit in edits {
        detect_duplicate_identifiers(&edit.file, &edit.new_content, &mut warnings);
    }

    warnings
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

/// Count leading spaces on a line.
pub(crate) fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Extract the field/identifier name from a struct field or variable declaration line.
/// Returns the identifier if the line looks like a field declaration.
///
/// Matches patterns like:
/// - `pub field_name: Type,`
/// - `field_name: Type,`
/// - `pub(crate) field_name: Type,`
/// - `let field_name = ...`
/// - `fn field_name(...`
pub(crate) fn extract_field_identifier(trimmed: &str) -> Option<String> {
    // Skip attributes, comments, empty lines
    if trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.is_empty()
    {
        return None;
    }

    // Strip visibility modifiers
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub(super) "))
        .or_else(|| trimmed.strip_prefix("pub "))
        .unwrap_or(trimmed);

    // Strip let/fn/const/static
    let rest = rest
        .strip_prefix("let mut ")
        .or_else(|| rest.strip_prefix("let "))
        .or_else(|| rest.strip_prefix("fn "))
        .or_else(|| rest.strip_prefix("const "))
        .or_else(|| rest.strip_prefix("static "))
        .unwrap_or(rest);

    // Extract identifier (alphanumeric + underscore until : or ( or = or space)
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if ident.is_empty() {
        return None;
    }

    // Must be followed by : or ( or = or < (type params) to be an identifier
    let after = &rest[ident.len()..].trim_start();
    if after.starts_with(':')
        || after.starts_with('(')
        || after.starts_with('=')
        || after.starts_with('<')
    {
        Some(ident)
    } else {
        None
    }
}
