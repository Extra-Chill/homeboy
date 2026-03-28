//! Language grammar primitive — structure-aware regex matching.
//!
//! A generic engine for extracting structural information from source files.
//! The engine itself has zero language knowledge. Languages are defined by
//! grammar files shipped in extensions.
//!
//! # Architecture
//!
//! ```text
//! utils/grammar.rs   (this file)
//!   ├── Grammar       — loaded from extension TOML, defines patterns for a language
//!   ├── StructuralParser — brace-depth, string/comment-aware iteration
//!   └── Extractor     — applies Grammar patterns via StructuralParser
//!
//! Extension grammar.toml → Grammar → Extractor → Vec<Symbol>
//! ```
//!
//! # Design Principles
//!
//! - **Zero built-in language knowledge** — all patterns come from grammars
//! - **Structure-aware** — tracks brace depth, skips strings and comments
//! - **Composable** — features query for concepts ("give me methods") not languages
//! - **Same model as `utils/baseline.rs`** — dumb primitive, smart consumers

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::engine::local_files;
use crate::error::{Error, Result};

// ============================================================================
// Grammar definition (loaded from extension TOML/JSON)
// ============================================================================

/// A language grammar defining patterns for structural concepts.
///
/// Grammars are loaded from extension files (e.g., `grammar.toml`).
/// Each grammar defines how to recognize methods, classes, imports, etc.
/// in a specific language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grammar {
    /// Language metadata.
    pub language: LanguageMeta,

    /// Comment syntax for this language.
    pub comments: CommentSyntax,

    /// String literal syntax for this language.
    pub strings: StringSyntax,

    /// Block delimiter (usually braces, but could be indentation).
    #[serde(default)]
    pub blocks: BlockSyntax,

    /// Named patterns for structural concepts.
    pub patterns: HashMap<String, ConceptPattern>,

    /// Contract extraction patterns — for analyzing function internals.
    /// Optional: extensions that don't provide this get no contract extraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractGrammar>,
}

/// Grammar section for function contract extraction.
///
/// Defines patterns that identify control flow, side effects, and return
/// paths within function bodies. All patterns are applied only inside
/// function body ranges (between the function's opening and closing braces).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractGrammar {
    /// Patterns that identify side effects. Keys are effect kind names
    /// (e.g., "file_read", "file_write", "process_spawn"), values are
    /// regex patterns to match against lines inside function bodies.
    #[serde(default)]
    pub effects: HashMap<String, Vec<String>>,

    /// Patterns that identify early return / guard clause lines.
    /// Each pattern should match a line that contains a conditional return.
    #[serde(default)]
    pub guard_patterns: Vec<String>,

    /// Patterns that identify return expressions with their variant.
    /// Keys are variant names (e.g., "ok", "err", "some", "none", "true", "false").
    /// Values are regex patterns that match return statements of that variant.
    #[serde(default)]
    pub return_patterns: HashMap<String, Vec<String>>,

    /// Patterns that identify error propagation (e.g., `?` in Rust, `throw` in JS).
    #[serde(default)]
    pub error_propagation: Vec<String>,

    /// Return type shape detection patterns. Keys are shape names
    /// (e.g., "result", "option", "bool"), values are regex patterns
    /// to match against the function signature's return type.
    #[serde(default)]
    pub return_shapes: HashMap<String, Vec<String>>,

    /// Patterns for detecting panic/abort/unreachable paths.
    #[serde(default)]
    pub panic_patterns: Vec<String>,

    /// The separator between the parameter list and return type in function declarations.
    /// Rust: `"->"`, PHP: `":"`, TypeScript: `":"`.
    /// Defaults to `"->"` for backward compatibility.
    #[serde(default = "default_return_type_separator")]
    pub return_type_separator: String,

    /// Parameter format in function declarations.
    /// `"name_colon_type"` — Rust/Go: `name: Type` (default)
    /// `"type_dollar_name"` — PHP: `Type $name` or `$name`
    #[serde(default = "default_param_format")]
    pub param_format: String,

    /// Test code templates keyed by template name (e.g., "result_ok", "option_none").
    /// Templates contain variables like `{fn_name}`, `{param_names}`, `{test_name}`,
    /// `{condition}`, etc. that are replaced by the test plan renderer.
    ///
    /// This is what makes test output language-specific without any language code in core.
    #[serde(default)]
    pub test_templates: HashMap<String, String>,

    /// Type-to-default-value mappings for test input construction.
    /// Keys are regex patterns matched against parameter types.
    /// Values are code expressions that produce a valid zero/default value.
    ///
    /// Example (Rust): `"&str" → "\"\"", "&Path" → "Path::new(\"\")"`.
    ///
    /// Patterns are tried in order; first match wins. The fallback for
    /// unmatched types is `Default::default()` (Rust) or language equivalent.
    #[serde(default)]
    pub type_defaults: Vec<TypeDefault>,

    /// Behavioral constructors for condition-specific test inputs.
    ///
    /// Maps a `(semantic_hint, type_pattern)` pair to a code expression.
    /// Core analyzes branch conditions to produce semantic hints like
    /// `"empty"`, `"non_empty"`, `"nonexistent_path"`, `"none"`, etc.
    /// The grammar then provides the language-specific code that
    /// produces a value satisfying that hint for the matched type.
    ///
    /// This keeps core language-agnostic: core recognizes *what* the
    /// condition needs, the grammar provides *how* to express it.
    #[serde(default)]
    pub type_constructors: Vec<TypeConstructor>,

    /// Assertion templates for behavioral test assertions.
    ///
    /// Maps an assertion key (e.g., `"result_ok_value"`, `"result_err_value"`,
    /// `"option_none"`, `"bool_true"`) to a template string containing
    /// variables like `{condition}`, `{expected_value}`.
    ///
    /// Core selects the assertion key based on the branch return; the grammar
    /// provides the language-specific assertion code. This avoids hardcoding
    /// `unwrap()` or `is_ok()` in core.
    #[serde(default)]
    pub assertion_templates: HashMap<String, String>,

    /// Fallback default expression when no type_default or type_constructor
    /// matches. Language-specific (e.g., `"Default::default()"` for Rust,
    /// `"null"` for PHP).
    #[serde(default = "default_fallback_default")]
    pub fallback_default: String,

    /// Regex pattern for extracting struct/class field declarations.
    /// Must have two capture groups for field name and field type.
    /// Applied to each line inside a struct/class body.
    ///
    /// Which capture group is name vs type is controlled by `field_name_group`
    /// and `field_type_group` (default: group 1 = name, group 2 = type).
    ///
    /// Rust: `"^\s*(?:pub\s+)?(\w+)\s*:\s*(.+?),?\s*$"` — name:1, type:2
    /// PHP:  `"^\s*(?:public|protected|private)\s+(?:readonly\s+)?(\??\w+)\s+\$(\w+)"` — type:1, name:2
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_pattern: Option<String>,

    /// Which capture group in `field_pattern` contains the field name. Default: 1.
    #[serde(default = "default_group_1")]
    pub field_name_group: usize,

    /// Which capture group in `field_pattern` contains the field type. Default: 2.
    #[serde(default = "default_group_2")]
    pub field_type_group: usize,

    /// Regex pattern that identifies public visibility on a field line.
    /// Used to set `FieldDef.is_public`.
    ///
    /// Rust: `"^\s*pub\b"`, PHP: `"^\s*public\b"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_visibility_pattern: Option<String>,

    /// Template for asserting a single struct field in a generated test.
    /// Variables: `{field_name}`, `{expected_value}`, `{indent}`.
    ///
    /// Rust:  `"{indent}assert_eq!(inner.{field_name}, {expected_value});"`
    /// PHP:   `"{indent}$this->assertSame( {expected_value}, $result->{field_name} );"`
    ///
    /// If not set, field-level assertions are not generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_assertion_template: Option<String>,
}

