//! Lexical projections of source text for term-matching detectors.
//!
//! A term-matching detector that reads raw lines cannot tell the difference
//! between behavior and prose about behavior. `/// runs on every node` and
//! `run("node")` are the same bytes to a substring search, but only one of them
//! is a boundary leak. #6857 named this for the whole detector family; the
//! `core-agnostic-source` policy reported ~80% false positives because of it.
//!
//! [`SourceMasks`] scans a file once and returns two same-shaped projections of
//! every line, with the excluded regions blanked to spaces so byte offsets, line
//! numbers, and token boundaries all survive:
//!
//! - [`SourceMasks::code`] — comments removed, everything else kept. What the
//!   file *does*.
//! - [`SourceMasks::strings`] — only string-literal spans kept. What the file
//!   *names*. Ecosystem knowledge that leaks into core does so as data — command
//!   names, filenames, error substrings — so a term whose bare word is ordinary
//!   vocabulary (`node`, `playground`) can be scoped to this projection and stop
//!   matching local variables without losing the leaks that matter.
//!
//! This is a lexer, not a parser: it tracks comments, strings, and escapes, and
//! deliberately does not resolve macros, heredocs, or preprocessor conditionals.

use super::conventions::Language;

/// Punctuation preserved in the string projection so that seam patterns
/// (`"car", "go"`) still see the join between two adjacent literals.
const SEAM_PUNCTUATION: [char; 2] = [',', '+'];

/// Comment-stripped and string-only projections of one file, line by line.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceMasks {
    code: Vec<String>,
    strings: Vec<String>,
}

impl SourceMasks {
    /// Scan `content` under `language`'s lexical rules.
    pub(crate) fn new(content: &str, language: Language) -> Self {
        Scanner::new(content, language).run()
    }

    /// Line with comment spans blanked. Index is 0-based.
    pub(crate) fn code(&self, line_index: usize) -> &str {
        self.code.get(line_index).map(String::as_str).unwrap_or("")
    }

    /// Line with everything but string-literal spans blanked. Index is 0-based.
    pub(crate) fn strings(&self, line_index: usize) -> &str {
        self.strings
            .get(line_index)
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// Which lexical region the scanner is currently inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    /// Rust block comments nest; the depth is carried so `/* /* */ */` closes
    /// once rather than leaking the rest of the file into a comment.
    BlockComment(u32),
    /// A quoted string. The delimiter distinguishes `"` / `'` / JS backticks.
    Quoted(char),
    /// A Rust raw string: `r"..."`, `r#"..."#`, `r##"..."##`. No escapes apply,
    /// and it closes only on a quote followed by the opening hash count.
    RawString(usize),
}

struct Scanner<'a> {
    chars: Vec<char>,
    language: Language,
    content: &'a str,
}

impl<'a> Scanner<'a> {
    fn new(content: &'a str, language: Language) -> Self {
        Self {
            chars: content.chars().collect(),
            language,
            content,
        }
    }

    /// True when `#` starts a line comment in this language.
    fn hash_comments(&self) -> bool {
        matches!(self.language, Language::Php | Language::Unknown)
    }

    /// True when backticks delimit template literals in this language.
    fn template_literals(&self) -> bool {
        matches!(self.language, Language::JavaScript | Language::TypeScript)
    }

    /// True when `r"..."` / `r#"..."#` raw strings apply.
    fn raw_strings(&self) -> bool {
        matches!(self.language, Language::Rust)
    }

    fn run(self) -> SourceMasks {
        let mut code_line = String::new();
        let mut string_line = String::new();
        let mut masks = SourceMasks::default();
        let mut state = State::Code;
        let mut index = 0usize;

        while index < self.chars.len() {
            let ch = self.chars[index];

            if ch == '\n' {
                masks.code.push(std::mem::take(&mut code_line));
                masks.strings.push(std::mem::take(&mut string_line));
                // A line comment ends at the newline; every other state spans it.
                if state == State::LineComment {
                    state = State::Code;
                }
                index += 1;
                continue;
            }

            let (next_state, consumed, class) = self.step(state, index);
            for offset in 0..consumed {
                let ch = self.chars[index + offset];
                code_line.push(match class {
                    Class::Comment => blank(ch),
                    _ => ch,
                });
                string_line.push(match class {
                    Class::String => ch,
                    _ if SEAM_PUNCTUATION.contains(&ch) => ch,
                    _ => blank(ch),
                });
            }
            state = next_state;
            index += consumed;
        }

        masks.code.push(code_line);
        masks.strings.push(string_line);
        masks
    }

