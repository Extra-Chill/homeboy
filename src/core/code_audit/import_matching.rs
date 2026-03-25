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
        let p = p
            .strip_suffix("::")
            .or_else(|| p.strip_suffix('\\'))
            .unwrap_or(p);
        Some(p)
    } else if prefix_len > 0 {
        Some(&expected[..prefix_len - 1]) // strip single separator char
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
        if (imp.contains("::{") || imp.contains("\\{")) && grouped_import_contains(imp, terminal) {
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

    // 4. Local definition check: if the file defines the symbol locally,
    //    it doesn't need an import (e.g., `fn default_true() -> bool { true }`)
    if !terminal.is_empty() && content_defines_name(file_content, terminal) {
        return true;
    }

    // 5. Usage check: if the terminal name isn't referenced outside imports,
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

/// Check if file content contains a local definition of a name.
///
/// Detects `fn name`, `struct name`, `enum name`, `type name`, `const name`,
/// `static name`, `trait name`, and `macro_rules! name`. Skips import/use
/// lines to avoid false positives from re-exports.
///
/// This prevents the convention detector from flagging a "missing import" when
/// the file already defines the symbol locally (e.g., `fn default_true()`).
pub(crate) fn content_defines_name(content: &str, name: &str) -> bool {
    // Declaration keywords that introduce a named definition
    const DEF_KEYWORDS: &[&str] = &[
        "fn ",
        "struct ",
        "enum ",
        "type ",
        "const ",
        "static ",
        "trait ",
        "macro_rules! ",
    ];

    for line in content.lines() {
        let trimmed = line.trim();
        // Skip comments
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }
        // Skip import/use lines
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            continue;
        }
        // Strip visibility modifiers to find the keyword
        let stripped = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub(super) "))
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);
        // Also strip async/unsafe/const qualifiers before fn
        let stripped = stripped.strip_prefix("async ").unwrap_or(stripped);
        let stripped = stripped.strip_prefix("unsafe ").unwrap_or(stripped);

        for kw in DEF_KEYWORDS {
            if let Some(rest) = stripped.strip_prefix(kw) {
                // The name should appear right after the keyword, followed by
                // a non-identifier char (paren, brace, colon, angle bracket, etc.)
                if rest.starts_with(name) {
                    let after = &rest[name.len()..];
                    if after.is_empty()
                        || after.starts_with('(')
                        || after.starts_with('<')
                        || after.starts_with(':')
                        || after.starts_with('{')
                        || after.starts_with(' ')
                        || after.starts_with(';')
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Check if file content references a name outside of import/use statements.
///
/// Skips references that only appear inside string literals within attribute macros
/// (e.g., `#[serde(default = "default_true")]`), since those are resolved by the
/// macro at compile time, not by Rust's import system.
pub(crate) fn content_references_name(content: &str, name: &str) -> bool {
    let mut found_real_reference = false;

    for line in content.lines() {
        let trimmed = line.trim();
        // Skip import/use lines — we're looking for usage, not declarations
        if trimmed.starts_with("use ") || trimmed.starts_with("import ") {
            continue;
        }
        if !contains_word(trimmed, name) {
            continue;
        }
        // If the name only appears inside a string literal on an attribute line,
        // it's a macro-resolved reference (serde, clap, etc.), not a real import.
        if is_only_in_attribute_string(trimmed, name) {
            continue;
        }
        found_real_reference = true;
        break;
    }
    found_real_reference
}

/// Check if `name` only appears inside string literals on an attribute line.
///
/// Catches patterns like `#[serde(default = "default_true")]` where the name
/// is referenced by a proc macro (serde, clap, etc.) via a string path, not
/// by Rust's module/import system. These don't need a `use` statement.
fn is_only_in_attribute_string(line: &str, name: &str) -> bool {
    let trimmed = line.trim();

    // Must be an attribute line or a field with an attribute
    let is_attribute_context =
        trimmed.starts_with("#[") || trimmed.starts_with("#![") || trimmed.contains("#[");

    if !is_attribute_context {
        return false;
    }

    // Check if ALL occurrences of `name` on this line are inside quoted strings.
    // Walk through the line tracking whether we're inside a string literal.
    let name_bytes = name.as_bytes();
    let line_bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut all_in_string = true;
    let mut found_any = false;

    while i < line_bytes.len() {
        if line_bytes[i] == b'"' && (i == 0 || line_bytes[i - 1] != b'\\') {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if i + name_bytes.len() <= line_bytes.len()
            && &line_bytes[i..i + name_bytes.len()] == name_bytes
        {
            // Check word boundaries
            let before_ok =
                i == 0 || (!line_bytes[i - 1].is_ascii_alphanumeric() && line_bytes[i - 1] != b'_');
            let after = i + name_bytes.len();
            let after_ok = after >= line_bytes.len()
                || (!line_bytes[after].is_ascii_alphanumeric() && line_bytes[after] != b'_');
            if before_ok && after_ok {
                found_any = true;
                if !in_string {
                    all_in_string = false;
                }
            }
        }
        i += 1;
    }

    found_any && all_in_string
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
            || !text.as_bytes()[after].is_ascii_alphanumeric() && text.as_bytes()[after] != b'_';
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
        assert!(has_import(
            "super::CmdResult",
            &imports,
            "use super::CmdResult;\nfn run() -> CmdResult<T> {}"
        ));
    }

    #[test]
    fn has_import_grouped_import() {
        // super::{CmdResult, DynamicSetArgs} should satisfy super::CmdResult
        let imports = vec!["super::{CmdResult, DynamicSetArgs}".to_string()];
        assert!(has_import(
            "super::CmdResult",
            &imports,
            "fn run() -> CmdResult<T> {}"
        ));
    }

    #[test]
    fn has_import_grouped_serde() {
        // serde::{Deserialize, Serialize} should satisfy serde::Serialize
        let imports = vec!["serde::{Deserialize, Serialize}".to_string()];
        assert!(has_import(
            "serde::Serialize",
            &imports,
            "#[derive(Serialize)]\nstruct Foo {}"
        ));
    }

    #[test]
    fn has_import_path_equivalence() {
        // crate::commands::CmdResult should satisfy super::CmdResult
        let imports = vec!["crate::commands::CmdResult".to_string()];
        assert!(has_import(
            "super::CmdResult",
            &imports,
            "fn run() -> CmdResult<T> {}"
        ));
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
        assert!(has_import(
            "super::CmdResult",
            &imports,
            "fn run() -> CmdResult<T> {}"
        ));
    }

    #[test]
    fn has_import_local_definition_satisfies() {
        // File defines fn default_true locally — no import needed
        let imports = vec![];
        let content = r#"
#[serde(default = "default_true")]
pub enabled: bool,

fn default_true() -> bool {
    true
}
"#;
        assert!(has_import(
            "crate::core::defaults::default_true",
            &imports,
            content
        ));
    }

    #[test]
    fn has_import_local_pub_fn_satisfies() {
        let imports = vec![];
        let content = "pub fn helper() -> String { String::new() }\nfn main() { helper(); }\n";
        assert!(has_import("super::helper", &imports, content));
    }

    #[test]
    fn has_import_local_struct_satisfies() {
        let imports = vec![];
        let content = "pub struct Config { pub name: String }\nfn use_it(c: Config) {}\n";
        assert!(has_import("crate::types::Config", &imports, content));
    }

    #[test]
    fn has_import_local_async_fn_satisfies() {
        let imports = vec![];
        let content = "pub async fn fetch() -> Result<()> { Ok(()) }\nfn main() { fetch(); }\n";
        assert!(has_import("super::fetch", &imports, content));
    }

    #[test]
    fn has_import_no_local_definition_still_flags() {
        // File uses Config but doesn't define it — should still flag
        let imports = vec![];
        let content = "fn build() -> Config { Config::default() }\n";
        assert!(!has_import("crate::types::Config", &imports, content));
    }

    #[test]
    fn content_defines_name_detects_fn() {
        assert!(content_defines_name(
            "fn default_true() -> bool { true }",
            "default_true"
        ));
    }

    #[test]
    fn content_defines_name_detects_pub_fn() {
        assert!(content_defines_name(
            "pub fn helper() -> String {}",
            "helper"
        ));
    }

    #[test]
    fn content_defines_name_detects_pub_crate_struct() {
        assert!(content_defines_name(
            "pub(crate) struct Config {}",
            "Config"
        ));
    }

    #[test]
    fn content_defines_name_detects_async_fn() {
        assert!(content_defines_name(
            "pub async fn fetch() -> Result<()> {}",
            "fetch"
        ));
    }

    #[test]
    fn content_defines_name_rejects_substring() {
        // "default_true_ext" should not match "default_true"
        assert!(!content_defines_name(
            "fn default_true_ext() -> bool { true }",
            "default_true"
        ));
    }

    #[test]
    fn content_defines_name_skips_comments() {
        assert!(!content_defines_name(
            "// fn default_true() -> bool { true }",
            "default_true"
        ));
    }

    #[test]
    fn content_defines_name_skips_use_statements() {
        assert!(!content_defines_name(
            "use crate::defaults::default_true;",
            "default_true"
        ));
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
    fn serde_attribute_string_not_a_real_reference() {
        // The name "default_true" only appears inside a serde attribute string —
        // not a real code reference, so missing import should not be flagged.
        let content = r#"    #[serde(default = "default_true")]
    pub enabled: bool,
"#;
        assert!(!content_references_name(content, "default_true"));
    }

    #[test]
    fn serde_attribute_string_with_real_usage_is_flagged() {
        // Name appears both in a serde attribute string AND as a real call
        let content = r#"    #[serde(default = "default_true")]
    pub enabled: bool,
    let x = default_true();
"#;
        assert!(content_references_name(content, "default_true"));
    }

    #[test]
    fn non_attribute_reference_is_flagged() {
        let content = "let x = default_true();\n";
        assert!(content_references_name(content, "default_true"));
    }

    #[test]
    fn attribute_string_detection_basics() {
        assert!(is_only_in_attribute_string(
            r#"#[serde(default = "default_true")]"#,
            "default_true"
        ));
        assert!(!is_only_in_attribute_string(
            "let x = default_true();",
            "default_true"
        ));
        // Name outside quotes on an attribute line
        assert!(!is_only_in_attribute_string(
            r#"#[derive(default_true)]"#,
            "default_true"
        ));
    }

    #[test]
    fn grouped_import_contains_finds_name() {
        assert!(grouped_import_contains(
            "super::{CmdResult, DynamicSetArgs}",
            "CmdResult"
        ));
        assert!(grouped_import_contains(
            "super::{CmdResult, DynamicSetArgs}",
            "DynamicSetArgs"
        ));
        assert!(!grouped_import_contains(
            "super::{CmdResult, DynamicSetArgs}",
            "GlobalArgs"
        ));
        assert!(grouped_import_contains(
            "serde::{Deserialize, Serialize}",
            "Serialize"
        ));
    }

    #[test]
    fn test_grouped_import_contains_if_let_some_brace_start_import_find() {

        let _result = grouped_import_contains();
    }

    #[test]
    fn test_content_defines_name_if_let_some_rest_stripped_strip_prefix_kw() {

        let _result = content_defines_name();
    }

    #[test]
    fn test_content_defines_name_default_path() {

        let result = content_defines_name();
        assert!(result, "expected true when: default path");
    }

    #[test]
    fn test_content_defines_name_is_attribute_context() {

        let result = content_defines_name();
        assert!(!result, "expected false when: !is_attribute_context");
    }

    #[test]
    fn test_content_references_name_default_path() {

        let _result = content_references_name();
    }

    #[test]
    fn test_contains_word_while_let_some_pos_text_start_find_word() {

        let _result = contains_word();
    }

    #[test]
    fn test_contains_word_before_ok_after_ok() {

        let result = contains_word();
        assert!(result, "expected true when: before_ok && after_ok");
    }

}
