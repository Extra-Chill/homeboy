//! Safety checks for generated rename plans.

use super::{FileEdit, FileRename, RenameWarning};
use std::collections::HashMap;
use std::path::Path;

/// Detect potential collisions in rename results.
///
/// Checks for:
/// 1. File rename targets that already exist on disk
/// 2. Duplicate identifiers within the same indentation block in edited files
///    (e.g., two struct fields both named `extensions` after rename)
pub(super) fn detect_collisions(
    edits: &[FileEdit],
    file_renames: &[FileRename],
    root: &Path,
) -> Vec<RenameWarning> {
    let mut warnings = Vec::new();

    // Check file rename collisions: target already exists.
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

    // Check content collisions: duplicate identifiers at the same indentation.
    for edit in edits {
        detect_duplicate_identifiers(&edit.file, &edit.new_content, &mut warnings);
    }

    warnings
}

/// Scan edited content for lines at the same indentation that introduce
/// duplicate field/identifier names. This catches the case where renaming
/// `modules` -> `extensions` creates a collision with an existing `extensions` field.
pub(super) fn detect_duplicate_identifiers(
    file: &str,
    content: &str,
    warnings: &mut Vec<RenameWarning>,
) {
    let lines: Vec<&str> = content.lines().collect();

    // Group lines by indentation level, looking for struct-like blocks
    // (lines with the same leading whitespace that contain identifier patterns).
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Look for struct/enum/block openers.
        if trimmed.ends_with('{') || trimmed.ends_with("{{") {
            let block_indent = leading_spaces(lines.get(i + 1).unwrap_or(&""));
            if block_indent == 0 {
                i += 1;
                continue;
            }

            // Collect identifiers at this indent level until the current block
            // closes. Track nested brace depth so keys in separate nested
            // object literals are not treated as siblings of the outer block.
            let mut seen: HashMap<String, usize> = HashMap::new();
            let mut j = i + 1;
            let mut nested_depth = 0isize;

            while j < lines.len() {
                let block_line = lines[j];
                let block_trimmed = block_line.trim();

                // Block ended.
                if nested_depth == 0 && (block_trimmed == "}" || block_trimmed == "},") {
                    break;
                }

                // Only check direct child declarations at this exact indent level.
                if nested_depth == 0 && leading_spaces(block_line) == block_indent {
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

                nested_depth += brace_delta(block_trimmed);

                j += 1;
            }

            i = j;
        } else {
            i += 1;
        }
    }
}

/// Return the net brace change for a line, ignoring braces inside quoted
/// strings. This is intentionally lightweight; it only supports collision
/// detection heuristics, not full language parsing.
fn brace_delta(line: &str) -> isize {
    let mut delta = 0isize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && (in_single || in_double) {
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if in_single || in_double {
            continue;
        }

        match ch {
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }

    delta
}

/// Count leading spaces on a line.
fn leading_spaces(line: &str) -> usize {
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
pub(super) fn extract_field_identifier(trimmed: &str) -> Option<String> {
    // Skip attributes, comments, empty lines.
    if trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.is_empty()
    {
        return None;
    }

    // Strip visibility modifiers.
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub(super) "))
        .or_else(|| trimmed.strip_prefix("pub "))
        .unwrap_or(trimmed);

    // Strip let/fn/const/static.
    let rest = rest
        .strip_prefix("let mut ")
        .or_else(|| rest.strip_prefix("let "))
        .or_else(|| rest.strip_prefix("fn "))
        .or_else(|| rest.strip_prefix("const "))
        .or_else(|| rest.strip_prefix("static "))
        .unwrap_or(rest);

    // Extract identifier (alphanumeric + underscore until : or ( or = or space).
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if ident.is_empty() {
        return None;
    }

    if is_language_keyword(&ident) {
        return None;
    }

    // Must be followed by : or ( or = or < (type params) to be an identifier.
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

fn is_language_keyword(ident: &str) -> bool {
    matches!(
        ident,
        "as" | "async"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "continue"
            | "default"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "from"
            | "if"
            | "impl"
            | "import"
            | "in"
            | "interface"
            | "loop"
            | "match"
            | "mod"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "use"
            | "while"
    )
}