    /// Classify the run of characters starting at `index`.
    ///
    /// Returns the state to continue in, how many characters were consumed, and
    /// how they should be projected.
    fn step(&self, state: State, index: usize) -> (State, usize, Class) {
        match state {
            State::LineComment => (State::LineComment, 1, Class::Comment),
            State::BlockComment(depth) => self.step_block_comment(depth, index),
            State::Quoted(delimiter) => self.step_quoted(delimiter, index),
            State::RawString(hashes) => self.step_raw_string(hashes, index),
            State::Code => self.step_code(index),
        }
    }

    fn step_block_comment(&self, depth: u32, index: usize) -> (State, usize, Class) {
        if self.matches_at(index, "*/") {
            let next = if depth <= 1 {
                State::Code
            } else {
                State::BlockComment(depth - 1)
            };
            return (next, 2, Class::Comment);
        }
        // Only Rust nests; elsewhere an inner `/*` is just comment text.
        if self.raw_strings() && self.matches_at(index, "/*") {
            return (State::BlockComment(depth + 1), 2, Class::Comment);
        }
        (State::BlockComment(depth), 1, Class::Comment)
    }

    fn step_quoted(&self, delimiter: char, index: usize) -> (State, usize, Class) {
        let ch = self.chars[index];
        if ch == '\\' && index + 1 < self.chars.len() {
            return (State::Quoted(delimiter), 2, Class::String);
        }
        if ch == delimiter {
            return (State::Code, 1, Class::String);
        }
        (State::Quoted(delimiter), 1, Class::String)
    }

    fn step_raw_string(&self, hashes: usize, index: usize) -> (State, usize, Class) {
        if self.chars[index] == '"' && self.hash_run(index + 1) >= hashes {
            return (State::Code, 1 + hashes, Class::String);
        }
        (State::RawString(hashes), 1, Class::String)
    }

    fn step_code(&self, index: usize) -> (State, usize, Class) {
        let ch = self.chars[index];

        if self.matches_at(index, "//") {
            return (State::LineComment, 2, Class::Comment);
        }
        if self.matches_at(index, "/*") {
            return (State::BlockComment(1), 2, Class::Comment);
        }
        if ch == '#' && self.hash_comments() {
            return (State::LineComment, 1, Class::Comment);
        }
        if self.raw_strings() && (ch == 'r' || ch == 'b') {
            if let Some((hashes, consumed)) = self.raw_string_opener(index) {
                return (State::RawString(hashes), consumed, Class::String);
            }
        }
        if ch == '"' {
            return (State::Quoted('"'), 1, Class::String);
        }
        if ch == '`' && self.template_literals() {
            return (State::Quoted('`'), 1, Class::String);
        }
        if ch == '\'' && self.single_quote_opens_a_literal(index) {
            return (State::Quoted('\''), 1, Class::String);
        }
        (State::Code, 1, Class::Code)
    }

    /// Recognize `r"`, `r#"`, `br##"`, … and report the hash count and the
    /// width of the opener.
    fn raw_string_opener(&self, index: usize) -> Option<(usize, usize)> {
        let mut cursor = index;
        if self.chars.get(cursor) == Some(&'b') {
            cursor += 1;
        }
        if self.chars.get(cursor) != Some(&'r') {
            return None;
        }
        cursor += 1;
        let hashes = self.hash_run(cursor);
        cursor += hashes;
        if self.chars.get(cursor) != Some(&'"') {
            return None;
        }
        Some((hashes, cursor + 1 - index))
    }

    fn hash_run(&self, start: usize) -> usize {
        let mut count = 0;
        while self.chars.get(start + count) == Some(&'#') {
            count += 1;
        }
        count
    }

    /// Distinguish a Rust char literal from a lifetime.
    ///
    /// `'a` in `&'a str` and `Foo<'_>` is not a string; treating it as one
    /// would swallow source up to the next apostrophe. A char literal is
    /// `'x'` or an escape like `'\n'`, so require a closing quote within the
    /// short window one can occupy.
    fn single_quote_opens_a_literal(&self, index: usize) -> bool {
        if !self.raw_strings() {
            // PHP and JS use `'` for ordinary strings.
            return true;
        }
        match self.chars.get(index + 1) {
            Some('\\') => true,
            Some(_) => self.chars.get(index + 2) == Some(&'\''),
            None => false,
        }
    }

    fn matches_at(&self, index: usize, needle: &str) -> bool {
        needle
            .chars()
            .enumerate()
            .all(|(offset, expected)| self.chars.get(index + offset) == Some(&expected))
    }
}

/// How a consumed run of characters projects into each mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Code,
    Comment,
    String,
}

/// Blank a character while preserving tabs, so column math and indentation-
/// sensitive rendering survive masking.
fn blank(ch: char) -> char {
    if ch == '\t' {
        '\t'
    } else {
        ' '
    }
}

impl std::fmt::Debug for Scanner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scanner")
            .field("language", &self.language)
            .field("len", &self.content.len())
            .finish()
    }
}

#[cfg(test)]
#[path = "source_text_test.rs"]
mod source_text_test;