fn default_fallback_default() -> String {
    "Default::default()".to_string()
}

fn default_group_1() -> usize {
    1
}

fn default_group_2() -> usize {
    2
}

fn default_return_type_separator() -> String {
    "->".to_string()
}

fn default_param_format() -> String {
    "name_colon_type".to_string()
}

/// A single type-to-default-value mapping for test input construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDefault {
    /// Regex pattern to match against the parameter type string.
    pub pattern: String,
    /// Code expression that produces a valid default value for matched types.
    pub value: String,
    /// Optional extra `use` imports required by this default value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
}

/// A behavioral constructor mapping a semantic hint + type pattern to a code expression.
///
/// Core produces semantic hints from branch conditions (e.g., `"empty"` from
/// `items.is_empty()`). The grammar maps each `(hint, type_pattern)` pair to
/// the language-specific expression that produces a value satisfying that hint.
///
/// The `hint` field is matched exactly. The `pattern` field is a regex matched
/// against the parameter type. First match wins (entries are tried in order).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeConstructor {
    /// Semantic hint from behavioral inference (e.g., "empty", "non_empty",
    /// "nonexistent_path", "none", "some_default", "true", "false", "zero",
    /// "positive", "contains").
    pub hint: String,
    /// Regex pattern to match against the parameter type string.
    pub pattern: String,
    /// Code expression that produces a value satisfying the hint for this type.
    /// May contain `{param_name}` which is replaced with the actual param name.
    pub value: String,
    /// Optional override for the call argument (e.g., `"{param_name}.path()"` for tempdir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_arg: Option<String>,
    /// Optional extra `use` imports required by this value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
}

/// Language identification metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageMeta {
    /// Language identifier (e.g., "php", "rust", "javascript").
    pub id: String,

    /// File extensions this grammar applies to.
    pub extensions: Vec<String>,
}

/// How comments work in this language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentSyntax {
    /// Single-line comment prefixes (e.g., ["//", "#"]).
    #[serde(default)]
    pub line: Vec<String>,

    /// Multi-line comment delimiters (e.g., [["/*", "*/"]]).
    #[serde(default)]
    pub block: Vec<(String, String)>,

    /// Doc comment prefixes (e.g., ["///", "//!"]).
    #[serde(default)]
    pub doc: Vec<String>,
}

/// How string literals work in this language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringSyntax {
    /// Quote characters (e.g., ["\"", "'", "`"]).
    #[serde(default = "default_quotes")]
    pub quotes: Vec<String>,

    /// Escape character (usually backslash).
    #[serde(default = "default_escape_string")]
    pub escape: String,

    /// Multi-line string delimiters (e.g., Python's triple-quote).
    #[serde(default)]
    pub multiline: Vec<(String, String)>,
}

