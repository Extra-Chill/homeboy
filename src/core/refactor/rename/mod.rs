//! Rename engine — find and replace terms across a codebase with case awareness.
//!
//! Given a `RenameSpec` (from → to), this extension:
//! 1. Generates all case variants (snake, camel, Pascal, UPPER, plural)
//! 2. Walks the codebase finding word-boundary matches
//! 3. Generates file content edits and file/directory renames
//! 4. Applies changes to disk (or returns a dry-run preview)

mod rename_context;
mod rename_scope;
mod types;

pub use rename_context::*;
pub use rename_scope::*;
pub use types::*;


use crate::engine::codebase_scan::{
    self, find_boundary_matches, find_case_insensitive_matches, find_literal_matches,
    ExtensionFilter, ScanConfig,
};
use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============================================================================
// Types
// ============================================================================

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
    /// - `kebab-case` (e.g., `data-machine-agent`)
    /// - `snake_case` (e.g., `data_machine_agent`)
    /// - `UPPER_SNAKE` (e.g., `DATA_MACHINE_AGENT`)
    /// - `PascalCase` (e.g., `DataMachineAgent`)
    /// - `camelCase` (e.g., `dataMachineAgent`)
    /// - `Display Name` (e.g., `Data Machine Agent`)
    /// - Plus plural forms of each
    ///
    /// This means a single `--from wp-agent --to data-machine-agent` will also
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

        // Deduplicate — remove variants where from matches a previous one.
        // Sort by from length descending first so longer matches take priority.
        variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));
        let mut seen = std::collections::HashSet::new();
        variants.retain(|v| seen.insert(v.from.clone()));

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

// ============================================================================
// Case utilities
// ============================================================================

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

fn pluralize(s: &str) -> String {
    if s.ends_with('s') || s.ends_with('x') || s.ends_with("sh") || s.ends_with("ch") {
        format!("{}es", s)
    } else if s.ends_with('y') && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("ay") {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{}s", s)
    }
}

// ============================================================================
// Word splitting — decompose any naming convention into constituent words
// ============================================================================

/// Split a term into its constituent words, regardless of naming convention.
///
/// Handles:
/// - `kebab-case` → `["kebab", "case"]`
/// - `snake_case` → `["snake", "case"]`
/// - `camelCase` → `["camel", "case"]`
/// - `PascalCase` → `["pascal", "case"]`
/// - `UPPER_SNAKE` → `["upper", "snake"]`
/// - `WPAgent` → `["wp", "agent"]` (consecutive uppercase → separate word)
/// - `XMLParser` → `["xml", "parser"]`
/// - `data-machine-agent` → `["data", "machine", "agent"]`
/// - Mixed: `my_WPAgent-thing` → `["my", "wp", "agent", "thing"]`
///
/// All returned words are lowercase.
fn split_words(term: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = term.chars().collect();
    let len = chars.len();

    for i in 0..len {
        let c = chars[i];

        // Separators: hyphens, underscores, spaces, dots
        if c == '-' || c == '_' || c == ' ' || c == '.' {
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
            continue;
        }

        if c.is_uppercase() && !current.is_empty() {
            let prev = chars[i - 1];
            // Split on camelCase boundary (lowercase/digit → uppercase)
            // or consecutive-uppercase boundary (uppercase → uppercase+lowercase)
            let is_camel_boundary = prev.is_lowercase() || prev.is_ascii_digit();
            let is_acronym_boundary =
                prev.is_uppercase() && i + 1 < len && chars[i + 1].is_lowercase();

            if is_camel_boundary || is_acronym_boundary {
                words.push(current.to_lowercase());
                current.clear();
            }
        }

        current.push(c);
    }

    if !current.is_empty() {
        words.push(current.to_lowercase());
    }

    words
}

// ============================================================================
// Cross-separator join functions
// ============================================================================

/// Join words as kebab-case: `["data", "machine", "agent"]` → `"data-machine-agent"`
fn join_kebab(words: &[String]) -> String {
    words.join("-")
}

/// Join words as snake_case: `["data", "machine", "agent"]` → `"data_machine_agent"`
fn join_snake(words: &[String]) -> String {
    words.join("_")
}

/// Join words as UPPER_SNAKE: `["data", "machine", "agent"]` → `"DATA_MACHINE_AGENT"`
fn join_upper_snake(words: &[String]) -> String {
    words
        .iter()
        .map(|w| w.to_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Join words as PascalCase: `["data", "machine", "agent"]` → `"DataMachineAgent"`
fn join_pascal(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join("")
}

/// Join words as camelCase: `["data", "machine", "agent"]` → `"dataMachineAgent"`
fn join_camel(words: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            parts.push(w.to_lowercase());
        } else {
            parts.push(capitalize(w));
        }
    }
    parts.join("")
}

/// Join words as display name: `["data", "machine", "agent"]` → `"Data Machine Agent"`
fn join_display(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join(" ")
}

// Boundary matching and literal matching are provided by crate::engine::codebase_scan.
// See: find_boundary_matches(), find_literal_matches()

// ============================================================================
// File walking — delegates to crate::engine::codebase_scan
// ============================================================================

