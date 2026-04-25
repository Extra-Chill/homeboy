//! Pattern-based code transforms — regex find/replace across a codebase.
//!
//! Applies named transform sets (collections of find/replace rules) to files
//! matching glob patterns. Rules are defined in `homeboy.json` under the
//! `transforms` key, or passed ad-hoc via CLI flags.
//!
//! Phase 1: line-context regex transforms (no AST, no extension scripts).

use glob_match::glob_match;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::engine::local_files;
use crate::error::{Error, Result};

// ============================================================================
// Rule model
// ============================================================================

/// A named collection of transform rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformSet {
    /// Human-readable description of this transform set.
    #[serde(default)]
    pub description: String,
    /// Ordered list of rules to apply.
    pub rules: Vec<TransformRule>,
}

/// A single find/replace rule with a file glob filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformRule {
    /// Unique identifier within the set.
    pub id: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Regex pattern to find (supports capture groups).
    pub find: String,
    /// Replacement template. Supports `$1`, `$2`, `${name}` capture group refs,
    /// `$1:lower`/`:upper`/`:kebab`/`:snake`/`:pascal`/`:camel` case transforms,
    /// and `$$` for a literal dollar sign.
    ///
    /// Backslash escapes are collapsed before the template is handed to the
    /// regex engine: `\\` → one literal backslash, `\n` → newline, `\t` → tab,
    /// `\r` → CR, `\0` → nul, `\"` → `"`, `\'` → `'`. Unknown escapes pass
    /// through verbatim. This means that to emit a PHP fully-qualified name
    /// like `\WP_Foo` on disk, write `\\WP_Foo` in JSON (which decodes to
    /// `\WP_Foo` in memory — the literal `\` you want). See #1277.
    pub replace: String,
    /// Glob pattern for files to apply to (e.g., `tests/**/*.php`).
    #[serde(default = "default_files_glob")]
    pub files: String,
    /// Match context: "line" (default) or "file" (whole-file regex, for multi-line).
    #[serde(default = "default_context")]
    pub context: String,
}

fn default_files_glob() -> String {
    "**/*".to_string()
}

// ============================================================================
// Output model
// ============================================================================

/// Result of applying a transform set.
#[derive(Debug, Clone, Serialize)]
pub struct TransformResult {
    /// Name of the transform set (or "ad-hoc" for CLI-provided rules).
    pub name: String,
    /// Per-rule results.
    pub rules: Vec<RuleResult>,
    /// Total replacements across all rules.
    pub total_replacements: usize,
    /// Total files modified.
    pub total_files: usize,
    /// Whether changes were written to disk.
    pub written: bool,
}

/// Result for a single rule.
#[derive(Debug, Clone, Serialize)]
pub struct RuleResult {
    /// Rule ID.
    pub id: String,
    /// Rule description.
    pub description: String,
    /// Matches found.
    pub matches: Vec<TransformMatch>,
    /// Number of replacements.
    pub replacement_count: usize,
}

/// A single match/replacement within a file.
#[derive(Debug, Clone, Serialize)]
pub struct TransformMatch {
    /// File path relative to component root.
    pub file: String,
    /// Line number (1-indexed). For file-context, this is the first line of the match.
    pub line: usize,
    /// Original text that matched.
    pub before: String,
    /// Replacement text.
    pub after: String,
}

// ============================================================================
// Replacement template unescape
// ============================================================================

