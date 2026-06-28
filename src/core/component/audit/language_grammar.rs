use serde::{Deserialize, Serialize};

use super::extend_unique;

/// A component-supplied grammar describing how to count top-level item
/// declarations in a family of source files.
///
/// Core stays ecosystem-agnostic: it never hardcodes language, framework, or
/// project item keywords. All extensions, prefixes, and markers come from
/// config. The structural detector applies this grammar generically:
///
/// 1. Walk each line of the file in order.
/// 2. If a line (trimmed) equals one of `ignore_after_line_equals`, stop
///    counting the remainder of the file (e.g. an inline test-module marker).
/// 3. Skip lines that are not at zero indentation.
/// 4. Strip at most one matching prefix from `visibility_prefixes` (first match
///    wins), then repeatedly strip matching prefixes from `modifier_prefixes`
///    until none match (so chained modifiers like `async` then `unsafe` are
///    both removed).
/// 5. If the remainder starts with any of `item_declaration_prefixes`, count it
///    as one top-level item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LanguageGrammar {
    /// File extensions (without dot) this grammar applies to, e.g.
    /// `["js", "jsx", "mjs", "ts", "tsx"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Prefixes that mark a top-level item declaration once visibility and
    /// modifier prefixes are stripped, e.g. `"fn "`, `"struct "`, `"class "`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_declaration_prefixes: Vec<String>,
    /// Visibility prefixes stripped FIRST (at most one, first match wins),
    /// e.g. `"pub(crate) "`, `"pub "`, `"public "`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibility_prefixes: Vec<String>,
    /// Modifier prefixes stripped SECOND, repeatedly until none match, e.g.
    /// `"async "`, `"unsafe "`, `"static "`, `"final "`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifier_prefixes: Vec<String>,
    /// When a line (trimmed) equals one of these, stop counting the remainder of
    /// the file, e.g. `"#[cfg(test)]"` to exclude inline test modules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_after_line_equals: Vec<String>,
}

impl LanguageGrammar {
    /// Whether this grammar applies to the given file extension (without dot).
    pub fn matches_extension(&self, ext: &str) -> bool {
        self.file_extensions.iter().any(|e| e == ext)
    }

    /// Count top-level item declarations in `content` using this grammar.
    ///
    /// Lightweight pattern matching, not full parsing — we just need
    /// approximate counts for threshold detection, not exact ASTs.
    pub fn count_items(&self, content: &str) -> usize {
        let mut count = 0;
        let mut stop = false;

        for line in content.lines() {
            let trimmed = line.trim();

            if !stop
                && self
                    .ignore_after_line_equals
                    .iter()
                    .any(|marker| marker == trimmed)
            {
                stop = true;
                continue;
            }
            if stop {
                continue;
            }

            // Only count items at top level (zero indentation).
            let indent = line.len() - line.trim_start().len();
            if indent > 0 {
                continue;
            }

            // Strip at most one visibility prefix (first match wins).
            let mut rest = trimmed;
            for prefix in &self.visibility_prefixes {
                if let Some(stripped) = rest.strip_prefix(prefix.as_str()) {
                    rest = stripped;
                    break;
                }
            }

            // Strip modifier prefixes repeatedly until none match.
            loop {
                let mut stripped_any = false;
                for prefix in &self.modifier_prefixes {
                    if let Some(stripped) = rest.strip_prefix(prefix.as_str()) {
                        rest = stripped;
                        stripped_any = true;
                        break;
                    }
                }
                if !stripped_any {
                    break;
                }
            }

            if self
                .item_declaration_prefixes
                .iter()
                .any(|prefix| rest.starts_with(prefix.as_str()))
            {
                count += 1;
            }
        }

        count
    }
}

/// Look up the grammar whose `file_extensions` contains `ext`.
pub fn grammar_for_extension<'a>(
    grammars: &'a [LanguageGrammar],
    ext: &str,
) -> Option<&'a LanguageGrammar> {
    grammars
        .iter()
        .find(|grammar| grammar.matches_extension(ext))
}

pub(super) fn merge_language_grammars(
    target: &mut Vec<LanguageGrammar>,
    source: &[LanguageGrammar],
) {
    extend_unique(target, source);
}