fn default_quotes() -> Vec<String> {
    vec!["\"".to_string(), "'".to_string()]
}

fn default_escape_string() -> String {
    "\\".to_string()
}

/// Block (scope) delimiters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSyntax {
    /// Opening delimiter (default: "{").
    #[serde(default = "default_open")]
    pub open: String,

    /// Closing delimiter (default: "}").
    #[serde(default = "default_close")]
    pub close: String,
}

impl Default for BlockSyntax {
    fn default() -> Self {
        Self {
            open: "{".to_string(),
            close: "}".to_string(),
        }
    }
}

fn default_open() -> String {
    "{".to_string()
}

fn default_close() -> String {
    "}".to_string()
}

/// A pattern for a structural concept (method, class, import, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptPattern {
    /// Regex pattern to match this concept.
    pub regex: String,

    /// Named capture group mapping.
    /// Maps semantic names to capture group indices.
    /// e.g., {"name": 1, "visibility": 2, "params": 3}
    #[serde(default)]
    pub captures: HashMap<String, usize>,

    /// Context constraint: where this pattern is valid.
    /// - "any" (default) — match anywhere
    /// - "top_level" — only at brace depth 0
    /// - "in_block" — only inside a block (depth > 0)
    /// - "line" — match per-line (default for most patterns)
    #[serde(default = "default_context")]
    pub context: String,

    /// Whether to skip matches inside comments.
    #[serde(default = "default_true")]
    pub skip_comments: bool,

    /// Whether to skip matches inside string literals.
    #[serde(default = "default_true")]
    pub skip_strings: bool,

    /// Filter: only include matches where this capture group is non-empty.
    #[serde(default)]
    pub require_capture: Option<String>,
}

fn default_context() -> String {
    "any".to_string()
}

fn default_true() -> bool {
    true
}

// ============================================================================
// Structural parser — context-aware iteration over source text
// ============================================================================

/// Region classification for a line or span of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Normal code.
    Code,
    /// Inside a single-line comment.
    LineComment,
    /// Inside a block comment.
    BlockComment,
    /// Inside a string literal.
    StringLiteral,
}

/// Tracks structural context while parsing source text.
#[derive(Debug, Clone)]
pub struct StructuralContext {
    /// Current brace nesting depth.
    pub depth: i32,

    /// Current region (code, comment, string).
    pub region: Region,

    /// Stack of block contexts: (kind_label, depth_when_entered).
    /// Features can push/pop to track impl blocks, test modules, etc.
    pub block_stack: Vec<(String, i32)>,
}

impl StructuralContext {
    pub fn new() -> Self {
        Self {
            depth: 0,
            region: Region::Code,
            block_stack: Vec::new(),
        }
    }

    /// Whether we're inside a block with the given label.
    #[cfg(test)]
    pub(crate) fn is_inside(&self, label: &str) -> bool {
        self.block_stack.iter().any(|(l, _)| l == label)
    }

    /// The label of the innermost block, if any.
    #[cfg(test)]
    pub(crate) fn current_block_label(&self) -> Option<&str> {
        self.block_stack.last().map(|(l, _)| l.as_str())
    }

    /// Push a labeled block at the current depth.
    #[cfg(test)]
    pub(crate) fn push_block(&mut self, label: String) {
        self.block_stack.push((label, self.depth));
    }

    /// Pop blocks that have been exited (depth dropped below entry depth).
    pub(crate) fn pop_exited_blocks(&mut self) {
        while let Some((_, entry_depth)) = self.block_stack.last() {
            if self.depth <= *entry_depth {
                self.block_stack.pop();
            } else {
                break;
            }
        }
    }
}

impl Default for StructuralContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A line of source with its structural context.
#[derive(Debug, Clone)]
pub struct ContextualLine<'a> {
    /// The line content.
    pub text: &'a str,

    /// 1-indexed line number.
    pub line_num: usize,

    /// Brace depth at the start of this line.
    pub depth: i32,

    /// What region this line is in.
    pub region: Region,
}

/// Check if a line opens a Rust raw string (`r#"`, `r##"`, etc.) that doesn't
/// close on the same line. Returns the closing delimiter pattern if found.
///
/// This is the shared implementation used by both the grammar walker (to mark
/// lines as `StringLiteral` during extraction) and the orphaned test fixer
/// (to skip function declarations inside test fixture strings).
pub fn find_unclosed_raw_string_on_line(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    while pos < len {
        // Skip regular strings — don't confuse `"..."` with raw string opening.
        if bytes[pos] == b'"' && (pos == 0 || bytes[pos - 1] != b'r') {
            pos += 1;
            while pos < len {
                if bytes[pos] == b'"' && bytes[pos - 1] != b'\\' {
                    pos += 1;
                    break;
                }
                pos += 1;
            }
            continue;
        }

        // Look for r followed by one or more # then "
        if bytes[pos] == b'r' && pos + 1 < len {
            let hash_start = pos + 1;
            let mut hash_end = hash_start;
            while hash_end < len && bytes[hash_end] == b'#' {
                hash_end += 1;
            }
            let hash_count = hash_end - hash_start;

            if hash_count > 0 && hash_end < len && bytes[hash_end] == b'"' {
                let close_pattern = format!("\"{}", "#".repeat(hash_count));

                // Check if the closing pattern appears later on the SAME line
                let after_open = &line[hash_end + 1..];
                if !after_open.contains(&close_pattern) {
                    return Some(close_pattern);
                }

                // Closed on same line — skip past the close and continue
                if let Some(close_pos) = after_open.find(&close_pattern) {
                    pos = hash_end + 1 + close_pos + close_pattern.len();
                    continue;
                }
            }
        }

        pos += 1;
    }

    None
}

