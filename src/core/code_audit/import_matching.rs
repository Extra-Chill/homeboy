//! import_matching — extracted from conventions.rs.

/// Check whether an expected import is satisfied by a file's actual imports,
/// accounting for grouped imports, path equivalence, and actual usage.
///
/// Returns `true` (import present or unnecessary) when:
/// 1. Exact match exists in imports
/// 2. A grouped import covers it (e.g., `super::{CmdResult, X}` satisfies `super::CmdResult`)
/// 3. An equivalent path provides the same terminal name
///    (e.g., `crate::commands::CmdResult` satisfies `super::CmdResult`)
/// 4. The file doesn't reference the terminal name outside import lines
///    (the import would be unused — not a real convention violation)
pub(crate) fn has_import(expected: &str, actual_imports: &[String], file_content: &str) -> bool {
    // 1. Exact match
    if actual_imports.iter().any(|imp| imp == expected) {
        return true;
    }

    // Extract terminal name (last segment after :: or \)
    let terminal = expected
        .rsplit("::")
        .next()
        .unwrap_or(expected)
        .rsplit('\\')
        .next()
        .unwrap_or(expected);
    // Extract prefix (everything before the terminal name)
    let prefix_len = expected.len() - terminal.len();
    let prefix = if prefix_len > 2 {
        // Strip trailing :: or \
        let p = &expected[..prefix_len];
        let p = p.strip_suffix("::").or_else(|| p.strip_suffix('\\')).unwrap_or(p);
        Some(p)
    } else if prefix_len > 0 {
        Some(&expected[..prefix_len - 1])  // strip single separator char
    } else {
        None
    };

    // 2 & 3. Check all actual imports for grouped coverage or path equivalence
    for imp in actual_imports {
        // Grouped import with matching prefix: super::{CmdResult, X}
        if let Some(pfx) = prefix {
            for sep in &["::", "\\"] {
                let group_prefix = format!("{}{}{}", pfx, sep, "{");
                if imp.starts_with(&group_prefix) && grouped_import_contains(imp, terminal) {
                    return true;
                }
            }
        }

        // Grouped import from any path containing the terminal name
        if (imp.contains("::{") || imp.contains("\\{"))
            && grouped_import_contains(imp, terminal)
        {
            return true;
        }

        // Path equivalence: different path, same terminal name
        let imp_terminal = imp
            .rsplit("::")
            .next()
            .unwrap_or(imp)
            .rsplit('\\')
            .next()
            .unwrap_or(imp);
        if imp_terminal == terminal && !imp.contains("::{") && !imp.contains("\\{") {
            return true;
        }
    }

    // 4. Usage check: if the terminal name isn't referenced outside imports,
    //    the import would be unused — not a real convention violation
    if !terminal.is_empty() && !content_references_name(file_content, terminal) {
        return true;
    }

    false
}

/// Check if a grouped import (e.g., `serde::{Deserialize, Serialize}`) contains a name.
pub(crate) fn grouped_import_contains(import: &str, name: &str) -> bool {
    if let Some(brace_start) = import.find('{') {
        let brace_end = import.rfind('}').unwrap_or(import.len());
        let inner = &import[brace_start + 1..brace_end];
        inner.split(',').map(|s| s.trim()).any(|n| n == name)
    } else {
        false
    }
}

/// Check if file content references a name outside of import/use statements.
pub(crate) fn content_references_name(content: &str, name: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        // Skip import/use lines — we're looking for usage, not declarations
        if trimmed.starts_with("use ") || trimmed.starts_with("import ") {
            continue;
        }
        if contains_word(trimmed, name) {
            return true;
        }
    }
    false
}

/// Check if `text` contains `word` as a standalone word (not a substring).
pub(crate) fn contains_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0
            || !text.as_bytes()[abs - 1].is_ascii_alphanumeric()
                && text.as_bytes()[abs - 1] != b'_';
        let after = abs + word.len();
        let after_ok = after >= text.len()
            || !text.as_bytes()[after].is_ascii_alphanumeric()
                && text.as_bytes()[after] != b'_';
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_import_exact_match() {
        let imports = vec!["super::CmdResult".to_string()];
        assert!(has_import("super::CmdResult", &imports, "use super::CmdResult;\nfn run() -> CmdResult<T> {}"));
    }

    #[test]
    fn has_import_grouped_import() {
        // super::{CmdResult, DynamicSetArgs} should satisfy super::CmdResult
        let imports = vec!["super::{CmdResult, DynamicSetArgs}".to_string()];
        assert!(has_import("super::CmdResult", &imports, "fn run() -> CmdResult<T> {}"));
    }

    #[test]
    fn has_import_grouped_serde() {
        // serde::{Deserialize, Serialize} should satisfy serde::Serialize
        let imports = vec!["serde::{Deserialize, Serialize}".to_string()];
        assert!(has_import("serde::Serialize", &imports, "#[derive(Serialize)]\nstruct Foo {}"));
    }

    #[test]
    fn has_import_path_equivalence() {
        // crate::commands::CmdResult should satisfy super::CmdResult
        let imports = vec!["crate::commands::CmdResult".to_string()];
        assert!(has_import("super::CmdResult", &imports, "fn run() -> CmdResult<T> {}"));
    }

    #[test]
    fn has_import_unused_name_skipped() {
        // File doesn't use Serialize at all — missing import is irrelevant
        let imports = vec![];
        let content = "pub fn run() -> SomeOutput {}\n";
        assert!(has_import("serde::Serialize", &imports, content));
    }

    #[test]
    fn has_import_used_name_flagged() {
        // File uses Serialize but doesn't import it — real finding
        let imports = vec![];
        let content = "#[derive(Serialize)]\npub struct Output {}\n";
        assert!(!has_import("serde::Serialize", &imports, content));
    }

    #[test]
    fn has_import_grouped_from_alternate_path() {
        // crate::commands::{CmdResult, GlobalArgs} should satisfy super::CmdResult
        let imports = vec!["crate::commands::{CmdResult, GlobalArgs}".to_string()];
        assert!(has_import("super::CmdResult", &imports, "fn run() -> CmdResult<T> {}"));
    }

    #[test]
    fn contains_word_matches_standalone() {
        assert!(contains_word("derive(Serialize)", "Serialize"));
        assert!(contains_word("use Serialize;", "Serialize"));
        assert!(!contains_word("SerializeMe", "Serialize"));
        assert!(!contains_word("MySerialize", "Serialize"));
        assert!(!contains_word("_Serialize_ext", "Serialize"));
    }

    #[test]
    fn grouped_import_contains_finds_name() {
        assert!(grouped_import_contains("super::{CmdResult, DynamicSetArgs}", "CmdResult"));
        assert!(grouped_import_contains("super::{CmdResult, DynamicSetArgs}", "DynamicSetArgs"));
        assert!(!grouped_import_contains("super::{CmdResult, DynamicSetArgs}", "GlobalArgs"));
        assert!(grouped_import_contains("serde::{Deserialize, Serialize}", "Serialize"));
    }
}
