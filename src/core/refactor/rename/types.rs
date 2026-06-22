//! Core types for the rename engine — specs, scopes, contexts, and results.

use crate::core::error::{Error, Result};
use serde::Serialize;

use super::casing::{
    join_camel, join_display, join_kebab, join_pascal, join_snake, join_upper_snake, pluralize,
    split_words,
};

// ============================================================================
// Types
// ============================================================================

/// What scope to apply renames to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameScope {
    /// Source files only.
    Code,
    /// Config files only (homeboy.json, component configs).
    Config,
    /// Everything.
    All,
}

impl RenameScope {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "code" => Ok(RenameScope::Code),
            "config" => Ok(RenameScope::Config),
            "all" => Ok(RenameScope::All),
            _ => Err(Error::validation_invalid_argument(
                "scope",
                format!("Unknown scope '{}'. Use: code, config, all", s),
                None,
                None,
            )),
        }
    }
}

/// Syntactic context filter for rename matches.
///
/// Restricts which occurrences of a term get renamed based on their
/// syntactic position in the source code. Useful for selective renames
/// where only certain usages should change (e.g., rename an array key
/// but not a variable with the same name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameContext {
    /// Only match inside string literals (`'term'`, `"term"`) and
    /// property access (`.term`, `->term`, `::term`).
    Key,
    /// Only match variable references (`$term` in PHP, standalone identifiers
    /// NOT inside strings or property access).
    Variable,
    /// Only match function parameter definitions (inside parentheses
    /// following a function/fn keyword).
    Parameter,
    /// Match everything — current default behavior.
    All,
}

impl RenameContext {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "key" => Ok(RenameContext::Key),
            "variable" | "var" => Ok(RenameContext::Variable),
            "parameter" | "param" => Ok(RenameContext::Parameter),
            "all" => Ok(RenameContext::All),
            _ => Err(Error::validation_invalid_argument(
                "context",
                format!(
                    "Unknown context '{}'. Use: key, variable (var), parameter (param), all",
                    s
                ),
                None,
                None,
            )),
        }
    }

    /// Check whether a match at the given position in a line passes this context filter.
    ///
    /// - `line`: the full line content
    /// - `col`: 0-indexed byte offset of the match start within the line
    /// - `match_len`: byte length of the matched text
    pub fn matches(&self, line: &str, col: usize, match_len: usize) -> bool {
        match self {
            RenameContext::All => true,
            RenameContext::Key => is_key_context(line, col, match_len),
            RenameContext::Variable => is_variable_context(line, col),
            RenameContext::Parameter => is_parameter_context(line, col),
        }
    }
}

/// Check if match is inside a string literal or follows a property accessor.
fn is_key_context(line: &str, col: usize, match_len: usize) -> bool {
    let bytes = line.as_bytes();

    // Check if preceded by `.`, `->`, or `::` (property/method access)
    let before = &line[..col];
    let trimmed = before.trim_end();
    if trimmed.ends_with('.') || trimmed.ends_with("->") || trimmed.ends_with("::") {
        return true;
    }

    // Check if inside string quotes: count unescaped quotes before the match position.
    // If an odd number of single or double quotes precede us, we're inside a string.
    let match_end = col + match_len;
    for quote in [b'\'', b'"'] {
        let mut count = 0;
        let mut i = 0;
        while i < col {
            if bytes[i] == b'\\' {
                i += 2; // skip escaped char
                continue;
            }
            if bytes[i] == quote {
                count += 1;
            }
            i += 1;
        }
        if count % 2 == 1 {
            // Verify the closing quote is after the match
            let mut j = match_end;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote {
                    return true; // Inside a quoted string
                }
                j += 1;
            }
        }
    }

    false
}

/// Check if match is a variable reference (`$term` in PHP, or a standalone identifier
/// not inside strings or property access).
fn is_variable_context(line: &str, col: usize) -> bool {
    // PHP variable: preceded by `$`
    if col > 0 && line.as_bytes()[col - 1] == b'$' {
        return true;
    }

    // Standalone identifier: NOT inside quotes and NOT after `.`, `->`, `::`
    let before = &line[..col];
    let trimmed = before.trim_end();
    if trimmed.ends_with('.') || trimmed.ends_with("->") || trimmed.ends_with("::") {
        return false; // Property access — not a variable
    }

    // Not inside a string (simple odd-quote check)
    for quote in ['\'', '"'] {
        let count = before.chars().filter(|&c| c == quote).count();
        if count % 2 == 1 {
            return false; // Inside a string
        }
    }

    true
}

