use serde::{Deserialize, Serialize};

/// Component-owned, language-agnostic grammar for counting top-level item
/// declarations in a source file.
///
/// Core owns only the mechanical counting algorithm; every language-specific
/// literal (file extensions, declaration keywords, visibility and modifier
/// prefixes) is supplied here by component configuration. This keeps the
/// structural detector free of ecosystem keywords while preserving the exact
/// counting behavior the detector previously hard-coded.
///
/// Counting algorithm (per zero-indentation line, after an optional
/// language-defined "stop after this line" marker disables further counting):
///   1. Strip at most one matching `visibility_prefixes` entry.
///   2. Repeatedly strip any matching `modifier_prefixes` entry.
///   3. Count the line if the remainder `starts_with` any
///      `item_declaration_prefixes` entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct LanguageGrammar {
    /// File extensions (without the dot) this grammar applies to, e.g. `rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Prefixes that introduce a top-level item declaration once visibility and
    /// modifier prefixes have been stripped, e.g. `fn `, `struct `, `class `.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_declaration_prefixes: Vec<String>,
    /// Visibility prefixes; at most one is stripped from the front of a line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibility_prefixes: Vec<String>,
    /// Modifier prefixes; stripped repeatedly until none match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifier_prefixes: Vec<String>,
    /// Trimmed line values that, once seen, stop all further counting in the
    /// file (e.g. a test-module attribute). Compared against the trimmed line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_after_line_equals: Vec<String>,
}

impl LanguageGrammar {
    /// Whether this grammar applies to the given file extension.
    pub fn matches_extension(&self, ext: &str) -> bool {
        self.file_extensions.iter().any(|e| e == ext)
    }

    /// Count top-level item declarations in `content` using this grammar.
    ///
    /// Reproduces the structural detector's lightweight, zero-indentation
    /// pattern match: only lines with no leading indentation are considered,
    /// and counting stops entirely after any `ignore_after_line_equals` marker.
    pub fn count_items(&self, content: &str) -> usize {
        let mut count = 0;
        let mut stopped = false;

        for line in content.lines() {
            let trimmed = line.trim();

            if !stopped && self.ignore_after_line_equals.iter().any(|m| m == trimmed) {
                stopped = true;
                continue;
            }
            if stopped {
                continue;
            }

            // Only count items at top level (zero indentation).
            let indent = line.len() - line.trim_start().len();
            if indent > 0 {
                continue;
            }

            let mut rest = trimmed;

            // Strip at most one visibility prefix.
            for prefix in &self.visibility_prefixes {
                if let Some(stripped) = rest.strip_prefix(prefix.as_str()) {
                    rest = stripped;
                    break;
                }
            }

            // Repeatedly strip modifier prefixes.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_grammar() -> LanguageGrammar {
        LanguageGrammar {
            file_extensions: vec!["rs".to_string()],
            item_declaration_prefixes: vec![
                "fn ".to_string(),
                "struct ".to_string(),
                "enum ".to_string(),
                "const ".to_string(),
                "static ".to_string(),
                "type ".to_string(),
                "trait ".to_string(),
                "impl ".to_string(),
                "impl<".to_string(),
            ],
            visibility_prefixes: vec![
                "pub(crate) ".to_string(),
                "pub(super) ".to_string(),
                "pub ".to_string(),
            ],
            modifier_prefixes: vec!["async ".to_string(), "unsafe ".to_string()],
            ignore_after_line_equals: vec!["#[cfg(test)]".to_string()],
        }
    }

    #[test]
    fn counts_zero_indent_items_only() {
        let content = "fn a() {}\n    fn nested() {}\nstruct S {}\n";
        assert_eq!(rust_grammar().count_items(content), 2);
    }

    #[test]
    fn strips_one_visibility_then_modifiers() {
        let content = "pub(crate) fn a() {}\npub async fn b() {}\npub(super) const X: i32 = 1;\n";
        assert_eq!(rust_grammar().count_items(content), 3);
    }

    #[test]
    fn stops_counting_after_marker() {
        let content = "fn a() {}\n#[cfg(test)]\nfn would_count() {}\n";
        assert_eq!(rust_grammar().count_items(content), 1);
    }

    #[test]
    fn matches_extension_checks_membership() {
        let grammar = rust_grammar();
        assert!(grammar.matches_extension("rs"));
        assert!(!grammar.matches_extension("php"));
    }
}