/// Unescape backslash sequences in a replacement template.
///
/// Users writing regex-replace rules in `homeboy.json` (or on the CLI) think of
/// the `replace` value the way they think of sed, shell, or `String.replace` —
/// `\\` means one literal backslash, `\n` means a newline, `\t` means a tab.
/// The regex crate's native replacement syntax, on the other hand, passes
/// backslashes through verbatim and only recognizes `$1`/`${name}`/`$$`. That
/// left users with no ergonomic way to emit a single literal backslash without
/// over-escaping in JSON and then hand-collapsing the result.
///
/// This helper closes the gap: it runs a single pass over the input and
/// collapses common C-style escapes so that by the time the regex crate sees
/// the template, `\\` is already one backslash.
///
/// Rules:
/// - `\\` → one literal backslash
/// - `\n` → newline, `\t` → tab, `\r` → carriage return, `\0` → nul
/// - `\"` → `"`, `\'` → `'`
/// - Any other `\X` is passed through unchanged (so stray escapes don't eat
///   characters silently — regex-crate `$` syntax keeps working via `$$`).
///
/// `$` handling (`$1`, `$$`, `${name}`) is intentionally left to the regex
/// crate. This pass only touches backslashes.
///
/// See: https://github.com/Extra-Chill/homeboy/issues/1277
pub(crate) fn unescape_replacement_template(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            // Unknown escape — preserve both characters so users aren't
            // surprised by silent character loss.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ============================================================================
// Rule loading
// ============================================================================

const HOMEBOY_JSON: &str = "homeboy.json";
const TRANSFORMS_KEY: &str = "transforms";

/// Load a named transform set from `homeboy.json` in the given root directory.
pub fn load_transform_set(root: &Path, name: &str) -> Result<TransformSet> {
    let json_path = root.join(HOMEBOY_JSON);
    if !json_path.exists() {
        return Err(Error::internal_io(
            format!("No homeboy.json found at {}", json_path.display()),
            Some("transform.load".to_string()),
        ));
    }

    let content = local_files::read_file(&json_path, "read homeboy.json")?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse homeboy.json: {}", e),
            Some("transform.load".to_string()),
        )
    })?;

    let transforms = data.get(TRANSFORMS_KEY).ok_or_else(|| {
        Error::config_missing_key(
            TRANSFORMS_KEY.to_string(),
            Some(json_path.to_string_lossy().to_string()),
        )
    })?;

    let set_value = transforms.get(name).ok_or_else(|| {
        // List available transforms for a helpful error
        let available: Vec<&str> = transforms
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        Error::internal_io(
            format!(
                "Transform set '{}' not found. Available: {:?}",
                name, available
            ),
            Some("transform.load".to_string()),
        )
    })?;

    serde_json::from_value(set_value.clone()).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse transform set '{}': {}", name, e),
            Some("transform.load".to_string()),
        )
    })
}

/// Create a transform set from ad-hoc CLI arguments.
pub fn ad_hoc_transform(find: &str, replace: &str, files: &str, context: &str) -> TransformSet {
    TransformSet {
        description: "Ad-hoc transform".to_string(),
        rules: vec![TransformRule {
            id: "ad-hoc".to_string(),
            description: String::new(),
            find: find.to_string(),
            replace: replace.to_string(),
            files: files.to_string(),
            context: context.to_string(),
        }],
    }
}

// ============================================================================
// Transform engine
// ============================================================================

/// Apply a transform set to a codebase rooted at `root`.
///
/// If `write` is true, modified files are written to disk.
/// If `rule_filter` is Some, only the rule with that ID is applied.
pub fn apply_transforms(
    root: &Path,
    name: &str,
    set: &TransformSet,
    write: bool,
    rule_filter: Option<&str>,
) -> Result<TransformResult> {
    // Compile all regexes up front
    let compiled_rules: Vec<(&TransformRule, Regex)> = set
        .rules
        .iter()
        .filter(|r| rule_filter.is_none_or(|f| r.id == f))
        .map(|r| {
            let regex = Regex::new(&r.find).map_err(|e| {
                Error::internal_io(
                    format!("Invalid regex in rule '{}': {}", r.id, e),
                    Some("transform.apply".to_string()),
                )
            })?;
            Ok((r, regex))
        })
        .collect::<Result<Vec<_>>>()?;

    if compiled_rules.is_empty() {
        if let Some(filter) = rule_filter {
            let available: Vec<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
            return Err(Error::internal_io(
                format!(
                    "Rule '{}' not found in transform set '{}'. Available: {:?}",
                    filter, name, available
                ),
                Some("transform.apply".to_string()),
            ));
        }
    }

    // Walk all files once
    let files = codebase_scan::walk_files(
        root,
        &ScanConfig {
            extensions: ExtensionFilter::All,
            ..Default::default()
        },
    );

    // Apply each rule
    let mut rule_results = Vec::new();
    // Track cumulative edits per file: file_path → final content
    let mut file_edits: HashMap<PathBuf, String> = HashMap::new();

    for (rule, regex) in &compiled_rules {
        let matching_files: Vec<&PathBuf> = files
            .iter()
            .filter(|f| {
                let rel = f.strip_prefix(root).unwrap_or(f);
                let rel_str = rel.to_string_lossy();
                // Normalize backslashes for Windows compat
                let normalized = rel_str.replace('\\', "/");
                glob_match(&rule.files, &normalized)
            })
            .collect();

        let mut matches = Vec::new();

        // Collapse C-style backslash escapes in the replacement template once
        // per rule so `\\` in JSON → one literal backslash on disk, matching
        // sed/shell/String.replace conventions. See #1277.
        let replace_unescaped = unescape_replacement_template(&rule.replace);

        for file_path in matching_files {
            // Read from accumulated edits or original file
            let content = if let Some(edited) = file_edits.get(file_path) {
                edited.clone()
            } else {
                match std::fs::read_to_string(file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };

            let relative = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let (new_content, file_matches) = if rule.context == "file" {
                apply_file_context(regex, &replace_unescaped, &content, &relative)
            } else if rule.context == "hoist_static" {
                apply_hoist_static_context(regex, &replace_unescaped, &content, &relative)
            } else {
                apply_line_context(regex, &replace_unescaped, &content, &relative)
            };

            if !file_matches.is_empty() {
                matches.extend(file_matches);
                file_edits.insert(file_path.clone(), new_content);
            }
        }

        let replacement_count = matches.len();
        rule_results.push(RuleResult {
            id: rule.id.clone(),
            description: rule.description.clone(),
            matches,
            replacement_count,
        });
    }

    // Calculate totals
    let total_replacements: usize = rule_results.iter().map(|r| r.replacement_count).sum();
    let total_files = file_edits.len();

    // Write if requested
    if write && !file_edits.is_empty() {
        for (path, content) in &file_edits {
            local_files::write_file(path, content, "write transformed file")?;
        }
    }

    Ok(TransformResult {
        name: name.to_string(),
        rules: rule_results,
        total_replacements,
        total_files,
        written: write,
    })
}

// ============================================================================
// Case transform expansion
// ============================================================================

/// Supported case transform modifiers for capture group references.
/// Usage in replacement templates: `$1:kebab`, `$2:pascal`, `${name}:snake`, etc.
const CASE_TRANSFORM_PATTERN: &str = r"\$(?:(\d+)|([a-zA-Z_]\w*)|\{([a-zA-Z_]\w*)\}):(\w+)";

/// Check if a replacement template contains case transform modifiers.
fn has_case_transforms(replace: &str) -> bool {
    lazy_static_regex(CASE_TRANSFORM_PATTERN).is_match(replace)
}

/// Lazy-compile a regex (avoids recompilation per call).
fn lazy_static_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("internal regex should be valid")
}