/// Build a ScanConfig appropriate for rename operations.
fn scan_config_for_scope(scope: &RenameScope) -> ScanConfig {
    let extensions = match scope {
        RenameScope::Code => ExtensionFilter::Except(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::Config => ExtensionFilter::Only(vec![
            "json".to_string(),
            "toml".to_string(),
            "yaml".to_string(),
            "yml".to_string(),
        ]),
        RenameScope::All => ExtensionFilter::SourceDefaults,
    };

    ScanConfig {
        extensions,
        ..ScanConfig::default()
    }
}

// ============================================================================
// Reference finding
// ============================================================================

/// Find all references to the rename term across the codebase.
///
/// After the initial pass, discovers additional case variants that exist in the
/// codebase but weren't generated (e.g., `WPAgent` when `WpAgent` was generated).
pub fn find_references(spec: &RenameSpec, root: &Path) -> Vec<Reference> {
    find_references_with_targeting(spec, root, &RenameTargeting::default())
}

/// Find references using optional include/exclude targeting controls.
pub fn find_references_with_targeting(
    spec: &RenameSpec,
    root: &Path,
    targeting: &RenameTargeting,
) -> Vec<Reference> {
    let config = scan_config_for_scope(&spec.scope);
    let files = target_files(codebase_scan::walk_files(root, &config), root, targeting);

    // Build the working variant list — may be extended by discovery
    let mut all_variants = spec.variants.clone();

    // Initial search with generated variants.
    if !spec.literal {
        // Discover additional case variants for any generated variant with 0 matches.
        // For example, if we generated "WpAgent" but the codebase uses "WPAgent",
        // the case-insensitive scan will find it and add it as a discovered variant.
        let mut discovered = Vec::new();
        for variant in &spec.variants {
            // Quick check: does this variant have any boundary matches?
            let has_matches = files.iter().any(|f| {
                std::fs::read_to_string(f)
                    .map(|content| {
                        content
                            .lines()
                            .any(|line| !find_boundary_matches(line, &variant.from).is_empty())
                    })
                    .unwrap_or(false)
            });

            if !has_matches {
                // No matches — discover what casing actually exists
                let casings = discover_casing_in_files(&files, &variant.from, spec.literal);
                for (actual_casing, _count) in &casings {
                    // Skip if it's the same as what we already have
                    if actual_casing == &variant.from {
                        continue;
                    }
                    // Skip if we already have this variant
                    if all_variants.iter().any(|v| v.from == *actual_casing) {
                        continue;
                    }
                    discovered.push(CaseVariant {
                        from: actual_casing.clone(),
                        to: variant.to.clone(),
                        label: format!("discovered ({})", variant.label),
                    });
                }
            }
        }
        all_variants.extend(discovered);
    }

    // Phase 2: Find all references using the full variant list
    let mut references = Vec::new();

    // Sort variants longest-first to prevent partial overlap
    all_variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));

    let use_literal = spec.literal;

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let relative = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        for (line_num, line) in content.lines().enumerate() {
            // Track which byte offsets in this line are already claimed
            // to prevent overlapping matches from shorter variants
            let mut claimed: Vec<(usize, usize)> = Vec::new();

            for variant in &all_variants {
                let positions = if use_literal {
                    find_literal_matches(line, &variant.from)
                } else {
                    find_boundary_matches(line, &variant.from)
                };
                for pos in positions {
                    let end = pos + variant.from.len();
                    // Skip if this range overlaps with an already-claimed match
                    if claimed.iter().any(|&(s, e)| pos < e && end > s) {
                        continue;
                    }
                    // Apply syntactic context filter
                    if !spec.rename_context.matches(line, pos, variant.from.len()) {
                        continue;
                    }
                    claimed.push((pos, end));
                    references.push(Reference {
                        file: relative.clone(),
                        line: line_num + 1,
                        column: pos + 1,
                        matched: variant.from.clone(),
                        replacement: variant.to.clone(),
                        variant: variant.label.clone(),
                        context: line.to_string(),
                    });
                }
            }
        }
    }

    references
}

fn discover_casing_in_files(
    files: &[PathBuf],
    pattern: &str,
    literal: bool,
) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };

        for line in content.lines() {
            if literal {
                for pos in find_literal_matches(line, pattern) {
                    let matched = &line[pos..pos + pattern.len()];
                    *counts.entry(matched.to_string()).or_insert(0) += 1;
                }
                continue;
            }

            for (_start, matched) in find_case_insensitive_matches(line, pattern) {
                *counts.entry(matched).or_insert(0) += 1;
            }
        }
    }

    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

// ============================================================================
// Rename generation
// ============================================================================

/// Generate file edits and file renames from found references.
pub fn generate_renames(spec: &RenameSpec, root: &Path) -> RenameResult {
    generate_renames_with_targeting(spec, root, &RenameTargeting::default())
}

/// Generate renames using optional include/exclude targeting controls.
pub fn generate_renames_with_targeting(
    spec: &RenameSpec,
    root: &Path,
    targeting: &RenameTargeting,
) -> RenameResult {
    let references = find_references_with_targeting(spec, root, targeting);
    let config = scan_config_for_scope(&spec.scope);
    let files = target_files(codebase_scan::walk_files(root, &config), root, targeting);

    // Sort variants longest-first to prevent partial matches
    let mut sorted_variants = spec.variants.clone();
    sorted_variants.sort_by(|a, b| b.from.len().cmp(&a.from.len()));

    // Generate file content edits using reverse-offset replacement
    let mut edits = Vec::new();
    let mut affected_files: HashMap<String, bool> = HashMap::new();
    let use_literal = spec.literal;

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let relative = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        // Collect all matches with their positions and replacements
        let mut all_matches: Vec<(usize, usize, String)> = Vec::new(); // (start, end, replacement)

        for variant in &sorted_variants {
            let positions = if use_literal {
                find_literal_matches(&content, &variant.from)
            } else {
                find_boundary_matches(&content, &variant.from)
            };
            for pos in positions {
                let end = pos + variant.from.len();
                // Skip if overlapping with an already-claimed longer match
                if all_matches.iter().any(|&(s, e, _)| pos < e && end > s) {
                    continue;
                }
                // Apply syntactic context filter on the line containing this match
                if spec.rename_context != RenameContext::All {
                    let line_start = content[..pos].rfind('\n').map_or(0, |p| p + 1);
                    let line_end = content[pos..].find('\n').map_or(content.len(), |p| pos + p);
                    let line = &content[line_start..line_end];
                    let col = pos - line_start;
                    if !spec.rename_context.matches(line, col, variant.from.len()) {
                        continue;
                    }
                }
                all_matches.push((pos, end, variant.to.clone()));
            }
        }

        if !all_matches.is_empty() {
            let count = all_matches.len();

            // Sort by position descending so we can replace from end to start
            // without invalidating earlier offsets
            all_matches.sort_by(|a, b| b.0.cmp(&a.0));

            let mut new_content = content;
            for (start, end, replacement) in &all_matches {
                new_content.replace_range(start..end, replacement);
            }

            affected_files.insert(relative.clone(), true);
            edits.push(FileEdit {
                file: relative,
                replacements: count,
                new_content,
            });
        }
    }

    // Generate file/directory renames
    let mut file_renames = Vec::new();
    if targeting.rename_files {
        for file_path in &files {
            let relative = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let mut new_relative = relative.clone();
            for variant in &sorted_variants {
                // Replace in path segments (word-boundary aware in file names)
                new_relative = new_relative.replace(&variant.from, &variant.to);
            }

            if new_relative != relative {
                file_renames.push(FileRename {
                    from: relative,
                    to: new_relative,
                });
            }
        }
    }

    // Deduplicate file renames
    file_renames.dedup_by(|a, b| a.from == b.from);

    let total_references = references.len();
    let total_files = affected_files.len() + file_renames.len();

    // Detect collisions
    let warnings = detect_collisions(&edits, &file_renames, root);

    RenameResult {
        variants: spec.variants.clone(),
        references,
        edits,
        file_renames,
        warnings,
        total_references,
        total_files,
        applied: false,
    }
}

