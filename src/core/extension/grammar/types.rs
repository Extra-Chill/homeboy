//! types — extracted from grammar.rs.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use crate::core::defaults::default_true;
use std::path::Path;
use crate::error::{Error, Result};
use WP_UnitTestCase;
use DataMachine\Core\Pipeline;
use super::new;
use super::name;
use super::get;
use super::visibility;
use super::super::*;


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