/// Iterate lines with structural context, tracking brace depth and regions.
///
/// This is the core primitive — it walks the file line-by-line, tracking
/// brace depth and whether we're inside comments or strings. Consumers
/// can then filter lines by depth, region, etc.
pub(crate) fn walk_lines<'a>(content: &'a str, grammar: &Grammar) -> Vec<ContextualLine<'a>> {
    let mut ctx = StructuralContext::new();
    let mut result = Vec::new();
    let mut in_block_comment = false;
    let mut block_comment_end = String::new();
    let mut in_raw_string = false;
    let mut raw_string_close = String::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let depth_at_start = ctx.depth;

        // Determine region for this line
        let region = if in_raw_string {
            // Inside a multi-line raw string — check if closing delimiter appears
            if line.contains(&raw_string_close) {
                in_raw_string = false;
            }
            Region::StringLiteral
        } else if in_block_comment {
            // Check if block comment ends on this line
            if let Some(pos) = trimmed.find(block_comment_end.as_str()) {
                // Comment ends partway through this line
                in_block_comment = false;
                let after = &trimmed[pos + block_comment_end.len()..].trim();
                if after.is_empty() {
                    Region::BlockComment
                } else {
                    // Mixed line — treat as code (conservative)
                    Region::Code
                }
            } else {
                Region::BlockComment
            }
        } else if is_line_comment(trimmed, &grammar.comments) {
            Region::LineComment
        } else {
            // Check for block comment start
            for (open, close) in &grammar.comments.block {
                if trimmed.starts_with(open.as_str())
                    && (!trimmed.contains(close.as_str()) || trimmed.ends_with(open.as_str()))
                {
                    in_block_comment = true;
                    block_comment_end = close.clone();
                }
            }
            if in_block_comment {
                Region::BlockComment
            } else {
                // Check for multi-line raw string opening (Rust r#"..."#, r##"..."##, etc.)
                if let Some(close) = find_unclosed_raw_string_on_line(line) {
                    in_raw_string = true;
                    raw_string_close = close;
                }
                // The opening line itself is Code (it has real code on it);
                // subsequent lines inside the string are StringLiteral.
                Region::Code
            }
        };

        // Track brace depth for code lines
        if region == Region::Code {
            update_depth(line, &grammar.blocks, &grammar.strings, &mut ctx);
        }

        result.push(ContextualLine {
            text: line,
            line_num: i + 1,
            depth: depth_at_start,
            region,
        });

        // Pop exited blocks
        ctx.pop_exited_blocks();
    }

    result
}

/// Check if a trimmed line is a single-line comment.
fn is_line_comment(trimmed: &str, comments: &CommentSyntax) -> bool {
    for prefix in &comments.line {
        if trimmed.starts_with(prefix.as_str()) {
            return true;
        }
    }
    for prefix in &comments.doc {
        if trimmed.starts_with(prefix.as_str()) {
            return true;
        }
    }
    false
}

/// Update brace depth for a line, skipping strings.
fn update_depth(
    line: &str,
    blocks: &BlockSyntax,
    strings: &StringSyntax,
    ctx: &mut StructuralContext,
) {
    let mut in_string: Option<char> = None;
    let mut prev_char = '\0';

    for ch in line.chars() {
        if let Some(quote) = in_string {
            if ch == quote && prev_char != strings.escape.chars().next().unwrap_or('\\') {
                in_string = None;
            }
        } else if strings.quotes.iter().any(|q| q.starts_with(ch)) {
            in_string = Some(ch);
        } else if blocks.open.starts_with(ch) {
            ctx.depth += 1;
        } else if blocks.close.starts_with(ch) {
            ctx.depth -= 1;
        }
        prev_char = ch;
    }
}

// ============================================================================
// Extraction — apply grammar patterns to get symbols
// ============================================================================

/// A symbol extracted from source code.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    /// What kind of concept this is (matches the pattern key in the grammar).
    /// e.g., "method", "class", "import", "namespace"
    pub concept: String,

    /// Named captures from the pattern match.
    /// e.g., {"name": "foo", "visibility": "pub", "params": "&self, key: &str"}
    pub captures: HashMap<String, String>,

    /// 1-indexed line number where the symbol was found.
    pub line: usize,

    /// Brace depth at the match location.
    pub depth: i32,

    /// The full matched text.
    pub matched_text: String,
}

impl Symbol {
    /// Get a named capture value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.captures.get(key).map(|s| s.as_str())
    }

    /// Get the "name" capture (most symbols have one).
    pub fn name(&self) -> Option<&str> {
        self.get("name")
    }

    /// Get the "visibility" capture.
    pub fn visibility(&self) -> Option<&str> {
        self.get("visibility")
    }
}

