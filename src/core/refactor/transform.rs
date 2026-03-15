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
    /// Replacement template (supports `$1`, `$2`, `${name}` capture group refs).
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

fn default_context() -> String {
    "line".to_string()
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
pub fn ad_hoc_transform(find: &str, replace: &str, files: &str) -> TransformSet {
    TransformSet {
        description: "Ad-hoc transform".to_string(),
        rules: vec![TransformRule {
            id: "ad-hoc".to_string(),
            description: String::new(),
            find: find.to_string(),
            replace: replace.to_string(),
            files: files.to_string(),
            context: "line".to_string(),
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
                apply_file_context(regex, &rule.replace, &content, &relative)
            } else {
                apply_line_context(regex, &rule.replace, &content, &relative)
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

    for (i, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            let replaced = regex.replace_all(line, replace).to_string();
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

    // Find all matches before replacing (for reporting)
    for cap in regex.find_iter(content) {
        let before_text = &content[..cap.start()];
        let line_num = before_text.chars().filter(|&c| c == '\n').count() + 1;
        let matched = cap.as_str().to_string();
        let replaced = regex.replace(cap.as_str(), replace).to_string();

        if matched != replaced {
            matches.push(TransformMatch {
                file: relative_path.to_string(),
                line: line_num,
                before: matched,
                after: replaced,
            });
        }
    }

    let new_content = regex.replace_all(content, replace).to_string();
    (new_content, matches)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn apply_transforms_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create test files
        let tests_dir = root.join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("FooTest.php"),
            "<?php\n$this->assertIsArray($result);\n$code = 'rest_forbidden';\n",
        )
        .unwrap();
        fs::write(root.join("src.php"), "<?php\n$code = 'rest_forbidden';\n").unwrap();

        let set = TransformSet {
            description: "test".into(),
            rules: vec![TransformRule {
                id: "fix_code".into(),
                description: "Fix error code".into(),
                find: "rest_forbidden".into(),
                replace: "ability_invalid_permissions".into(),
                files: "tests/**/*.php".into(),
                context: "line".into(),
            }],
        };

        let result = apply_transforms(root, "test", &set, false, None).unwrap();

        // Should match only the test file, not src.php
        assert_eq!(result.total_replacements, 1);
        assert_eq!(result.total_files, 1);
        assert!(!result.written);

        // File should be unchanged (dry-run)
        let content = fs::read_to_string(tests_dir.join("FooTest.php")).unwrap();
        assert!(content.contains("rest_forbidden"));
    }

    #[test]
    fn apply_transforms_write_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let tests_dir = root.join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("FooTest.php"),
            "<?php\n$code = 'rest_forbidden';\n",
        )
        .unwrap();

        let set = TransformSet {
            description: "test".into(),
            rules: vec![TransformRule {
                id: "fix".into(),
                description: String::new(),
                find: "rest_forbidden".into(),
                replace: "ability_invalid_permissions".into(),
                files: "tests/**/*.php".into(),
                context: "line".into(),
            }],
        };

        let result = apply_transforms(root, "test", &set, true, None).unwrap();

        assert_eq!(result.total_replacements, 1);
        assert!(result.written);

        // File should be changed
        let content = fs::read_to_string(tests_dir.join("FooTest.php")).unwrap();
        assert!(content.contains("ability_invalid_permissions"));
        assert!(!content.contains("rest_forbidden"));
    }

    #[test]
    fn apply_transforms_rule_filter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("test.php"), "aaa\nbbb\n").unwrap();

        let set = TransformSet {
            description: "test".into(),
            rules: vec![
                TransformRule {
                    id: "rule_a".into(),
                    description: String::new(),
                    find: "aaa".into(),
                    replace: "AAA".into(),
                    files: "**/*".into(),
                    context: "line".into(),
                },
                TransformRule {
                    id: "rule_b".into(),
                    description: String::new(),
                    find: "bbb".into(),
                    replace: "BBB".into(),
                    files: "**/*".into(),
                    context: "line".into(),
                },
            ],
        };

        // Only apply rule_a
        let result = apply_transforms(root, "test", &set, false, Some("rule_a")).unwrap();
        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0].id, "rule_a");
        assert_eq!(result.total_replacements, 1);
    }

    #[test]
    fn apply_transforms_multiple_rules_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("test.php"), "old_a and old_b\n").unwrap();

        let set = TransformSet {
            description: "test".into(),
            rules: vec![
                TransformRule {
                    id: "a".into(),
                    description: String::new(),
                    find: "old_a".into(),
                    replace: "new_a".into(),
                    files: "**/*".into(),
                    context: "line".into(),
                },
                TransformRule {
                    id: "b".into(),
                    description: String::new(),
                    find: "old_b".into(),
                    replace: "new_b".into(),
                    files: "**/*".into(),
                    context: "line".into(),
                },
            ],
        };

        let result = apply_transforms(root, "test", &set, true, None).unwrap();
        assert_eq!(result.total_replacements, 2);
        assert_eq!(result.total_files, 1); // Same file modified by both rules

        let content = fs::read_to_string(root.join("test.php")).unwrap();
        assert!(content.contains("new_a"));
        assert!(content.contains("new_b"));
        assert!(!content.contains("old_a"));
        assert!(!content.contains("old_b"));
    }

    #[test]
    fn apply_transforms_invalid_regex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let set = TransformSet {
            description: "test".into(),
            rules: vec![TransformRule {
                id: "bad".into(),
                description: String::new(),
                find: "[invalid regex".into(),
                replace: "x".into(),
                files: "**/*".into(),
                context: "line".into(),
            }],
        };

        let result = apply_transforms(root, "test", &set, false, None);
        assert!(result.is_err());
    }

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