/// Apply case transform to a string.
fn apply_case_transform(input: &str, transform: &str) -> Option<String> {
    match transform {
        "lower" => Some(input.to_lowercase()),
        "upper" => Some(input.to_uppercase()),
        "kebab" => Some(to_kebab_case(input)),
        "snake" => Some(to_snake_case(input)),
        "pascal" => Some(to_pascal_case(input)),
        "camel" => Some(to_camel_case(input)),
        _ => None,
    }
}

/// Expand a replacement template with case transforms using regex captures.
///
/// Handles `$1:kebab`, `$2:upper`, `${name}:snake`, etc.
/// Also handles standard `$1`, `$$` (literal $), and `${name}` via the regex crate.
///
/// Strategy: first expand case-transformed refs manually, then let regex crate
/// handle the remaining standard refs.
fn expand_with_case_transforms(template: &str, caps: &regex::Captures) -> String {
    let case_re = lazy_static_regex(CASE_TRANSFORM_PATTERN);

    // First pass: replace $N:transform, $name:transform, and ${name}:transform with expanded values
    let intermediate = case_re
        .replace_all(template, |m: &regex::Captures| {
            // Group 1 = numeric ($1:kebab), Group 2 = bare name ($name:kebab),
            // Group 3 = braced name (${name}:kebab), Group 4 = transform
            let transform = &m[4];

            let value = if let Some(num) = m.get(1) {
                let idx: usize = num.as_str().parse().unwrap_or(0);
                caps.get(idx).map(|c| c.as_str().to_string())
            } else if let Some(name) = m.get(2) {
                caps.name(name.as_str()).map(|c| c.as_str().to_string())
            } else if let Some(name) = m.get(3) {
                caps.name(name.as_str()).map(|c| c.as_str().to_string())
            } else {
                None
            };

            match value {
                Some(val) => apply_case_transform(&val, transform).unwrap_or(val),
                None => String::new(),
            }
        })
        .to_string();

    // Second pass: let the regex crate handle remaining standard refs ($1, $$, ${name})
    // We need to expand these manually since we already consumed the Captures
    expand_standard_refs(&intermediate, caps)
}