/// Extract all symbols from source content using a grammar.
pub fn extract(content: &str, grammar: &Grammar) -> Vec<Symbol> {
    let lines = walk_lines(content, grammar);
    let mut symbols = Vec::new();

    for (concept_name, pattern) in &grammar.patterns {
        let re = match Regex::new(&pattern.regex) {
            Ok(r) => r,
            Err(_) => continue, // Skip invalid patterns
        };

        for ctx_line in &lines {
            // Always skip lines inside string literals — function declarations,
            // imports, etc. inside raw strings are not real code.
            if ctx_line.region == Region::StringLiteral {
                continue;
            }

            // Skip based on region
            if pattern.skip_comments
                && (ctx_line.region == Region::LineComment
                    || ctx_line.region == Region::BlockComment)
            {
                continue;
            }

            // Skip based on context constraint
            match pattern.context.as_str() {
                "top_level" => {
                    if ctx_line.depth != 0 {
                        continue;
                    }
                }
                "in_block" => {
                    if ctx_line.depth == 0 {
                        continue;
                    }
                }
                _ => {} // "any" or "line" — no constraint
            }

            // Try to match
            if let Some(caps) = re.captures(ctx_line.text) {
                let mut capture_map = HashMap::new();

                for (name, &index) in &pattern.captures {
                    if let Some(m) = caps.get(index) {
                        capture_map.insert(name.clone(), m.as_str().to_string());
                    }
                }

                // Check require_capture filter
                if let Some(ref required) = pattern.require_capture {
                    if capture_map.get(required).is_none_or(|v| v.is_empty()) {
                        continue;
                    }
                }

                symbols.push(Symbol {
                    concept: concept_name.clone(),
                    captures: capture_map,
                    line: ctx_line.line_num,
                    depth: ctx_line.depth,
                    matched_text: caps[0].to_string(),
                });
            }
        }
    }

    // Sort by line number for stable output
    symbols.sort_by_key(|s| s.line);
    symbols
}

/// Extract symbols of a specific concept only.
#[cfg(test)]
pub(crate) fn extract_concept(content: &str, grammar: &Grammar, concept: &str) -> Vec<Symbol> {
    extract(content, grammar)
        .into_iter()
        .filter(|s| s.concept == concept)
        .collect()
}

// ============================================================================
// Grammar loading
// ============================================================================

/// Load a grammar from a TOML file.
pub fn load_grammar(path: &Path) -> Result<Grammar> {
    let content = local_files::read_file(path, "read grammar file")?;
    toml::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse grammar {}: {}", path.display(), e),
            Some("grammar.load".to_string()),
        )
    })
}

/// Load a grammar from a JSON file.
pub fn load_grammar_json(path: &Path) -> Result<Grammar> {
    let content = local_files::read_file(path, "read grammar file")?;
    serde_json::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse grammar {}: {}", path.display(), e),
            Some("grammar.load".to_string()),
        )
    })
}

// ============================================================================
// Convenience helpers for feature consumers
// ============================================================================

/// Get all method/function names from extracted symbols.
#[cfg(test)]
pub(crate) fn method_names(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| {
            s.concept == "method" || s.concept == "function" || s.concept == "free_function"
        })
        .filter_map(|s| s.name().map(|n| n.to_string()))
        .collect()
}

/// Get all class/struct/trait names from extracted symbols.
#[cfg(test)]
pub(crate) fn type_names(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| {
            s.concept == "class"
                || s.concept == "struct"
                || s.concept == "trait"
                || s.concept == "enum"
                || s.concept == "interface"
                || s.concept == "type"
        })
        .filter_map(|s| s.name().map(|n| n.to_string()))
        .collect()
}

/// Get all import paths from extracted symbols.
#[cfg(test)]
pub(crate) fn import_paths(symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| s.concept == "import" || s.concept == "use")
        .filter_map(|s| s.get("path").map(|p| p.to_string()))
        .collect()
}

/// Get the namespace from extracted symbols.
pub fn namespace(symbols: &[Symbol]) -> Option<String> {
    symbols
        .iter()
        .find(|s| s.concept == "namespace" || s.concept == "module")
        .and_then(|s| s.name().map(|n| n.to_string()))
}

/// Filter symbols to only public API (visibility contains "pub" or "public").
#[cfg(test)]
pub(crate) fn public_symbols(symbols: &[Symbol]) -> Vec<&Symbol> {
    symbols
        .iter()
        .filter(|s| {
            s.visibility()
                .is_none_or(|v| v.contains("pub") || v == "public")
        })
        .collect()
}

// ============================================================================
// Block body extraction
// ============================================================================