fn target_files(files: Vec<PathBuf>, root: &Path, targeting: &RenameTargeting) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter(|file| {
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");

            if !targeting.include_globs.is_empty()
                && !targeting
                    .include_globs
                    .iter()
                    .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            if targeting
                .exclude_globs
                .iter()
                .any(|glob| glob_match::glob_match(glob, &relative))
            {
                return false;
            }

            true
        })
        .collect()
}

// ============================================================================
// Collision detection
// ============================================================================

/// Detect potential collisions in rename results.
///
/// Checks for:
/// 1. File rename targets that already exist on disk
/// 2. Duplicate identifiers within the same indentation block in edited files
///    (e.g., two struct fields both named `extensions` after rename)
fn detect_collisions(
    edits: &[FileEdit],
    file_renames: &[FileRename],
    root: &Path,
) -> Vec<RenameWarning> {
    let mut warnings = Vec::new();

    // Check file rename collisions — target already exists
    for rename in file_renames {
        let target = root.join(&rename.to);
        if target.exists() {
            warnings.push(RenameWarning {
                kind: "file_collision".to_string(),
                file: rename.to.clone(),
                line: None,
                message: format!(
                    "Rename target '{}' already exists on disk (from '{}')",
                    rename.to, rename.from
                ),
            });
        }
    }

    // Check content collisions — duplicate identifiers at same indentation
    for edit in edits {
        detect_duplicate_identifiers(&edit.file, &edit.new_content, &mut warnings);
    }

    warnings
}

/// Scan edited content for lines at the same indentation that introduce
/// duplicate field/identifier names. This catches the case where renaming
/// `modules` → `extensions` creates a collision with an existing `extensions` field.
fn detect_duplicate_identifiers(file: &str, content: &str, warnings: &mut Vec<RenameWarning>) {
    let lines: Vec<&str> = content.lines().collect();

    // Group lines by indentation level, looking for struct-like blocks
    // (lines with the same leading whitespace that contain identifier patterns)
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Look for struct/enum/block openers
        if trimmed.ends_with('{') || trimmed.ends_with("{{") {
            let block_indent = leading_spaces(lines.get(i + 1).unwrap_or(&""));
            if block_indent == 0 {
                i += 1;
                continue;
            }

            // Collect identifiers at this indent level until block closes
            let mut seen: HashMap<String, usize> = HashMap::new();
            let mut j = i + 1;

            while j < lines.len() {
                let block_line = lines[j];
                let block_trimmed = block_line.trim();

                // Block ended
                if block_trimmed == "}" || block_trimmed == "}," {
                    break;
                }

                // Only check lines at this exact indent level
                if leading_spaces(block_line) == block_indent {
                    if let Some(ident) = extract_field_identifier(block_trimmed) {
                        if let Some(&first_line) = seen.get(&ident) {
                            warnings.push(RenameWarning {
                                kind: "duplicate_identifier".to_string(),
                                file: file.to_string(),
                                line: Some(j + 1),
                                message: format!(
                                    "Duplicate identifier '{}' at line {} (first at line {})",
                                    ident,
                                    j + 1,
                                    first_line
                                ),
                            });
                        } else {
                            seen.insert(ident, j + 1);
                        }
                    }
                }

                j += 1;
            }

            i = j;
        } else {
            i += 1;
        }
    }
}

/// Count leading spaces on a line.
fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Extract the field/identifier name from a struct field or variable declaration line.
/// Returns the identifier if the line looks like a field declaration.
///
/// Matches patterns like:
/// - `pub field_name: Type,`
/// - `field_name: Type,`
/// - `pub(crate) field_name: Type,`
/// - `let field_name = ...`
/// - `fn field_name(...`
fn extract_field_identifier(trimmed: &str) -> Option<String> {
    // Skip attributes, comments, empty lines
    if trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.is_empty()
    {
        return None;
    }

    // Strip visibility modifiers
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub(super) "))
        .or_else(|| trimmed.strip_prefix("pub "))
        .unwrap_or(trimmed);

    // Strip let/fn/const/static
    let rest = rest
        .strip_prefix("let mut ")
        .or_else(|| rest.strip_prefix("let "))
        .or_else(|| rest.strip_prefix("fn "))
        .or_else(|| rest.strip_prefix("const "))
        .or_else(|| rest.strip_prefix("static "))
        .unwrap_or(rest);

    // Extract identifier (alphanumeric + underscore until : or ( or = or space)
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if ident.is_empty() {
        return None;
    }

    // Must be followed by : or ( or = or < (type params) to be an identifier
    let after = &rest[ident.len()..].trim_start();
    if after.starts_with(':')
        || after.starts_with('(')
        || after.starts_with('=')
        || after.starts_with('<')
    {
        Some(ident)
    } else {
        None
    }
}

