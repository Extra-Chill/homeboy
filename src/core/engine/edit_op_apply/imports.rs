use crate::core::code_audit::conventions::Language;

/// Check if an import should be skipped (already present or alias collision).
pub(super) fn should_skip_import(content: &str, import_line: &str, language: &Language) -> bool {
    import_already_present(content, import_line, language)
        || import_alias_collides(content, import_line, language)
}

/// Check whether an import line is already present in the file content.
///
/// Normalizes whitespace before comparison so `use std::path::{Path, PathBuf};`
/// matches `use  std::path::{Path,   PathBuf};`.
fn import_already_present(content: &str, import_line: &str, language: &Language) -> bool {
    let normalized_candidate = normalize_import_line(import_line);
    if normalized_candidate.is_empty() {
        return true;
    }

    content.lines().any(|line| {
        let trimmed = line.trim();
        if !is_import_line(trimmed, language) {
            return false;
        }
        normalize_import_line(trimmed) == normalized_candidate
    })
}

fn is_import_line(line: &str, language: &Language) -> bool {
    match language {
        Language::JavaScript | Language::TypeScript => line.starts_with("import "),
        Language::Unknown => line.starts_with("use "),
        _ => line.starts_with("use "),
    }
}

fn normalize_import_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the short name (alias) that an import resolves to.
///
/// `use Foo\Bar\Baz;`         -> `Baz`
/// `use Foo\Bar\Baz as Qux;`  -> `Qux`
/// `use foo::bar::Baz;`       -> `Baz`
/// `use foo::bar::Baz as Qux;`-> `Qux`
fn extract_import_alias(import_line: &str) -> Option<String> {
    let trimmed = import_line.trim().trim_end_matches(';');

    // Handle `as Alias`
    if let Some(as_pos) = trimmed.rfind(" as ") {
        let alias = trimmed[as_pos + 4..].trim();
        if !alias.is_empty() {
            return Some(alias.to_string());
        }
    }

    let path = if let Some(rest) = trimmed.strip_prefix("use ") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("import ") {
        rest.trim()
    } else {
        return None;
    };

    // Skip brace-grouped imports like `use foo::{A, B};`
    if path.contains('{') {
        return None;
    }

    // Extract the last segment: `Foo\Bar\Baz` -> `Baz`, `foo::bar::Baz` -> `Baz`
    let last = path.rsplit(['\\', ':']).find(|s| !s.is_empty())?;
    if last.is_empty() {
        return None;
    }

    Some(last.to_string())
}

/// Check if inserting `import_line` would create an alias collision with an
/// existing import in the file.
fn import_alias_collides(content: &str, import_line: &str, language: &Language) -> bool {
    let Some(candidate_alias) = extract_import_alias(import_line) else {
        return false;
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if !is_import_line(trimmed, language) {
            continue;
        }
        if let Some(existing_alias) = extract_import_alias(trimmed) {
            if existing_alias == candidate_alias
                && normalize_import_line(trimmed) != normalize_import_line(import_line)
            {
                return true;
            }
        }
    }

    false
}