/// Expand standard capture group references ($1, $2, ${name}, $$) that weren't
/// consumed by case transforms.
fn expand_standard_refs(template: &str, caps: &regex::Captures) -> String {
    let ref_re = lazy_static_regex(r"\$\$|\$(\d+)|\$\{([a-zA-Z_]\w*)\}");

    ref_re
        .replace_all(template, |m: &regex::Captures| {
            let full = m.get(0).unwrap().as_str();
            if full == "$$" {
                return "$".to_string();
            }
            if let Some(num) = m.get(1) {
                let idx: usize = num.as_str().parse().unwrap_or(0);
                return caps
                    .get(idx)
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default();
            }
            if let Some(name) = m.get(2) {
                return caps
                    .name(name.as_str())
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default();
            }
            String::new()
        })
        .to_string()
}

// ============================================================================
// Case conversion helpers
// ============================================================================

/// Split a string into words by camelCase/PascalCase boundaries, underscores,
/// hyphens, and spaces.
fn split_into_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = input.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];

        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
            continue;
        }

        // Split on camelCase boundary: lowercase followed by uppercase
        if c.is_uppercase() && !current.is_empty() {
            let last = current.chars().last().unwrap();
            if last.is_lowercase() || last.is_ascii_digit() {
                words.push(current.clone());
                current.clear();
            }
            // Also split on ABCDef → ABC, Def (uppercase run followed by uppercase+lowercase)
            else if last.is_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_lowercase()
                && current.len() > 1
            {
                let last_char = current.pop().unwrap();
                if !current.is_empty() {
                    words.push(current.clone());
                }
                current.clear();
                current.push(last_char);
            }
        }

        current.push(c);
    }

    if !current.is_empty() {
        words.push(current);
    }

    words
}

fn to_kebab_case(input: &str) -> String {
    split_into_words(input)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

fn to_snake_case(input: &str) -> String {
    split_into_words(input)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

fn to_pascal_case(input: &str) -> String {
    split_into_words(input)
        .iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect()
}

fn to_camel_case(input: &str) -> String {
    let pascal = to_pascal_case(input);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(c) => {
            let lower: String = c.to_lowercase().collect();
            lower + chars.as_str()
        }
        None => String::new(),
    }
}

// ============================================================================
// Context-specific application
// ============================================================================

/// Apply regex per line. Returns (new_content, matches).
fn apply_line_context(
    regex: &Regex,
    replace: &str,
    content: &str,
    relative_path: &str,
) -> (String, Vec<TransformMatch>) {
    let mut matches = Vec::new();
    let mut new_lines = Vec::new();
    let use_case_transforms = has_case_transforms(replace);

    for (i, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            let replaced = if use_case_transforms {
                replace_with_case_transforms(regex, replace, line)
            } else {
                regex.replace_all(line, replace).to_string()
            };
            if replaced != line {
                matches.push(TransformMatch {
                    file: relative_path.to_string(),
                    line: i + 1,
                    before: line.to_string(),
                    after: replaced.clone(),
                });
                new_lines.push(replaced);
                continue;
            }
        }
        new_lines.push(line.to_string());
    }

    // Preserve trailing newline
    let mut result = new_lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }

    (result, matches)
}

/// Apply regex to entire file content. Returns (new_content, matches).
fn apply_file_context(
    regex: &Regex,
    replace: &str,
    content: &str,
    relative_path: &str,
) -> (String, Vec<TransformMatch>) {
    let mut matches = Vec::new();
    let use_case_transforms = has_case_transforms(replace);

    // Find all matches before replacing (for reporting)
    for cap in regex.captures_iter(content) {
        let full_match = cap.get(0).unwrap();
        let before_text = &content[..full_match.start()];
        let line_num = before_text.chars().filter(|&c| c == '\n').count() + 1;
        let matched = full_match.as_str().to_string();
        let replaced = if use_case_transforms {
            expand_with_case_transforms(replace, &cap)
        } else {
            regex.replace(full_match.as_str(), replace).to_string()
        };

        if matched != replaced {
            matches.push(TransformMatch {
                file: relative_path.to_string(),
                line: line_num,
                before: matched,
                after: replaced,
            });
        }
    }

    let new_content = if use_case_transforms {
        replace_with_case_transforms(regex, replace, content)
    } else {
        regex.replace_all(content, replace).to_string()
    };
    (new_content, matches)
}