// ============================================================================
// Apply renames
// ============================================================================

/// Apply rename edits and file renames to disk.
pub fn apply_renames(result: &mut RenameResult, root: &Path) -> Result<()> {
    // Apply content edits first
    for edit in &result.edits {
        let path = root.join(&edit.file);
        std::fs::write(&path, &edit.new_content).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
    }

    // Apply file renames (sort by path depth descending so children rename before parents)
    let mut renames = result.file_renames.clone();
    renames.sort_by(|a, b| {
        b.from
            .matches('/')
            .count()
            .cmp(&a.from.matches('/').count())
    });

    for rename in &renames {
        let from = root.join(&rename.from);
        let to = root.join(&rename.to);

        // Create parent dirs if needed
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if from.exists() {
            std::fs::rename(&from, &to).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("rename {} → {}", from.display(), to.display())),
                )
            })?;
        }
    }

    result.applied = true;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("widget"), "Widget");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("a"), "A");
    }

    #[test]
    fn pluralize_regular() {
        assert_eq!(pluralize("widget"), "widgets");
        assert_eq!(pluralize("gadget"), "gadgets");
    }

    #[test]
    fn pluralize_y_ending() {
        assert_eq!(pluralize("ability"), "abilities");
        assert_eq!(pluralize("query"), "queries");
    }

    #[test]
    fn pluralize_s_ending() {
        assert_eq!(pluralize("class"), "classes");
    }

    #[test]
    fn pluralize_preserves_ey_oy_ay() {
        assert_eq!(pluralize("key"), "keys");
        assert_eq!(pluralize("day"), "days");
    }

    #[test]
    fn rename_spec_generates_variants() {
        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();
        assert!(from_values.contains(&"widget"));
        assert!(from_values.contains(&"Widget"));
        assert!(from_values.contains(&"WIDGET"));
        assert!(from_values.contains(&"widgets"));
        assert!(from_values.contains(&"Widgets"));
        assert!(from_values.contains(&"WIDGETS"));

        let to_values: Vec<&str> = spec.variants.iter().map(|v| v.to.as_str()).collect();
        assert!(to_values.contains(&"gadget"));
        assert!(to_values.contains(&"Gadget"));
        assert!(to_values.contains(&"GADGET"));
        assert!(to_values.contains(&"gadgets"));
        assert!(to_values.contains(&"Gadgets"));
        assert!(to_values.contains(&"GADGETS"));
    }

    #[test]
    fn find_references_in_temp_dir() {
        let dir = std::env::temp_dir().join("homeboy_refactor_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "pub mod widget;\nuse crate::widget::WidgetManifest;\nconst WIDGET_DIR: &str = \"widgets\";\n",
        )
        .unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let refs = find_references(&spec, &dir);

        assert!(!refs.is_empty());

        // Should find: widget (2x), Widget (1x), WIDGET (1x), widgets (1x)
        let matched: Vec<&str> = refs.iter().map(|r| r.matched.as_str()).collect();
        assert!(
            matched.contains(&"widget"),
            "Expected 'widget' in {:?}",
            matched
        );
        assert!(
            matched.contains(&"Widget"),
            "Expected 'Widget' in {:?}",
            matched
        );
        assert!(
            matched.contains(&"WIDGET"),
            "Expected 'WIDGET' in {:?}",
            matched
        );
        assert!(
            matched.contains(&"widgets"),
            "Expected 'widgets' in {:?}",
            matched
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_renames_produces_edits() {
        let dir = std::env::temp_dir().join("homeboy_refactor_gen_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("test.rs"), "pub mod widget;\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let result = generate_renames(&spec, &dir);

        assert!(!result.edits.is_empty());
        assert_eq!(result.edits[0].new_content, "pub mod gadget;\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_renames_detects_file_renames() {
        let dir = std::env::temp_dir().join("homeboy_refactor_file_rename_test");
        let sub = dir.join("widget");
        let _ = std::fs::create_dir_all(&sub);

        std::fs::write(sub.join("widget.rs"), "fn widget_init() {}\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let result = generate_renames(&spec, &dir);

        assert!(!result.file_renames.is_empty());
        // Should want to rename widget/widget.rs → gadget/gadget.rs
        let rename = result
            .file_renames
            .iter()
            .find(|r| r.from.contains("widget.rs"))
            .unwrap();
        assert!(rename.to.contains("gadget.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn word_boundary_no_false_positives() {
        let dir = std::env::temp_dir().join("homeboy_refactor_boundary_test");
        let _ = std::fs::create_dir_all(&dir);

        // "widgets_plus" should NOT be matched as "widget" — the 's' makes it "widgets" (plural variant)
        // but "widgetry" should NOT be matched when renaming "widget"
        std::fs::write(dir.join("test.rs"), "let widgetry = true;\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let refs = find_references(&spec, &dir);

        assert!(
            refs.is_empty(),
            "Should not match 'widgetry' when renaming 'widget'"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_renames_writes_to_disk() {
        let dir = std::env::temp_dir().join("homeboy_refactor_apply_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("test.rs"), "pub mod widget;\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let mut result = generate_renames(&spec, &dir);

        apply_renames(&mut result, &dir).unwrap();
        assert!(result.applied);

        let content = std::fs::read_to_string(dir.join("test.rs")).unwrap();
        assert_eq!(content, "pub mod gadget;\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snake_case_compounds_match() {
        // find_boundary_matches should match "widget" inside "load_widget", "is_widget_linked", etc.
        let matches = find_boundary_matches("load_widget", "widget");
        assert_eq!(matches, vec![5], "Should match 'widget' in 'load_widget'");

        let matches = find_boundary_matches("is_widget_linked", "widget");
        assert_eq!(
            matches,
            vec![3],
            "Should match 'widget' in 'is_widget_linked'"
        );

        let matches = find_boundary_matches("widget_init", "widget");
        assert_eq!(
            matches,
            vec![0],
            "Should match 'widget' at start of 'widget_init'"
        );

        let matches = find_boundary_matches("WIDGET_DIR", "WIDGET");
        assert_eq!(matches, vec![0], "Should match 'WIDGET' in 'WIDGET_DIR'");

        let matches = find_boundary_matches("THE_WIDGET_CONFIG", "WIDGET");
        assert_eq!(
            matches,
            vec![4],
            "Should match 'WIDGET' in 'THE_WIDGET_CONFIG'"
        );
    }

    #[test]
    fn snake_case_rename_in_file() {
        let dir = std::env::temp_dir().join("homeboy_refactor_snake_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "fn load_widget() {}\nfn is_widget_linked() -> bool { true }\nconst WIDGET_DIR: &str = \"widgets\";\n",
        )
        .unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let result = generate_renames(&spec, &dir);

        assert!(!result.edits.is_empty());
        let content = &result.edits[0].new_content;
        assert!(
            content.contains("load_gadget"),
            "Expected 'load_gadget' in:\n{}",
            content
        );
        assert!(
            content.contains("is_gadget_linked"),
            "Expected 'is_gadget_linked' in:\n{}",
            content
        );
        assert!(
            content.contains("GADGET_DIR"),
            "Expected 'GADGET_DIR' in:\n{}",
            content
        );
        assert!(
            content.contains("\"gadgets\""),
            "Expected '\"gadgets\"' in:\n{}",
            content
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_modules_not_matched() {
        // "node_modules" should NOT have "module" matched inside it — the plural
        // variant "modules" consumes it first, but we don't want partial matches either.
        // "node_modules" as a directory name is handled by SKIP_DIRS, but in content
        // the plural variant "modules" should match (not "module" partially).
        let matches = find_boundary_matches("node_modules", "module");
        assert!(
            matches.is_empty(),
            "Should not match 'module' inside 'node_modules' — 's' follows"
        );

        // But "modules" (plural) should match
        let matches = find_boundary_matches("node_modules", "modules");
        assert_eq!(matches, vec![5], "Should match 'modules' in 'node_modules'");
    }

    #[test]
    fn extract_field_identifier_works() {
        assert_eq!(
            extract_field_identifier("pub name: String,"),
            Some("name".to_string())
        );
        assert_eq!(
            extract_field_identifier("pub(crate) id: u32,"),
            Some("id".to_string())
        );
        assert_eq!(
            extract_field_identifier("count: usize,"),
            Some("count".to_string())
        );
        assert_eq!(
            extract_field_identifier("let value = 42;"),
            Some("value".to_string())
        );
        assert_eq!(
            extract_field_identifier("fn init("),
            Some("init".to_string())
        );
        assert_eq!(extract_field_identifier("// a comment"), None);
        assert_eq!(extract_field_identifier("#[serde(skip)]"), None);
        assert_eq!(extract_field_identifier(""), None);
    }

    #[test]
    fn detect_duplicate_identifiers_catches_collision() {
        let content = "struct Foo {\n    pub name: String,\n    pub name: u32,\n}\n";
        let mut warnings = Vec::new();
        detect_duplicate_identifiers("test.rs", content, &mut warnings);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, "duplicate_identifier");
        assert!(warnings[0].message.contains("name"));
    }

    #[test]
    fn detect_duplicate_identifiers_no_false_positive() {
        let content = "struct Foo {\n    pub name: String,\n    pub age: u32,\n}\n";
        let mut warnings = Vec::new();
        detect_duplicate_identifiers("test.rs", content, &mut warnings);
        assert!(warnings.is_empty());
    }

    #[test]
    fn collision_detection_file_rename_target_exists() {
        let dir = std::env::temp_dir().join("homeboy_collision_file_test");
        let _ = std::fs::create_dir_all(&dir);

        // Create both source and target files
        std::fs::write(dir.join("old.rs"), "fn old() {}\n").unwrap();
        std::fs::write(dir.join("new.rs"), "fn new() {}\n").unwrap();

        let file_renames = vec![FileRename {
            from: "old.rs".to_string(),
            to: "new.rs".to_string(),
        }];

        let warnings = detect_collisions(&[], &file_renames, &dir);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, "file_collision");
        assert!(warnings[0].message.contains("new.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collision_detection_in_generate_renames() {
        let dir = std::env::temp_dir().join("homeboy_collision_gen_test");
        let _ = std::fs::create_dir_all(&dir);

        // This simulates the exact #284 bug: struct has both `widgets` and `gadgets` fields,
        // and renaming widget → gadget would create two `gadgets` fields.
        std::fs::write(
            dir.join("test.rs"),
            "struct Config {\n    pub widgets: Vec<String>,\n    pub gadgets: Vec<u32>,\n}\n",
        )
        .unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let result = generate_renames(&spec, &dir);

        assert!(
            !result.warnings.is_empty(),
            "Should detect duplicate 'gadgets' field"
        );
        assert!(result
            .warnings
            .iter()
            .any(|w| w.kind == "duplicate_identifier"));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.message.contains("gadgets")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_build_dir_not_skipped() {
        // scripts/build/ should be scanned (build is only skipped at root level)
        let dir = std::env::temp_dir().join("homeboy_refactor_build_dir_test");
        let sub = dir.join("scripts").join("build");
        let _ = std::fs::create_dir_all(&sub);

        std::fs::write(sub.join("setup.sh"), "WIDGET_PATH=\"$HOME\"\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let refs = find_references(&spec, &dir);

        assert!(
            !refs.is_empty(),
            "Should find 'WIDGET' in scripts/build/setup.sh"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_build_dir_still_skipped() {
        // build/ at root should still be skipped
        let dir = std::env::temp_dir().join("homeboy_refactor_root_build_test");
        let build_dir = dir.join("build");
        let _ = std::fs::create_dir_all(&build_dir);

        std::fs::write(build_dir.join("output.rs"), "let widget = true;\n").unwrap();

        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let refs = find_references(&spec, &dir);

        assert!(
            refs.is_empty(),
            "Should NOT find refs in root-level build/ dir"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ====================================================================
    // Literal mode tests
    // ====================================================================

    #[test]
    fn literal_spec_has_single_variant() {
        let spec = RenameSpec::literal(
            "datamachine-events",
            "data-machine-events",
            RenameScope::All,
        );
        assert!(spec.literal);
        assert_eq!(spec.variants.len(), 1);
        assert_eq!(spec.variants[0].from, "datamachine-events");
        assert_eq!(spec.variants[0].to, "data-machine-events");
        assert_eq!(spec.variants[0].label, "literal");
    }

    #[test]
    fn find_literal_matches_exact() {
        // Should find exact substring — no boundary detection
        let matches = find_literal_matches("datamachine-events is great", "datamachine-events");
        assert_eq!(matches, vec![0]);

        // Should match inside larger strings (no boundary filtering)
        let matches = find_literal_matches("the-datamachine-events-plugin", "datamachine-events");
        assert_eq!(matches, vec![4]);

        // Multiple occurrences
        let matches = find_literal_matches(
            "datamachine-events and datamachine-events",
            "datamachine-events",
        );
        assert_eq!(matches, vec![0, 23]);

        // No match
        let matches = find_literal_matches("data-machine-events", "datamachine-events");
        assert!(matches.is_empty());
    }

    #[test]
    fn literal_mode_finds_references_in_file() {
        let dir = std::env::temp_dir().join("homeboy_refactor_literal_refs_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("plugin.php"),
            "// Plugin: datamachine-events\ndefine('DATAMACHINE_EVENTS_VERSION', '1.0');\nfunction datamachine_events_init() {}\n",
        )
        .unwrap();

        // Literal mode: only exact match, no case variants
        let spec = RenameSpec::literal(
            "datamachine-events",
            "data-machine-events",
            RenameScope::All,
        );
        let refs = find_references(&spec, &dir);

        // Should find only the hyphenated form, not DATAMACHINE_EVENTS or datamachine_events
        assert_eq!(
            refs.len(),
            1,
            "Should find exactly 1 literal match, got: {:?}",
            refs.iter().map(|r| &r.matched).collect::<Vec<_>>()
        );
        assert_eq!(refs[0].matched, "datamachine-events");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literal_mode_generates_correct_edits() {
        let dir = std::env::temp_dir().join("homeboy_refactor_literal_edit_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("plugin.php"),
            "Text Domain: datamachine-events\nSlug: datamachine-events\n",
        )
        .unwrap();

        let spec = RenameSpec::literal(
            "datamachine-events",
            "data-machine-events",
            RenameScope::All,
        );
        let result = generate_renames(&spec, &dir);

        assert_eq!(result.edits.len(), 1);
        assert_eq!(result.edits[0].replacements, 2);
        assert_eq!(
            result.edits[0].new_content,
            "Text Domain: data-machine-events\nSlug: data-machine-events\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literal_mode_renames_files() {
        let dir = std::env::temp_dir().join("homeboy_refactor_literal_file_rename_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("datamachine-events.php"), "// main file\n").unwrap();

        let spec = RenameSpec::literal(
            "datamachine-events",
            "data-machine-events",
            RenameScope::All,
        );
        let result = generate_renames(&spec, &dir);

        assert!(
            !result.file_renames.is_empty(),
            "Should rename datamachine-events.php"
        );
        let rename = &result.file_renames[0];
        assert_eq!(rename.from, "datamachine-events.php");
        assert_eq!(rename.to, "data-machine-events.php");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literal_mode_apply_writes_to_disk() {
        let dir = std::env::temp_dir().join("homeboy_refactor_literal_apply_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("test.php"), "slug: datamachine-events\n").unwrap();

        let spec = RenameSpec::literal(
            "datamachine-events",
            "data-machine-events",
            RenameScope::All,
        );
        let mut result = generate_renames(&spec, &dir);

        apply_renames(&mut result, &dir).unwrap();
        assert!(result.applied);

        let content = std::fs::read_to_string(dir.join("test.php")).unwrap();
        assert_eq!(content, "slug: data-machine-events\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literal_mode_no_boundary_filtering() {
        // Normal mode would NOT match "widget" inside "widgetry" — literal mode SHOULD
        let matches = find_literal_matches("widgetry", "widget");
        assert_eq!(
            matches,
            vec![0],
            "Literal should match 'widget' inside 'widgetry'"
        );

        // Normal mode boundary test for comparison
        let boundary_matches = find_boundary_matches("widgetry", "widget");
        assert!(
            boundary_matches.is_empty(),
            "Boundary mode should NOT match 'widget' inside 'widgetry'"
        );
    }

    // ====================================================================
    // Word splitting tests
    // ====================================================================

    #[test]
    fn split_words_kebab() {
        assert_eq!(split_words("wp-agent"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("data-machine-agent"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_snake() {
        assert_eq!(split_words("wp_agent"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("data_machine_agent"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_upper_snake() {
        assert_eq!(split_words("WP_AGENT"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("DATA_MACHINE_AGENT"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_pascal() {
        assert_eq!(split_words("WpAgent"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("DataMachineAgent"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_consecutive_uppercase() {
        // WPAgent: WP is an acronym, Agent is a word
        assert_eq!(split_words("WPAgent"), vec!["wp", "agent"]);
        assert_eq!(split_words("XMLParser"), vec!["xml", "parser"]);
        assert_eq!(split_words("HTTPClient"), vec!["http", "client"]);
        // All-uppercase stays as one word (no lowercase to trigger split)
        assert_eq!(split_words("HTTP"), vec!["http"]);
    }

    #[test]
    fn split_words_camel() {
        assert_eq!(split_words("wpAgent"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("dataMachineAgent"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_display() {
        assert_eq!(split_words("WP Agent"), vec!["wp", "agent"]);
        assert_eq!(
            split_words("Data Machine Agent"),
            vec!["data", "machine", "agent"]
        );
    }

    #[test]
    fn split_words_single() {
        assert_eq!(split_words("widget"), vec!["widget"]);
        assert_eq!(split_words("Widget"), vec!["widget"]);
        assert_eq!(split_words("WIDGET"), vec!["widget"]);
    }

    // ====================================================================
    // Cross-separator variant generation tests
    // ====================================================================

    #[test]
    fn cross_separator_variants_from_kebab() {
        let spec = RenameSpec::new("wp-agent", "data-machine-agent", RenameScope::All);
        let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();
        let to_values: Vec<&str> = spec.variants.iter().map(|v| v.to.as_str()).collect();

        // Singular forms — all naming conventions
        assert!(from_values.contains(&"wp-agent"), "Missing kebab from");
        assert!(from_values.contains(&"wp_agent"), "Missing snake from");
        assert!(
            from_values.contains(&"WP_AGENT"),
            "Missing UPPER_SNAKE from"
        );
        assert!(from_values.contains(&"WpAgent"), "Missing PascalCase from");
        assert!(from_values.contains(&"wpAgent"), "Missing camelCase from");
        assert!(from_values.contains(&"Wp Agent"), "Missing display from");

        assert!(
            to_values.contains(&"data-machine-agent"),
            "Missing kebab to"
        );
        assert!(
            to_values.contains(&"data_machine_agent"),
            "Missing snake to"
        );
        assert!(
            to_values.contains(&"DATA_MACHINE_AGENT"),
            "Missing UPPER_SNAKE to"
        );
        assert!(
            to_values.contains(&"DataMachineAgent"),
            "Missing PascalCase to"
        );
        assert!(
            to_values.contains(&"dataMachineAgent"),
            "Missing camelCase to"
        );
        assert!(
            to_values.contains(&"Data Machine Agent"),
            "Missing display to"
        );

        // Plural forms
        assert!(
            from_values.contains(&"wp-agents"),
            "Missing plural kebab from"
        );
        assert!(
            from_values.contains(&"wp_agents"),
            "Missing plural snake from"
        );
        assert!(
            from_values.contains(&"WP_AGENTS"),
            "Missing plural UPPER_SNAKE from"
        );
        assert!(
            from_values.contains(&"WpAgents"),
            "Missing plural PascalCase from"
        );
    }

    #[test]
    fn cross_separator_variants_from_pascal() {
        // Providing PascalCase input should produce the same cross-separator variants
        let spec = RenameSpec::new("WpAgent", "DataMachineAgent", RenameScope::All);
        let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();

        assert!(from_values.contains(&"wp-agent"), "Missing kebab from");
        assert!(from_values.contains(&"wp_agent"), "Missing snake from");
        assert!(
            from_values.contains(&"WP_AGENT"),
            "Missing UPPER_SNAKE from"
        );
        assert!(from_values.contains(&"WpAgent"), "Missing PascalCase from");
        assert!(from_values.contains(&"wpAgent"), "Missing camelCase from");
    }

    #[test]
    fn cross_separator_variants_from_snake() {
        // Providing snake_case input should produce the same cross-separator variants
        let spec = RenameSpec::new("wp_agent", "data_machine_agent", RenameScope::All);
        let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();

        assert!(from_values.contains(&"wp-agent"), "Missing kebab from");
        assert!(from_values.contains(&"wp_agent"), "Missing snake from");
        assert!(
            from_values.contains(&"WP_AGENT"),
            "Missing UPPER_SNAKE from"
        );
        assert!(from_values.contains(&"WpAgent"), "Missing PascalCase from");
    }

    #[test]
    fn single_word_variants_dedup() {
        // For a single word, all separator joins produce the same thing
        let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
        let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();

        // Should still have the core variants
        assert!(from_values.contains(&"widget"));
        assert!(from_values.contains(&"Widget"));
        assert!(from_values.contains(&"WIDGET"));
        assert!(from_values.contains(&"widgets"));
        assert!(from_values.contains(&"Widgets"));
        assert!(from_values.contains(&"WIDGETS"));

        // No duplicate entries
        let mut seen = std::collections::HashSet::new();
        for v in &spec.variants {
            assert!(seen.insert(&v.from), "Duplicate variant 'from': {}", v.from);
        }
    }

    // ====================================================================
    // Boundary detection for consecutive-uppercase PascalCase
    // ====================================================================

    #[test]
    fn boundary_matches_consecutive_uppercase_pascal() {
        // WPAgent → should match "Agent" at position 2
        let matches = find_boundary_matches("WPAgent", "Agent");
        assert_eq!(
            matches,
            vec![2],
            "Should match 'Agent' in 'WPAgent' at consecutive-uppercase boundary"
        );

        // WPAgent → should match "WP" at position 0
        let matches = find_boundary_matches("WPAgent", "WP");
        assert_eq!(matches, vec![0], "Should match 'WP' at start of 'WPAgent'");

        // XMLParser → should match "XML" and "Parser"
        let matches = find_boundary_matches("XMLParser", "XML");
        assert_eq!(
            matches,
            vec![0],
            "Should match 'XML' at start of 'XMLParser'"
        );

        let matches = find_boundary_matches("XMLParser", "Parser");
        assert_eq!(matches, vec![3], "Should match 'Parser' in 'XMLParser'");
    }

    #[test]
    fn boundary_matches_wp_agent_display_name() {
        // "WP Agent" with a space — should match at word boundaries
        let matches = find_boundary_matches("Plugin: WP Agent v1", "WP Agent");
        assert_eq!(
            matches,
            vec![8],
            "Should match 'WP Agent' in display context"
        );
    }

    #[test]
    fn cross_separator_end_to_end_rename() {
        // The real use case: rename wp-agent → data-machine-agent across all conventions
        let dir = std::env::temp_dir().join("homeboy_cross_sep_e2e_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("plugin.php"),
            concat!(
                "// Plugin: wp-agent\n",
                "namespace WpAgent;\n",
                "define('WP_AGENT_VERSION', '1.0');\n",
                "function wp_agent_init() {}\n",
                "// slug: wp-agents\n",
            ),
        )
        .unwrap();

        let spec = RenameSpec::new("wp-agent", "data-machine-agent", RenameScope::All);
        let result = generate_renames(&spec, &dir);

        assert!(!result.edits.is_empty(), "Should have edits");
        let content = &result.edits[0].new_content;

        assert!(
            content.contains("data-machine-agent"),
            "Should rename kebab: {}",
            content
        );
        assert!(
            content.contains("DataMachineAgent"),
            "Should rename PascalCase: {}",
            content
        );
        assert!(
            content.contains("DATA_MACHINE_AGENT_VERSION"),
            "Should rename UPPER_SNAKE: {}",
            content
        );
        assert!(
            content.contains("data_machine_agent_init"),
            "Should rename snake_case: {}",
            content
        );
        assert!(
            content.contains("data-machine-agents"),
            "Should rename plural kebab: {}",
            content
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_glob_limits_edits_to_targeted_files() {
        let dir = std::env::temp_dir().join("homeboy_refactor_target_include_test");
        let _ = std::fs::create_dir_all(dir.join("src"));
        let _ = std::fs::create_dir_all(dir.join("tests"));

        std::fs::write(dir.join("src/lib.rs"), "fn mark_item_processed() {}\n").unwrap();
        std::fs::write(
            dir.join("tests/lib_test.rs"),
            "fn test_mark_item_processed() {}\n",
        )
        .unwrap();

        let spec = RenameSpec::new(
            "mark_item_processed",
            "add_processed_item",
            RenameScope::All,
        );
        let targeting = RenameTargeting {
            include_globs: vec!["tests/**/*.rs".to_string()],
            ..RenameTargeting::default()
        };

        let result = generate_renames_with_targeting(&spec, &dir, &targeting);

        assert_eq!(result.edits.len(), 1, "Should only edit tests files");
        assert_eq!(result.edits[0].file, "tests/lib_test.rs");
        assert!(result.edits[0].new_content.contains("add_processed_item"));
        assert!(!result.edits[0].new_content.contains("mark_item_processed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exclude_glob_omits_matching_files() {
        let dir = std::env::temp_dir().join("homeboy_refactor_target_exclude_test");
        let _ = std::fs::create_dir_all(dir.join("src"));
        let _ = std::fs::create_dir_all(dir.join("tests"));

        std::fs::write(dir.join("src/lib.rs"), "fn mark_item_processed() {}\n").unwrap();
        std::fs::write(
            dir.join("tests/lib_test.rs"),
            "fn test_mark_item_processed() {}\n",
        )
        .unwrap();

        let spec = RenameSpec::new(
            "mark_item_processed",
            "add_processed_item",
            RenameScope::All,
        );
        let targeting = RenameTargeting {
            exclude_globs: vec!["src/**/*.rs".to_string()],
            ..RenameTargeting::default()
        };

        let result = generate_renames_with_targeting(&spec, &dir, &targeting);

        assert_eq!(result.edits.len(), 1, "Should skip excluded src files");
        assert_eq!(result.edits[0].file, "tests/lib_test.rs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_file_renames_suppresses_path_renames() {
        let dir = std::env::temp_dir().join("homeboy_refactor_no_file_rename_test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("mark_item_processed_test.rs"), "fn ok() {}\n").unwrap();

        let spec = RenameSpec::new(
            "mark_item_processed",
            "add_processed_item",
            RenameScope::All,
        );
        let targeting = RenameTargeting {
            rename_files: false,
            ..RenameTargeting::default()
        };

        let result = generate_renames_with_targeting(&spec, &dir, &targeting);
        assert!(
            result.file_renames.is_empty(),
            "File renames should be disabled when rename_files=false"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_key_filters_to_string_keys_only() {
        let dir = std::env::temp_dir().join("homeboy_context_key_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"
$handler_config = [];
$input['handler_config'] = 'value';
function foo($handler_config) {}
echo "handler_config";
$obj->handler_config = true;
"#;
        std::fs::write(dir.join("test.php"), content).unwrap();

        let mut spec = RenameSpec::literal("handler_config", "handler_configs", RenameScope::All);
        spec.rename_context = RenameContext::Key;

        let refs = find_references_with_targeting(&spec, &dir, &RenameTargeting::default());

        // Should match: 'handler_config' (string key), "handler_config" (string),
        // ->handler_config (property access)
        // Should NOT match: $handler_config (variable), $handler_config (parameter)
        let matched_lines: Vec<usize> = refs.iter().map(|r| r.line).collect();
        assert!(
            !matched_lines.contains(&2), // $handler_config = [] — variable
            "Should not match variable assignment, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            matched_lines.contains(&3), // $input['handler_config'] — string key
            "Should match string key, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            !matched_lines.contains(&4), // function foo($handler_config) — parameter
            "Should not match function parameter, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            matched_lines.contains(&5), // "handler_config" — string
            "Should match string literal, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            matched_lines.contains(&6), // ->handler_config — property access
            "Should match property access, got matches at lines: {:?}",
            matched_lines
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_variable_filters_to_variables_only() {
        let dir = std::env::temp_dir().join("homeboy_context_var_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"
$handler_config = [];
$input['handler_config'] = 'value';
$obj->handler_config = true;
"#;
        std::fs::write(dir.join("test.php"), content).unwrap();

        let mut spec = RenameSpec::literal("handler_config", "handler_configs", RenameScope::All);
        spec.rename_context = RenameContext::Variable;

        let refs = find_references_with_targeting(&spec, &dir, &RenameTargeting::default());

        let matched_lines: Vec<usize> = refs.iter().map(|r| r.line).collect();
        assert!(
            matched_lines.contains(&2), // $handler_config — variable
            "Should match PHP variable, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            !matched_lines.contains(&3), // 'handler_config' — string key, not variable
            "Should not match string key, got matches at lines: {:?}",
            matched_lines
        );
        assert!(
            !matched_lines.contains(&4), // ->handler_config — property access, not variable
            "Should not match property access, got matches at lines: {:?}",
            matched_lines
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