/// Check if match is inside a function parameter list.
fn is_parameter_context(line: &str, col: usize) -> bool {
    let before = &line[..col];

    // Look for an unclosed `(` that follows a function keyword
    let mut paren_depth: i32 = 0;
    for ch in before.chars().rev() {
        match ch {
            ')' => paren_depth += 1,
            '(' => {
                paren_depth -= 1;
                if paren_depth < 0 {
                    // We're inside an opening paren — check if it follows a function keyword
                    let before_paren = before[..before.rfind('(').unwrap_or(0)].trim_end();
                    return before_paren.ends_with("function")
                        || before_paren.ends_with("fn")
                        || before_paren.ends_with(')')  // return type: fn foo() -> Type
                        || before_paren.contains("function ")
                        || before_paren.contains("fn ");
                }
            }
            _ => {}
        }
    }

    false
}

/// A case variant of a rename term.
#[derive(Debug, Clone, Serialize)]
pub struct CaseVariant {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// A rename specification with all generated case variants.
#[derive(Debug, Clone)]
pub struct RenameSpec {
    pub from: String,
    pub to: String,
    pub scope: RenameScope,
    pub variants: Vec<CaseVariant>,
    /// When true, use exact string matching (no boundary detection).
    pub literal: bool,
    /// Syntactic context filter — restricts which occurrences get renamed.
    pub rename_context: RenameContext,
}

/// Optional file-targeting controls for rename operations.
#[derive(Debug, Clone)]
pub struct RenameTargeting {
    /// Include only files matching at least one glob. Empty = include all.
    pub include_globs: Vec<String>,
    /// Exclude files matching any glob.
    pub exclude_globs: Vec<String>,
    /// Whether file/directory renames should be generated/applied.
    pub rename_files: bool,
}

impl Default for RenameTargeting {
    fn default() -> Self {
        Self {
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            rename_files: true,
        }
    }
}

impl RenameSpec {
    /// Create a rename spec, auto-generating cross-separator case variants.
    ///
    /// Splits the `from` and `to` terms into constituent words, then generates
    /// all standard naming convention variants:
    ///
    /// - `kebab-case` (e.g., `sample-plugin-agent`)
    /// - `snake_case` (e.g., `sample_plugin_agent`)
    /// - `UPPER_SNAKE` (e.g., `SAMPLE_PLUGIN_AGENT`)
    /// - `PascalCase` (e.g., `SamplePluginAgent`)
    /// - `camelCase` (e.g., `samplePluginAgent`)
    /// - `Display Name` (e.g., `Sample Plugin Agent`)
    /// - Plus plural forms of each
    ///
    /// This means a single `--from wp-agent --to sample-plugin-agent` will also
    /// match and replace `wp_agent`, `WP_AGENT`, `WPAgent`, `wpAgent`, `WP Agent`,
    /// and all their plurals.
    pub fn new(from: &str, to: &str, scope: RenameScope) -> Self {
        let from_words = split_words(from);
        let to_words = split_words(to);

        let mut variants = Vec::new();

        // If word splitting produced words, generate cross-separator variants.
        // If it produced a single word (e.g., "widget"), the joins all collapse
        // to the same thing, and dedup handles it naturally.
        if !from_words.is_empty() && !to_words.is_empty() {
            // Singular forms — all naming conventions
            let join_fns: [fn(&[String]) -> String; 6] = [
                join_kebab,
                join_snake,
                join_upper_snake,
                join_pascal,
                join_camel,
                join_display,
            ];
            let labels = [
                "kebab",
                "snake_case",
                "UPPER_SNAKE",
                "PascalCase",
                "camelCase",
                "Display Name",
            ];

            for (label, join_fn) in labels.iter().zip(join_fns.iter()) {
                variants.push(CaseVariant {
                    from: join_fn(&from_words),
                    to: join_fn(&to_words),
                    label: label.to_string(),
                });
            }

            // Plural forms — pluralize the last word, then generate all conventions
            let mut from_words_plural = from_words.clone();
            let mut to_words_plural = to_words.clone();
            if let Some(last) = from_words_plural.last_mut() {
                *last = pluralize(last);
            }
            if let Some(last) = to_words_plural.last_mut() {
                *last = pluralize(last);
            }

            for (label, join_fn) in labels.iter().zip(join_fns.iter()) {
                variants.push(CaseVariant {
                    from: join_fn(&from_words_plural),
                    to: join_fn(&to_words_plural),
                    label: format!("plural {}", label),
                });
            }
        } else {
            // Fallback for empty/unparseable input — use the original simple logic
            variants.push(CaseVariant {
                from: from.to_lowercase(),
                to: to.to_lowercase(),
                label: "lowercase".to_string(),
            });
        }

        deduplicate_variants(&mut variants);

        RenameSpec {
            from: from.to_string(),
            to: to.to_string(),
            scope,
            variants,
            literal: false,
            rename_context: RenameContext::All,
        }
    }