/// Hoist local `let` bindings to `static` declarations using `LazyLock`.
///
/// Context `"hoist_static"`: the `find` regex must capture two groups:
///   1. The variable name (e.g., `re`)
///   2. The initializer expression (e.g., `Regex::new(r"\d+").unwrap()`)
///
/// The `replace` template receives:
///   - `$1` → SCREAMING_SNAKE version of the variable name
///   - `$2` → the original initializer expression
///
/// After replacing the declaration line, all references to the old variable
/// name within the same function scope are renamed to the new static name.
///
/// Example with ad-hoc:
/// ```text
/// --find 'let\s+(mut\s+)?(\w+)\s*=\s*((?:regex::)?Regex::new\(r.*?\)\.unwrap\(\));'
/// --replace 'static $1: std::sync::LazyLock<regex::Regex> =\n        std::sync::LazyLock::new(|| $2);'
/// --context hoist_static
/// ```
fn apply_hoist_static_context(
    regex: &Regex,
    replace: &str,
    content: &str,
    relative_path: &str,
) -> (String, Vec<TransformMatch>) {
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let mut matches = Vec::new();

    // Collect all match sites first (line index, captures)
    let mut match_sites: Vec<(usize, String, String, String)> = Vec::new(); // (line_idx, old_var, new_var, old_line)

    // Pre-scan to find #[cfg(test)] boundary — skip test code
    let test_mod_start = lines.iter().position(|l| l.trim() == "#[cfg(test)]");

    for (i, line) in lines.iter().enumerate() {
        // Skip matches inside #[cfg(test)] modules
        if let Some(test_start) = test_mod_start {
            if i >= test_start {
                continue;
            }
        }

        if let Some(caps) = regex.captures(line) {
            // Find the variable name — try capture groups in order.
            // The regex may have optional groups (e.g., `(mut\s+)?(\w+)`).
            // We want the first non-empty capture that looks like a variable name.
            let mut var_name = None;
            let mut init_expr = None;
            for g in 1..=caps.len().saturating_sub(1) {
                if let Some(m) = caps.get(g) {
                    let text = m.as_str().trim();
                    if text.is_empty() || text.starts_with("mut") {
                        continue;
                    }
                    if var_name.is_none()
                        && text.len() < 50
                        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        var_name = Some(text.to_string());
                    } else if init_expr.is_none() {
                        init_expr = Some(text.to_string());
                    }
                }
            }

            let var = match var_name {
                Some(v) => v,
                None => continue,
            };

            // Convert to SCREAMING_SNAKE_CASE
            let screaming = to_snake_case(&var).to_uppercase();

            // Build the replacement line using the template.
            // $1 → screaming name, $2 → initializer
            let indent = &line[..line.len() - line.trim_start().len()];
            let replaced = replace
                .replace("$1", &screaming)
                .replace("$2", init_expr.as_deref().unwrap_or(""))
                .split('\n')
                .enumerate()
                .map(|(j, part)| {
                    if j == 0 {
                        format!("{}{}", indent, part)
                    } else {
                        format!("{}{}", indent, part)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            matches.push(TransformMatch {
                file: relative_path.to_string(),
                line: i + 1,
                before: line.to_string(),
                after: replaced.clone(),
            });

            new_lines[i] = replaced;
            match_sites.push((i, var, screaming, line.to_string()));
        }
    }

    // For each match, rename the old variable to the new static name
    // within the enclosing function scope.
    for (match_line, old_var, new_var, _) in &match_sites {
        if old_var == new_var {
            continue;
        }

        // Find function boundaries: scan backward for fn declaration,
        // forward for closing brace at the same depth.
        let fn_start = find_enclosing_fn_start(&new_lines, *match_line);
        let fn_end = find_enclosing_fn_end(&new_lines, fn_start.unwrap_or(0));

        let start = fn_start.unwrap_or(0);
        let end = fn_end.unwrap_or(new_lines.len());

        // Build a word-boundary regex for the old variable name
        let var_re = match Regex::new(&format!(r"\b{}\b", regex::escape(old_var))) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Rename all references within the function scope (skip the declaration line itself)
        for i in start..end {
            if i == *match_line {
                continue; // Already replaced
            }
            if var_re.is_match(&new_lines[i]) {
                let renamed = var_re.replace_all(&new_lines[i], new_var.as_str());
                if renamed != new_lines[i] {
                    matches.push(TransformMatch {
                        file: relative_path.to_string(),
                        line: i + 1,
                        before: new_lines[i].clone(),
                        after: renamed.to_string(),
                    });
                    new_lines[i] = renamed.to_string();
                }
            }
        }
    }

    let mut result = new_lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }

    (result, matches)
}

/// Find the line index of the enclosing `fn` declaration.
fn find_enclosing_fn_start(lines: &[String], from: usize) -> Option<usize> {
    static FN_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+\w+").unwrap()
    });
    (0..=from).rev().find(|&i| FN_RE.is_match(&lines[i]))
}