/// Extract the body of a block starting from a given line.
///
/// Finds the opening brace on or after `start_line` (0-indexed into lines),
/// then returns all lines until the matching closing brace.
#[cfg(test)]
pub(crate) fn extract_block_body<'a>(
    lines: &[ContextualLine<'a>],
    start_line_idx: usize,
    grammar: &Grammar,
) -> Option<Vec<&'a str>> {
    let open = grammar.blocks.open.chars().next().unwrap_or('{');
    let close = grammar.blocks.close.chars().next().unwrap_or('}');

    // Find the opening brace
    let mut idx = start_line_idx;
    let mut found_open = false;
    let mut depth: i32 = 0;
    let mut body_lines = Vec::new();

    while idx < lines.len() {
        let line = lines[idx].text;
        for ch in line.chars() {
            if ch == open {
                depth += 1;
                found_open = true;
            } else if ch == close {
                depth -= 1;
                if found_open && depth == 0 {
                    body_lines.push(line);
                    return Some(body_lines);
                }
            }
        }
        if found_open {
            body_lines.push(line);
        }
        idx += 1;
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
            },
            comments: CommentSyntax {
                line: vec!["//".to_string()],
                block: vec![("/*".to_string(), "*/".to_string())],
                doc: vec!["///".to_string(), "//!".to_string()],
            },
            strings: StringSyntax {
                quotes: vec!["\"".to_string()],
                escape: "\\".to_string(),
                multiline: vec![],
            },
            blocks: BlockSyntax::default(),
            contract: None,
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "function".to_string(),
                    ConceptPattern {
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c.insert("params".to_string(), 2);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "struct".to_string(),
                    ConceptPattern {
                        regex: r"(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+(\w+)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "import".to_string(),
                    ConceptPattern {
                        regex: r"use\s+([\w:]+(?:::\{[^}]+\})?);".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("path".to_string(), 1);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p
            },
        }
    }

    fn php_grammar() -> Grammar {
        Grammar {
            language: LanguageMeta {
                id: "php".to_string(),
                extensions: vec!["php".to_string()],
            },
            comments: CommentSyntax {
                line: vec!["//".to_string(), "#".to_string()],
                block: vec![("/*".to_string(), "*/".to_string())],
                doc: vec![],
            },
            strings: StringSyntax {
                quotes: vec!["\"".to_string(), "'".to_string()],
                escape: "\\".to_string(),
                multiline: vec![],
            },
            blocks: BlockSyntax::default(),
            contract: None,
            patterns: {
                let mut p = HashMap::new();
                p.insert(
                    "method".to_string(),
                    ConceptPattern {
                        regex: r"(?:(?:public|protected|private|static|abstract|final)\s+)*function\s+(\w+)\s*\(([^)]*)\)".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c.insert("params".to_string(), 2);
                            c
                        },
                        context: "any".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "class".to_string(),
                    ConceptPattern {
                        regex: r"(?:abstract\s+)?(?:final\s+)?(class|trait|interface)\s+(\w+)"
                            .to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("kind".to_string(), 1);
                            c.insert("name".to_string(), 2);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p.insert(
                    "namespace".to_string(),
                    ConceptPattern {
                        regex: r"namespace\s+([\w\\]+);".to_string(),
                        captures: {
                            let mut c = HashMap::new();
                            c.insert("name".to_string(), 1);
                            c
                        },
                        context: "top_level".to_string(),
                        skip_comments: true,
                        skip_strings: true,
                        require_capture: None,
                    },
                );
                p
            },
        }
    }

    // ---- Structural parser tests ----

    #[test]
    fn walk_lines_tracks_depth() {
        let content = "fn main() {\n    let x = 1;\n    if true {\n        foo();\n    }\n}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].depth, 0); // fn main() {
        assert_eq!(lines[1].depth, 1); // let x = 1;
        assert_eq!(lines[2].depth, 1); // if true {
        assert_eq!(lines[3].depth, 2); // foo();
        assert_eq!(lines[4].depth, 2); // }
        assert_eq!(lines[5].depth, 1); // }
    }

    #[test]
    fn walk_lines_detects_line_comments() {
        let content = "let x = 1;\n// this is a comment\nlet y = 2;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code);
        assert_eq!(lines[1].region, Region::LineComment);
        assert_eq!(lines[2].region, Region::Code);
    }

    #[test]
    fn walk_lines_detects_block_comments() {
        let content = "let x = 1;\n/* multi\nline\ncomment */\nlet y = 2;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code);
        assert_eq!(lines[1].region, Region::BlockComment);
        assert_eq!(lines[2].region, Region::BlockComment);
        assert_eq!(lines[3].region, Region::BlockComment);
        assert_eq!(lines[4].region, Region::Code);
    }

    #[test]
    fn depth_skips_braces_in_strings() {
        let content = "let x = \"{ not a block }\";\nlet y = 1;\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        // Braces inside string should NOT change depth
        assert_eq!(lines[0].depth, 0);
        assert_eq!(lines[1].depth, 0);
    }

    #[test]
    fn php_hash_comments() {
        let content = "<?php\n# this is a comment\n$x = 1;\n";
        let grammar = php_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[1].region, Region::LineComment);
        assert_eq!(lines[2].region, Region::Code);
    }

    // ---- Extraction tests ----

    #[test]
    fn extract_rust_functions() {
        let content = "pub fn parse_config(path: &Path) -> Config {\n    todo!()\n}\n\nfn internal() {}\n\npub(crate) fn helper() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);

        let fns: Vec<_> = symbols.iter().filter(|s| s.concept == "function").collect();
        assert_eq!(fns.len(), 3);
        assert_eq!(fns[0].name(), Some("parse_config"));
        assert_eq!(fns[1].name(), Some("internal"));
        assert_eq!(fns[2].name(), Some("helper"));
    }

    #[test]
    fn extract_rust_structs() {
        let content = "pub struct Config {\n    data: String,\n}\n\nenum State {\n    Running,\n    Stopped,\n}\n";
        let grammar = rust_grammar();
        let symbols = extract_concept(content, &grammar, "struct");

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name(), Some("Config"));
        assert_eq!(symbols[1].name(), Some("State"));
    }

    #[test]
    fn extract_rust_imports() {
        let content = "use std::path::Path;\nuse crate::error::Result;\n\nfn foo() {}\n";
        let grammar = rust_grammar();
        let paths = import_paths(&extract(content, &grammar));

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "std::path::Path");
        assert_eq!(paths[1], "crate::error::Result");
    }

    #[test]
    fn extract_php_methods() {
        let content = "<?php\nclass Foo {\n    public function bar() {}\n    protected function baz($x) {}\n    private function internal() {}\n}\n";
        let grammar = php_grammar();
        let methods = extract_concept(content, &grammar, "method");

        assert_eq!(methods.len(), 3);
        assert_eq!(methods[0].name(), Some("bar"));
        assert_eq!(methods[1].name(), Some("baz"));
        assert_eq!(methods[2].name(), Some("internal"));
    }

    #[test]
    fn extract_php_class() {
        let content =
            "<?php\nnamespace App\\Models;\n\nclass User {\n    public function save() {}\n}\n";
        let grammar = php_grammar();
        let symbols = extract(content, &grammar);

        let ns = namespace(&symbols);
        assert_eq!(ns, Some("App\\Models".to_string()));

        let types = type_names(&symbols);
        assert_eq!(types, vec!["User"]);
    }

    #[test]
    fn skip_comments_in_extraction() {
        let content = "// pub fn commented_out() {}\npub fn real_fn() {}\n";
        let grammar = rust_grammar();
        let symbols = extract_concept(content, &grammar, "function");

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name(), Some("real_fn"));
    }

    #[test]
    fn top_level_context_filter() {
        let content = "pub struct Outer {\n    inner: Inner,\n}\n\nimpl Outer {\n    pub struct NotTopLevel {}\n}\n";
        let grammar = rust_grammar();
        // struct pattern has context: "top_level"
        let symbols = extract_concept(content, &grammar, "struct");

        // Should only find Outer (depth 0), not NotTopLevel (depth > 0)
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name(), Some("Outer"));
    }

    #[test]
    fn method_names_helper() {
        let content = "pub fn alpha() {}\nfn beta() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);
        let names = method_names(&symbols);
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn extract_block_body_basic() {
        let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);
        let body = extract_block_body(&lines, 0, &grammar);
        assert!(body.is_some());
        let body = body.unwrap();
        assert_eq!(body.len(), 4); // All 4 lines (fn { ... })
    }

    #[test]
    fn grammar_roundtrip_toml() {
        let grammar = rust_grammar();
        let toml_str = toml::to_string_pretty(&grammar).unwrap();
        let parsed: Grammar = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.language.id, "rust");
        assert_eq!(parsed.patterns.len(), 3);
    }

    #[test]
    fn grammar_roundtrip_json() {
        let grammar = rust_grammar();
        let json_str = serde_json::to_string_pretty(&grammar).unwrap();
        let parsed: Grammar = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.language.id, "rust");
        assert_eq!(parsed.patterns.len(), 3);
    }

    #[test]
    fn structural_context_block_tracking() {
        let mut ctx = StructuralContext::new();
        ctx.depth = 1;
        ctx.push_block("impl".to_string());
        assert!(ctx.is_inside("impl"));
        assert_eq!(ctx.current_block_label(), Some("impl"));

        ctx.depth = 0;
        ctx.pop_exited_blocks();
        assert!(!ctx.is_inside("impl"));
        assert_eq!(ctx.current_block_label(), None);
    }

    #[test]
    fn public_symbols_filter() {
        let content = "pub fn visible() {}\nfn hidden() {}\npub(crate) fn semi() {}\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);

        // All three are extracted, but public_symbols includes those without
        // visibility info (no visibility capture group → defaults to included)
        // since our simple grammar doesn't capture visibility
        let pub_syms = public_symbols(&symbols);
        assert_eq!(pub_syms.len(), 3); // All pass because no "visibility" capture
    }

    // ── Raw string region detection tests ─────────────────────────────

    #[test]
    fn walk_lines_detects_raw_string_regions() {
        let content = "fn real() {}\nlet s = r#\"\nfn fake_inside_string() {}\nanother line\n\"#;\nfn also_real() {}\n";
        let grammar = rust_grammar();
        let lines = walk_lines(content, &grammar);

        assert_eq!(lines[0].region, Region::Code, "fn real()");
        assert_eq!(lines[1].region, Region::Code, "opening line has real code");
        assert_eq!(lines[2].region, Region::StringLiteral, "inside raw string");
        assert_eq!(lines[3].region, Region::StringLiteral, "inside raw string");
        assert_eq!(lines[4].region, Region::StringLiteral, "closing delimiter");
        assert_eq!(lines[5].region, Region::Code, "fn also_real()");
    }

    #[test]
    fn extract_skips_functions_inside_raw_strings() {
        // Regression: the grammar extracted fn declarations from inside raw
        // strings used as test fixtures, polluting the fingerprint with fake
        // source methods like "helper", "load", "write" etc.
        let content = "pub fn real_function() -> bool {\n    true\n}\nlet fixture = r#\"\npub fn fake_function() -> bool {\n    false\n}\nfn another_fake() {}\n\"#;\n";
        let grammar = rust_grammar();
        let symbols = extract(content, &grammar);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.concept == "function")
            .filter_map(|s| s.name())
            .collect();

        assert!(
            fn_names.contains(&"real_function"),
            "Should find real_function. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"fake_function"),
            "Should NOT find fake_function inside raw string. Found: {:?}",
            fn_names
        );
        assert!(
            !fn_names.contains(&"another_fake"),
            "Should NOT find another_fake inside raw string. Found: {:?}",
            fn_names
        );
    }
}