    /// Create a literal rename spec — exact string match, no boundary detection,
    /// no case variant generation. The `from` string is matched as-is.
    pub fn literal(from: &str, to: &str, scope: RenameScope) -> Self {
        let variants = vec![CaseVariant {
            from: from.to_string(),
            to: to.to_string(),
            label: "literal".to_string(),
        }];

        RenameSpec {
            from: from.to_string(),
            to: to.to_string(),
            scope,
            variants,
            literal: true,
            rename_context: RenameContext::All,
        }
    }

    /// Add explicit caller-provided variant mappings.
    ///
    /// This keeps project/domain naming conventions out of core. Callers can map
    /// any source spelling to any target spelling, such as acronym display names
    /// or language-specific class prefixes, without teaching the rename engine
    /// what those conventions mean.
    pub fn with_explicit_variants(mut self, variants: Vec<(String, String)>) -> Self {
        for (from, to) in variants {
            self.variants.push(CaseVariant {
                from,
                to,
                label: "explicit".to_string(),
            });
        }
        deduplicate_variants(&mut self.variants);
        self
    }
}

pub(super) fn deduplicate_variants(variants: &mut Vec<CaseVariant>) {
    // Sort by from length descending first so longer matches take priority.
    variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));
    let mut seen = std::collections::HashSet::new();
    variants.retain(|variant| seen.insert(variant.from.clone()));
}

/// A single reference found in the codebase.
#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    /// File path relative to root.
    pub file: String,
    /// Line number (1-indexed).
    pub line: usize,
    /// Column number (1-indexed).
    pub column: usize,
    /// The matched text.
    pub matched: String,
    /// What it would be replaced with.
    pub replacement: String,
    /// The case variant label.
    pub variant: String,
    /// The full line content for context.
    pub context: String,
}

/// An edit to apply to a file's content.
#[derive(Debug, Clone, Serialize)]
pub struct FileEdit {
    /// File path relative to root.
    pub file: String,
    /// Number of replacements in this file.
    pub replacements: usize,
    /// New content after all replacements.
    #[serde(skip)]
    pub new_content: String,
}

/// A file or directory rename.
#[derive(Debug, Clone, Serialize)]
pub struct FileRename {
    /// Original path relative to root.
    pub from: String,
    /// New path relative to root.
    pub to: String,
}

/// A warning about a potential collision or issue.
#[derive(Debug, Clone, Serialize)]
pub struct RenameWarning {
    /// Warning category.
    pub kind: String,
    /// File path relative to root.
    pub file: String,
    /// Line number (if applicable).
    pub line: Option<usize>,
    /// Human-readable description.
    pub message: String,
}

/// The full result of a rename operation.
#[derive(Debug, Clone, Serialize)]
pub struct RenameResult {
    /// Case variants that were searched.
    pub variants: Vec<CaseVariant>,
    /// All references found.
    pub references: Vec<Reference>,
    /// File content edits to apply.
    pub edits: Vec<FileEdit>,
    /// File/directory renames to apply.
    pub file_renames: Vec<FileRename>,
    /// Warnings about potential collisions or issues.
    pub warnings: Vec<RenameWarning>,
    /// Total reference count.
    pub total_references: usize,
    /// Total files affected.
    pub total_files: usize,
    /// Whether changes were written to disk.
    pub applied: bool,
}