/// Find the closing brace of a function starting at `fn_line`.
fn find_enclosing_fn_end(lines: &[String], fn_line: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut found_open = false;
    for i in fn_line..lines.len() {
        for ch in lines[i].chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
                if found_open && depth == 0 {
                    return Some(i + 1);
                }
            }
        }
    }
    None
}

/// Replace all matches using case-transform-aware expansion.
fn replace_with_case_transforms(regex: &Regex, replace: &str, text: &str) -> String {
    regex
        .replace_all(text, |caps: &regex::Captures| {
            expand_with_case_transforms(replace, caps)
        })
        .to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- Replacement unescape tests (regression for #1277) ---

    #[test]
    fn unescape_collapses_double_backslash_to_one() {
        // User wrote `\\X` in JSON source → `\\X` (2 bs) after JSON decode.
        // In Rust source, `"\\\\X"` is that same 2-bs-then-X string.
        // After unescape, we want 1 bs + X — ready to emit as a literal on disk.
        assert_eq!(unescape_replacement_template("\\\\X"), "\\X");
    }

    #[test]
    fn unescape_preserves_single_literal_backslash_escape_in_php_namespace() {
        // Reproducer from #1277: ternary_registry_guard rule.
        // JSON source:        "\\\\WP_Abilities_Registry::get_instance()"
        // In-memory (Rust):   "\\\\WP_Abilities_Registry::get_instance()" — 2 bs
        // Want on disk:       "\\WP_Abilities_Registry::get_instance()"   — 1 bs
        let input = "\\\\WP_Abilities_Registry::get_instance()";
        let expected = "\\WP_Abilities_Registry::get_instance()";
        assert_eq!(unescape_replacement_template(input), expected);
    }

    #[test]
    fn unescape_handles_control_sequences() {
        assert_eq!(unescape_replacement_template("a\\nb"), "a\nb");
        assert_eq!(unescape_replacement_template("a\\tb"), "a\tb");
        assert_eq!(unescape_replacement_template("a\\rb"), "a\rb");
        assert_eq!(unescape_replacement_template("a\\0b"), "a\0b");
    }

    #[test]
    fn unescape_handles_quote_escapes() {
        assert_eq!(unescape_replacement_template("\\\""), "\"");
        assert_eq!(unescape_replacement_template("\\'"), "'");
    }

    #[test]
    fn unescape_leaves_dollar_captures_alone() {
        // `$` syntax is the regex crate's territory; we don't touch it.
        assert_eq!(unescape_replacement_template("$1"), "$1");
        assert_eq!(unescape_replacement_template("$$"), "$$");
        assert_eq!(unescape_replacement_template("${name}"), "${name}");
    }

    #[test]
    fn unescape_preserves_unknown_escape_sequences() {
        // Unknown \X should pass through unchanged — no silent character loss.
        assert_eq!(unescape_replacement_template("\\q"), "\\q");
        assert_eq!(unescape_replacement_template("\\!"), "\\!");
    }

    #[test]
    fn unescape_handles_trailing_backslash() {
        assert_eq!(unescape_replacement_template("abc\\"), "abc\\");
    }

    #[test]
    fn unescape_is_idempotent_over_single_backslash_runs() {
        // Triple-backslash in JSON (`\\\`) is invalid JSON, so we never see it.
        // But `\\\\\\\\` in JSON = 4 bs in memory → should collapse to 2 bs.
        assert_eq!(unescape_replacement_template("\\\\\\\\"), "\\\\");
    }

    #[test]
    fn end_to_end_php_namespace_ternary_replacement() {
        // Full reproducer from #1277: replacing a class_exists ternary with
        // a direct \\WP_Abilities_Registry::get_instance() call. Before this
        // fix, 2 bs in memory → 2 bs on disk (PHP parse error). Now: 2 bs in
        // memory → 1 bs on disk (valid PHP FQN).
        let find = r"class_exists\( 'WP_Abilities_Registry' \) \? \\WP_Abilities_Registry::get_instance\(\) : null";
        // Simulates the in-memory value after JSON decode of `"\\\\WP_Abilities_Registry::get_instance()"`
        let replace_in_memory = "\\\\WP_Abilities_Registry::get_instance()";
        let content =
            "$r = class_exists( 'WP_Abilities_Registry' ) ? \\WP_Abilities_Registry::get_instance() : null;";

        let regex = Regex::new(find).unwrap();
        let unescaped = unescape_replacement_template(replace_in_memory);
        let (out, matches) = apply_line_context(&regex, &unescaped, content, "t.php");

        assert_eq!(matches.len(), 1);
        // One literal backslash before WP_ — the PHP FQN the user intended.
        assert!(
            out.contains("= \\WP_Abilities_Registry::get_instance()"),
            "expected single backslash PHP FQN, got: {out:?}"
        );
        assert!(
            !out.contains("\\\\WP_Abilities_Registry"),
            "should not emit double backslashes: {out:?}"
        );
    }

    // --- Rule model tests ---

    #[test]
    fn deserialize_transform_set() {
        let json = r#"{
            "description": "Test migration",
            "rules": [
                {
                    "id": "fix_code",
                    "find": "old_function",
                    "replace": "new_function",
                    "files": "**/*.php"
                }
            ]
        }"#;
        let set: TransformSet = serde_json::from_str(json).unwrap();
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "fix_code");
        assert_eq!(set.rules[0].context, "line"); // default
    }

    #[test]
    fn deserialize_rule_defaults() {
        let json = r#"{"id": "x", "find": "a", "replace": "b"}"#;
        let rule: TransformRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.files, "**/*");
        assert_eq!(rule.context, "line");
        assert_eq!(rule.description, "");
    }

    // --- Line context tests ---

    #[test]
    fn line_context_simple_replace() {
        let regex = Regex::new("rest_forbidden").unwrap();
        let content = "if ($code === 'rest_forbidden') {\n    return false;\n}\n";
        let (new, matches) =
            apply_line_context(&regex, "ability_invalid_permissions", content, "test.php");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].before, "if ($code === 'rest_forbidden') {");
        assert_eq!(
            matches[0].after,
            "if ($code === 'ability_invalid_permissions') {"
        );
        assert!(new.contains("ability_invalid_permissions"));
        assert!(!new.contains("rest_forbidden"));
    }

    #[test]
    fn line_context_with_capture_groups() {
        let regex = Regex::new(r"\$this->assertIsArray\((.+?)\)").unwrap();
        let content = "$this->assertIsArray($result);\n$this->assertIsArray($other);\n";
        let (new, matches) = apply_line_context(
            &regex,
            "$$this->assertInstanceOf(WP_Error::class, $1)",
            content,
            "test.php",
        );
        assert_eq!(matches.len(), 2);
        assert!(new.contains("assertInstanceOf(WP_Error::class, $result)"));
        assert!(new.contains("assertInstanceOf(WP_Error::class, $other)"));
    }

    #[test]
    fn line_context_no_match_unchanged() {
        let regex = Regex::new("xyz_not_found").unwrap();
        let content = "some normal code\nmore code\n";
        let (new, matches) = apply_line_context(&regex, "replaced", content, "test.php");
        assert!(matches.is_empty());
        assert_eq!(new, content);
    }

    #[test]
    fn line_context_preserves_trailing_newline() {
        let regex = Regex::new("old").unwrap();
        let content = "old\n";
        let (new, _) = apply_line_context(&regex, "new", content, "f.txt");
        assert!(new.ends_with('\n'));
        assert_eq!(new, "new\n");
    }

    #[test]
    fn line_context_no_trailing_newline() {
        let regex = Regex::new("old").unwrap();
        let content = "old";
        let (new, _) = apply_line_context(&regex, "new", content, "f.txt");
        assert!(!new.ends_with('\n'));
        assert_eq!(new, "new");
    }

    // --- File context tests ---

    #[test]
    fn file_context_multiline_match() {
        let regex = Regex::new(r"(?s)function\s+old_name\(\).*?\}").unwrap();
        let content = "function old_name() {\n    return 1;\n}\n";
        let (new, matches) = apply_file_context(
            &regex,
            "function new_name() {\n    return 2;\n}",
            content,
            "test.php",
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert!(new.contains("new_name"));
    }

    // --- Case transform tests ---

    #[test]
    fn case_transform_kebab() {
        assert_eq!(to_kebab_case("BlueskyDelete"), "bluesky-delete");
        assert_eq!(to_kebab_case("FacebookPost"), "facebook-post");
        assert_eq!(to_kebab_case("simple"), "simple");
    }

    #[test]
    fn case_transform_snake() {
        assert_eq!(to_snake_case("BlueskyDelete"), "bluesky_delete");
        assert_eq!(to_snake_case("camelCase"), "camel_case");
    }

    #[test]
    fn case_transform_pascal() {
        assert_eq!(to_pascal_case("bluesky-delete"), "BlueskyDelete");
        assert_eq!(to_pascal_case("some_snake"), "SomeSnake");
        assert_eq!(to_pascal_case("already"), "Already");
    }

    #[test]
    fn case_transform_camel() {
        assert_eq!(to_camel_case("BlueskyDelete"), "blueskyDelete");
        assert_eq!(to_camel_case("some-kebab"), "someKebab");
    }

    #[test]
    fn case_transform_upper_lower() {
        assert_eq!(
            apply_case_transform("Hello", "upper"),
            Some("HELLO".to_string())
        );
        assert_eq!(
            apply_case_transform("Hello", "lower"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn line_context_with_case_transform() {
        let regex = Regex::new(r"new (\w+)Ability\(\)").unwrap();
        let content = "let x = new BlueskyDeleteAbility();\nlet y = new FacebookPostAbility();\n";
        let (new, matches) = apply_line_context(
            &regex,
            "wp_get_ability('datamachine/$1:kebab')",
            content,
            "test.rs",
        );
        assert_eq!(matches.len(), 2);
        assert!(
            new.contains("wp_get_ability('datamachine/bluesky-delete')"),
            "got: {}",
            new
        );
        assert!(
            new.contains("wp_get_ability('datamachine/facebook-post')"),
            "got: {}",
            new
        );
    }

    #[test]
    fn case_transform_with_literal_dollar() {
        let regex = Regex::new(r"new (\w+)Ability\(\)").unwrap();
        let content = "$ability = new BlueskyDeleteAbility();\n";
        let (new, _) = apply_line_context(
            &regex,
            "$$ability = wp_get_ability('datamachine/$1:kebab')",
            content,
            "test.php",
        );
        assert!(
            new.contains("$ability = wp_get_ability('datamachine/bluesky-delete')"),
            "got: {}",
            new
        );
    }

    #[test]
    fn case_transform_mixed_with_plain_refs() {
        let regex = Regex::new(r"(\w+)::(\w+)").unwrap();
        let content = "BlueskyApi::PostMessage\n";
        let (new, _) = apply_line_context(
            &regex,
            "$1:snake::$2:kebab (was $1::$2)",
            content,
            "test.rs",
        );
        assert!(
            new.contains("bluesky_api::post-message (was BlueskyApi::PostMessage)"),
            "got: {}",
            new
        );
    }

    #[test]
    fn has_case_transforms_detection() {
        assert!(has_case_transforms("$1:kebab"));
        assert!(has_case_transforms("prefix $2:upper suffix"));
        assert!(has_case_transforms("${name}:snake"));
        assert!(!has_case_transforms("$1 plain"));
        assert!(!has_case_transforms("no refs here"));
        // Note: $$1:kebab contains $1:kebab after the literal $$ — detection sees it,
        // but runtime expansion handles $$ → $ correctly before case transforms apply.
        assert!(has_case_transforms("$$1:kebab"));
    }

    // --- Glob matching tests ---

    #[test]
    fn glob_matches_php_test_files() {
        assert!(glob_match("tests/**/*.php", "tests/Unit/FooTest.php"));
        assert!(glob_match("tests/**/*.php", "tests/FooTest.php"));
        assert!(!glob_match("tests/**/*.php", "src/Foo.php"));
    }

    #[test]
    fn glob_matches_all_files() {
        assert!(glob_match("**/*", "any/path/file.rs"));
        assert!(glob_match("**/*.php", "deep/nested/path/file.php"));
    }

    // --- Integration: apply_transforms with temp dir ---

    #[test]
    fn load_transform_set_from_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let homeboy_json = serde_json::json!({
            "transforms": {
                "my_migration": {
                    "description": "Test migration",
                    "rules": [
                        {
                            "id": "rule1",
                            "find": "old",
                            "replace": "new",
                            "files": "**/*.php"
                        }
                    ]
                }
            }
        });

        fs::write(
            root.join("homeboy.json"),
            serde_json::to_string_pretty(&homeboy_json).unwrap(),
        )
        .unwrap();

        let set = load_transform_set(root, "my_migration").unwrap();
        assert_eq!(set.description, "Test migration");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "rule1");
    }

    #[test]
    fn load_transform_set_not_found_lists_available() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let homeboy_json = serde_json::json!({
            "transforms": {
                "exists": {
                    "description": "",
                    "rules": []
                }
            }
        });

        fs::write(
            root.join("homeboy.json"),
            serde_json::to_string_pretty(&homeboy_json).unwrap(),
        )
        .unwrap();

        let err = load_transform_set(root, "not_here").unwrap_err();
        let msg = format!("{:?}", err.details);
        assert!(msg.contains("not_here"));
        assert!(msg.contains("exists"));
    }
}