// ============================================================================
// Integration tests — load real grammar files from extensions
// ============================================================================

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Load the Rust grammar from the extensions workspace and validate it
    /// against real Rust source code (this file!).
    #[test]
    fn load_and_use_rust_grammar() {
        // Try to find the Rust grammar in the extensions workspace
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/rust/grammar.toml",
        );
        if !grammar_path.exists() {
            // Skip if not in development environment
            eprintln!("Skipping: Rust grammar not found at {:?}", grammar_path);
            return;
        }

        let grammar = load_grammar(grammar_path).expect("Failed to load Rust grammar");
        assert_eq!(grammar.language.id, "rust");
        assert!(grammar.patterns.contains_key("function"));
        assert!(grammar.patterns.contains_key("struct"));
        assert!(grammar.patterns.contains_key("import"));

        // Test against a sample of Rust code
        let sample = r#"
use std::path::Path;
use crate::error::Result;

pub struct Config {
    data: String,
}

impl Config {
    pub fn new() -> Self {
        Self { data: String::new() }
    }

    pub fn load(path: &Path) -> Result<Self> {
        todo!()
    }

    fn private_helper(&self) {}
}

pub fn standalone(x: i32) -> bool {
    x > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
"#;

        let symbols = extract(sample, &grammar);

        // Should find functions
        let fns: Vec<_> = symbols.iter().filter(|s| s.concept == "function").collect();
        assert!(
            fns.len() >= 3,
            "Expected at least 3 functions, got {}: {:?}",
            fns.len(),
            fns.iter().map(|f| f.name()).collect::<Vec<_>>()
        );

        // Should find struct
        let structs: Vec<_> = symbols.iter().filter(|s| s.concept == "struct").collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name(), Some("Config"));

        // Should find imports
        let imports: Vec<_> = symbols.iter().filter(|s| s.concept == "import").collect();
        assert_eq!(imports.len(), 2);

        // Should find impl block
        let impls: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "impl_block")
            .collect();
        assert_eq!(impls.len(), 1);

        // Should find cfg(test)
        let cfg_tests: Vec<_> = symbols.iter().filter(|s| s.concept == "cfg_test").collect();
        assert_eq!(cfg_tests.len(), 1);

        // Should find test attribute
        let test_attrs: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "test_attribute")
            .collect();
        assert_eq!(test_attrs.len(), 1);
    }

    /// Load the PHP grammar and validate it against sample PHP code.
    #[test]
    fn load_and_use_php_grammar() {
        let grammar_path = std::path::Path::new(
            "/var/lib/datamachine/workspace/homeboy-modules/wordpress/grammar.toml",
        );
        if !grammar_path.exists() {
            eprintln!("Skipping: PHP grammar not found at {:?}", grammar_path);
            return;
        }

        let grammar = load_grammar(grammar_path).expect("Failed to load PHP grammar");
        assert_eq!(grammar.language.id, "php");
        assert!(grammar.patterns.contains_key("method"));
        assert!(grammar.patterns.contains_key("class"));
        assert!(grammar.patterns.contains_key("namespace"));

        let sample = r#"<?php
namespace DataMachine\Abilities;

use WP_UnitTestCase;
use DataMachine\Core\Pipeline;

class PipelineAbilities extends BaseAbilities {
    public function register() {
        add_action('init', [$this, 'setup']);
    }

    public function executeCreate($config) {
        return new Pipeline($config);
    }

    protected function validate($input) {
        return true;
    }

    private function internal() {}

    public static function getInstance() {
        return new static();
    }
}
"#;

        let symbols = extract(sample, &grammar);

        // Should find namespace
        let ns = namespace(&symbols);
        assert_eq!(ns, Some("DataMachine\\Abilities".to_string()));

        // Should find class
        let classes: Vec<_> = symbols.iter().filter(|s| s.concept == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), Some("PipelineAbilities"));
        assert_eq!(classes[0].get("extends"), Some("BaseAbilities"));

        // Should find methods
        let methods: Vec<_> = symbols.iter().filter(|s| s.concept == "method").collect();
        assert!(
            methods.len() >= 4,
            "Expected at least 4 methods, got {}",
            methods.len()
        );

        // Should find imports
        let imports: Vec<_> = symbols.iter().filter(|s| s.concept == "import").collect();
        assert_eq!(imports.len(), 2);

        // Should find add_action
        let actions: Vec<_> = symbols
            .iter()
            .filter(|s| s.concept == "add_action")
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name(), Some("init"));
    }
}
