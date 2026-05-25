//! Syntactic context filters for rename matches.

use crate::core::error::{Error, Result};

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
