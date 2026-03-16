//! Test drift detection — cross-reference test failures with production changes.
//!
//! Parses git diffs to extract structural changes (renamed methods, changed
//! error codes, removed classes), then scans test files for references to the
//! changed symbols. Outputs a drift report showing which tests are likely
//! broken by which production changes.
//!
//! Phase 1: symbol-level cross-reference (method names, class names, strings).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::code_audit::walker::is_test_path;
use crate::error::{Error, Result};
use crate::git;

// ============================================================================
// Models
// ============================================================================

/// A production change that may cause test drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionChange {
    /// Type of change detected.
    pub change_type: ChangeType,
    /// Production file where the change occurred.
    pub file: String,
    /// The old symbol/value (removed/changed from).
    pub old_symbol: String,
    /// The new symbol/value (added/changed to), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_symbol: Option<String>,
    /// Line number in the diff (approximate).
    #[serde(default)]
    pub line: usize,
}

/// Type of production change detected from git diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// Method/function was renamed.
    MethodRename,
    /// Method/function was removed entirely.
    MethodRemoved,
    /// Class/trait was renamed.
    ClassRename,
    /// Class/trait was removed entirely.
    ClassRemoved,
    /// Error code string changed.
    ErrorCodeChange,
    /// Return type annotation changed.
    ReturnTypeChange,
    /// Method signature changed (different parameters).
    SignatureChange,
    /// File was moved/renamed.
    FileMove,
    /// String constant changed.
    StringChange,
}

/// A test file that references a changed symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftedTest {
    /// Test file path.
    pub test_file: String,
    /// Line number where the old symbol is referenced.
    pub line: usize,
    /// The line content.
    pub content: String,
    /// Reference to the production change that caused the drift.
    pub change_index: usize,
}

/// Full drift report.
#[derive(Debug, Clone, Serialize)]
pub struct DriftReport {
    /// Component name.
    pub component: String,
    /// Git ref used as baseline (tag, commit, branch).
    pub since: String,
    /// Production changes detected.
    pub production_changes: Vec<ProductionChange>,
    /// Tests that reference changed symbols.
    pub drifted_tests: Vec<DriftedTest>,
    /// Total unique test files affected.
    pub total_drifted_files: usize,
    /// Total drift references found.
    pub total_drift_references: usize,
    /// Changes that could be auto-fixed with refactor transform.
    pub auto_fixable: usize,
}

// ============================================================================
// Git diff parsing
// ============================================================================

/// Options for drift detection.
pub struct DriftOptions<'a> {
    /// Component root directory.
    pub root: &'a Path,
    /// Git ref to compare against (tag, commit, branch).
    pub since: &'a str,
    /// Glob patterns for production files (non-test).
    pub source_patterns: Vec<String>,
    /// Glob patterns for test files.
    pub test_patterns: Vec<String>,
}

impl<'a> DriftOptions<'a> {
    /// Create options with common defaults for a PHP project.
    pub fn php(root: &'a Path, since: &'a str) -> Self {
        Self {
            root,
            since,
            source_patterns: vec![
                "src/**/*.php".into(),
                "inc/**/*.php".into(),
                "lib/**/*.php".into(),
            ],
            test_patterns: vec!["tests/**/*.php".into()],
        }
    }

    /// Create options with common defaults for a Rust project.
    pub fn rust(root: &'a Path, since: &'a str) -> Self {
        Self {
            root,
            since,
            source_patterns: vec!["src/**/*.rs".into()],
            test_patterns: vec!["tests/**/*.rs".into()],
        }
    }
}

/// Detect test drift by cross-referencing git changes with test files.
pub fn detect_drift(component: &str, opts: &DriftOptions) -> Result<DriftReport> {
    // Step 1: Get changed production files from git diff
    let changed_files = get_changed_files(opts.root, opts.since)?;

    // Filter to production files only (exclude tests)
    let prod_files: Vec<&str> = changed_files
        .iter()
        .filter(|f| !is_test_path(f))
        .map(|s| s.as_str())
        .collect();

    if prod_files.is_empty() {
        return Ok(DriftReport {
            component: component.to_string(),
            since: opts.since.to_string(),
            production_changes: Vec::new(),
            drifted_tests: Vec::new(),
            total_drifted_files: 0,
            total_drift_references: 0,
            auto_fixable: 0,
        });
    }

    // Step 2: Parse diffs to extract structural changes
    let mut changes = Vec::new();
    for file in &prod_files {
        let diff = get_file_diff(opts.root, opts.since, file)?;
        let file_changes = extract_changes_from_diff(file, &diff);
        changes.extend(file_changes);
    }

    // Also detect file renames
    let renames = get_renamed_files(opts.root, opts.since)?;
    for (old, new) in &renames {
        if !is_test_path(old) {
            changes.push(ProductionChange {
                change_type: ChangeType::FileMove,
                file: new.clone(),
                old_symbol: old.clone(),
                new_symbol: Some(new.clone()),
                line: 0,
            });
        }
    }

    // Step 3: Scan test files for references to changed symbols
    let test_files = collect_test_files(opts.root);
    let drifted = find_drift_references(&changes, &test_files, opts.root);

    // Step 4: Build report
    let total_drifted_files = {
        let unique: std::collections::HashSet<&str> =
            drifted.iter().map(|d| d.test_file.as_str()).collect();
        unique.len()
    };
    let total_drift_references = drifted.len();

    let auto_fixable = changes.iter().filter(|c| is_auto_fixable(c)).count();

    Ok(DriftReport {
        component: component.to_string(),
        since: opts.since.to_string(),
        production_changes: changes,
        drifted_tests: drifted,
        total_drifted_files,
        total_drift_references,
        auto_fixable,
    })
}

// ============================================================================
// Git operations
// ============================================================================

/// Get list of changed files between `since` ref and HEAD.
/// Delegates to the core `git::changes::get_files_changed_since` primitive.
fn get_changed_files(root: &Path, since: &str) -> Result<Vec<String>> {
    let root_str = root.to_string_lossy();
    git::get_files_changed_since(&root_str, since)
}

/// Get diff for a specific file.
fn get_file_diff(root: &Path, since: &str, file: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", since, "HEAD", "--", file])
        .current_dir(root)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run git diff for {}: {}", file, e),
                Some("test_drift.git".to_string()),
            )
        })?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get renamed files from git diff.
fn get_renamed_files(root: &Path, since: &str) -> Result<Vec<(String, String)>> {
    let output = Command::new("git")
        .args(["diff", "--diff-filter=R", "--name-status", since, "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to get renamed files: {}", e),
                Some("test_drift.git".to_string()),
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut renames = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 && parts[0].starts_with('R') {
            renames.push((parts[1].to_string(), parts[2].to_string()));
        }
    }

    Ok(renames)
}

// ============================================================================
// Diff parsing — extract structural changes
// ============================================================================

/// Extract production changes from a unified diff.
fn extract_changes_from_diff(file: &str, diff: &str) -> Vec<ProductionChange> {
    let mut changes = Vec::new();

    // Track removed and added method definitions
    let mut removed_methods: Vec<(String, usize)> = Vec::new();
    let mut added_methods: Vec<(String, usize)> = Vec::new();

    // Track removed and added class/trait definitions
    let mut removed_classes: Vec<(String, usize)> = Vec::new();
    let mut added_classes: Vec<(String, usize)> = Vec::new();

    // Track removed and added string literals (for error codes, etc.)
    let mut removed_strings: Vec<(String, usize)> = Vec::new();
    let mut added_strings: Vec<(String, usize)> = Vec::new();

    // PHP patterns
    let method_re = Regex::new(
        r"(?:public|protected|private|static|abstract|final)\s+(?:static\s+)?function\s+(\w+)",
    )
    .unwrap();
    let class_re = Regex::new(r"(?:abstract\s+)?(?:class|trait|interface)\s+(\w+)").unwrap();
    let string_re = Regex::new(r#"'([a-z_]{3,50})'"#).unwrap();

    // Rust patterns
    let rust_fn_re = Regex::new(r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)").unwrap();
    let rust_struct_re =
        Regex::new(r"(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+(\w+)").unwrap();

    let is_rust = file.ends_with(".rs");
    let fn_re = if is_rust { &rust_fn_re } else { &method_re };
    let cls_re = if is_rust { &rust_struct_re } else { &class_re };
    let hunk_re = Regex::new(r"@@ -\d+(?:,\d+)? \+(\d+)").unwrap();

    let mut line_num: usize = 0;

    for line in diff.lines() {
        // Track line numbers from hunk headers
        if line.starts_with("@@") {
            if let Some(cap) = hunk_re.captures(line) {
                line_num = cap[1].parse().unwrap_or(0);
            }
            continue;
        }

        if line.starts_with('-') && !line.starts_with("---") {
            let content = &line[1..];

            // Check for removed method definitions
            if let Some(cap) = fn_re.captures(content) {
                removed_methods.push((cap[1].to_string(), line_num));
            }

            // Check for removed class definitions
            if let Some(cap) = cls_re.captures(content) {
                removed_classes.push((cap[1].to_string(), line_num));
            }

            // Check for removed string constants (error codes, etc.)
            for cap in string_re.captures_iter(content) {
                removed_strings.push((cap[1].to_string(), line_num));
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            let content = &line[1..];

            // Check for added method definitions
            if let Some(cap) = fn_re.captures(content) {
                added_methods.push((cap[1].to_string(), line_num));
            }

            // Check for added class definitions
            if let Some(cap) = cls_re.captures(content) {
                added_classes.push((cap[1].to_string(), line_num));
            }

            // Check for added string constants
            for cap in string_re.captures_iter(content) {
                added_strings.push((cap[1].to_string(), line_num));
            }

            line_num += 1;
        } else if !line.starts_with('\\') {
            line_num += 1;
        }
    }

    // Match removed methods to added methods (renames)
    let mut matched_removed: Vec<bool> = vec![false; removed_methods.len()];
    let mut matched_added: Vec<bool> = vec![false; added_methods.len()];

    for (ri, (removed, rline)) in removed_methods.iter().enumerate() {
        // Look for a close-by addition (same hunk, ≤10 lines apart)
        for (ai, (added, aline)) in added_methods.iter().enumerate() {
            if !matched_added[ai] && removed != added {
                let dist = (*aline as isize - *rline as isize).unsigned_abs();
                if dist <= 10 {
                    changes.push(ProductionChange {
                        change_type: ChangeType::MethodRename,
                        file: file.to_string(),
                        old_symbol: removed.clone(),
                        new_symbol: Some(added.clone()),
                        line: *rline,
                    });
                    matched_removed[ri] = true;
                    matched_added[ai] = true;
                    break;
                }
            }
        }
    }

    // Unmatched removals are pure removals
    for (ri, (removed, rline)) in removed_methods.iter().enumerate() {
        if !matched_removed[ri] {
            changes.push(ProductionChange {
                change_type: ChangeType::MethodRemoved,
                file: file.to_string(),
                old_symbol: removed.clone(),
                new_symbol: None,
                line: *rline,
            });
        }
    }

    // Match removed classes to added classes (renames)
    let mut cls_matched_removed: Vec<bool> = vec![false; removed_classes.len()];
    let mut cls_matched_added: Vec<bool> = vec![false; added_classes.len()];

    for (ri, (removed, rline)) in removed_classes.iter().enumerate() {
        for (ai, (added, aline)) in added_classes.iter().enumerate() {
            if !cls_matched_added[ai] && removed != added {
                let dist = (*aline as isize - *rline as isize).unsigned_abs();
                if dist <= 15 {
                    changes.push(ProductionChange {
                        change_type: ChangeType::ClassRename,
                        file: file.to_string(),
                        old_symbol: removed.clone(),
                        new_symbol: Some(added.clone()),
                        line: *rline,
                    });
                    cls_matched_removed[ri] = true;
                    cls_matched_added[ai] = true;
                    break;
                }
            }
        }
    }

    for (ri, (removed, rline)) in removed_classes.iter().enumerate() {
        if !cls_matched_removed[ri] {
            changes.push(ProductionChange {
                change_type: ChangeType::ClassRemoved,
                file: file.to_string(),
                old_symbol: removed.clone(),
                new_symbol: None,
                line: *rline,
            });
        }
    }

    // Match removed strings to added strings (error code changes, etc.)
    let mut str_matched_removed: Vec<bool> = vec![false; removed_strings.len()];

    for (ri, (removed, rline)) in removed_strings.iter().enumerate() {
        for (added, aline) in &added_strings {
            if removed != added {
                let dist = (*aline as isize - *rline as isize).unsigned_abs();
                if dist <= 5 {
                    changes.push(ProductionChange {
                        change_type: ChangeType::ErrorCodeChange,
                        file: file.to_string(),
                        old_symbol: removed.clone(),
                        new_symbol: Some(added.clone()),
                        line: *rline,
                    });
                    str_matched_removed[ri] = true;
                    break;
                }
            }
        }
    }

    changes
}

// ============================================================================
// Test file scanning
// ============================================================================

/// Collect all test files in the repo.
fn collect_test_files(root: &Path) -> Vec<PathBuf> {
    let tests_dir = root.join("tests");
    if !tests_dir.exists() {
        return Vec::new();
    }

    collect_files_recursive(&tests_dir)
}

fn collect_files_recursive(dir: &Path) -> Vec<PathBuf> {
    use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

    let config = ScanConfig {
        extensions: ExtensionFilter::All,
        ..Default::default()
    };
    codebase_scan::walk_files(dir, &config)
}

/// Scan test files for references to changed production symbols.
fn find_drift_references(
    changes: &[ProductionChange],
    test_files: &[PathBuf],
    root: &Path,
) -> Vec<DriftedTest> {
    let mut drifted = Vec::new();

    for (ci, change) in changes.iter().enumerate() {
        // Skip changes with very short symbols (likely false positives)
        if change.old_symbol.len() < 3 {
            continue;
        }

        // Build search pattern for the old symbol
        let search = &change.old_symbol;

        for test_file in test_files {
            let Ok(content) = std::fs::read_to_string(test_file) else {
                continue;
            };

            let relative = test_file
                .strip_prefix(root)
                .unwrap_or(test_file)
                .to_string_lossy()
                .to_string();

            for (i, line) in content.lines().enumerate() {
                if line.contains(search) {
                    // Skip if it's a comment-only line
                    let trimmed = line.trim();
                    if trimmed.starts_with("//")
                        || trimmed.starts_with('#')
                        || trimmed.starts_with('*')
                        || trimmed.starts_with("/*")
                    {
                        continue;
                    }

                    drifted.push(DriftedTest {
                        test_file: relative.clone(),
                        line: i + 1,
                        content: line.trim().to_string(),
                        change_index: ci,
                    });
                }
            }
        }
    }

    drifted
}

/// Check if a change type is auto-fixable with refactor transform.
fn is_auto_fixable(change: &ProductionChange) -> bool {
    match change.change_type {
        ChangeType::MethodRename => change.new_symbol.is_some(),
        ChangeType::ClassRename => change.new_symbol.is_some(),
        ChangeType::ErrorCodeChange => change.new_symbol.is_some(),
        ChangeType::FileMove => change.new_symbol.is_some(),
        ChangeType::StringChange => change.new_symbol.is_some(),
        ChangeType::MethodRemoved => false,
        ChangeType::ClassRemoved => false,
        ChangeType::ReturnTypeChange => false,
        ChangeType::SignatureChange => false,
    }
}

/// Generate transform rules from a drift report.
///
/// For each auto-fixable change, creates a TransformRule that replaces
/// the old symbol with the new one in test files.
pub fn generate_transform_rules(report: &DriftReport) -> Vec<crate::refactor::TransformRule> {
    let mut rules = Vec::new();

    for change in &report.production_changes {
        if !is_auto_fixable(change) {
            continue;
        }

        let new_symbol = match &change.new_symbol {
            Some(s) => s,
            None => continue,
        };

        let id = format!("{:?}_{}", change.change_type, change.old_symbol)
            .to_lowercase()
            .replace(' ', "_");

        let description = match change.change_type {
            ChangeType::MethodRename => {
                format!(
                    "Rename {} → {} ({})",
                    change.old_symbol, new_symbol, change.file
                )
            }
            ChangeType::ClassRename => {
                format!(
                    "Rename class {} → {} ({})",
                    change.old_symbol, new_symbol, change.file
                )
            }
            ChangeType::ErrorCodeChange => {
                format!(
                    "Error code {} → {} ({})",
                    change.old_symbol, new_symbol, change.file
                )
            }
            ChangeType::FileMove => {
                format!("File moved {} → {}", change.old_symbol, new_symbol)
            }
            _ => format!("{} → {} ({})", change.old_symbol, new_symbol, change.file),
        };

        let (find, replace) = match change.change_type {
            // Identifier renames should use word boundaries to avoid partial matches.
            ChangeType::MethodRename | ChangeType::ClassRename => {
                if !is_identifier(&change.old_symbol) || !is_identifier(new_symbol) {
                    continue;
                }
                (
                    format!(r"\b{}\b", regex::escape(&change.old_symbol)),
                    new_symbol.clone(),
                )
            }
            // Error/string changes are prone to false positives when symbols are too generic
            // (e.g. old="name", new="assistant"). Only auto-fix token-like literals.
            ChangeType::ErrorCodeChange | ChangeType::StringChange => {
                if !is_safe_literal_token(&change.old_symbol) || !is_safe_literal_token(new_symbol)
                {
                    continue;
                }
                (regex::escape(&change.old_symbol), new_symbol.clone())
            }
            // File moves should look like path-ish values.
            ChangeType::FileMove => {
                if !looks_like_path(&change.old_symbol) || !looks_like_path(new_symbol) {
                    continue;
                }
                (regex::escape(&change.old_symbol), new_symbol.clone())
            }
            _ => continue,
        };

        rules.push(crate::refactor::TransformRule {
            id,
            description,
            find,
            replace,
            files: "tests/**/*".to_string(),
            context: "line".to_string(),
        });
    }

    rules
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_safe_literal_token(s: &str) -> bool {
    if s.len() < 4 {
        return false;
    }

    // Path-ish and namespaced/string tokens are usually safe for literal replacement.
    if s.contains('/') || s.contains('\\') || s.contains(':') || s.contains('.') || s.contains('-')
    {
        return true;
    }

    // For underscore tokens, constrain to error-code-like vocabulary.
    // This avoids unsafe schema/field rewrites (e.g. flow_id -> pipeline_id).
    if s.contains('_') {
        let lower = s.to_ascii_lowercase();
        return lower.starts_with("rest_")
            || lower.starts_with("ability_")
            || lower.contains("error")
            || lower.contains("invalid")
            || lower.contains("forbidden")
            || lower.contains("denied")
            || lower.contains("not_found")
            || lower.contains("failed");
    }

    false
}

fn looks_like_path(s: &str) -> bool {
    s.len() >= 5 && (s.contains('/') || s.contains('\\'))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_method_rename() {
        let diff = r#"@@ -10,7 +10,7 @@
-    public function executeRunFlow($id) {
+    public function executeWorkflow($id) {
         return $this->doWork($id);
     }
"#;
        let changes = extract_changes_from_diff("src/Abilities/JobAbilities.php", diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, ChangeType::MethodRename);
        assert_eq!(changes[0].old_symbol, "executeRunFlow");
        assert_eq!(changes[0].new_symbol.as_deref(), Some("executeWorkflow"));
    }

    #[test]
    fn extract_method_removed() {
        let diff = r#"@@ -20,5 +20,0 @@
-    public function oldHelper() {
-        return true;
-    }
"#;
        let changes = extract_changes_from_diff("src/Helper.php", diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, ChangeType::MethodRemoved);
        assert_eq!(changes[0].old_symbol, "oldHelper");
        assert!(changes[0].new_symbol.is_none());
    }

    #[test]
    fn extract_error_code_change() {
        let diff = r#"@@ -5,7 +5,7 @@
-        return new WP_Error('rest_forbidden', 'Access denied');
+        return new WP_Error('ability_invalid_permissions', 'Access denied');
"#;
        let changes = extract_changes_from_diff("src/REST/Auth.php", diff);

        let code_changes: Vec<_> = changes
            .iter()
            .filter(|c| c.change_type == ChangeType::ErrorCodeChange)
            .collect();

        assert!(!code_changes.is_empty());
        assert_eq!(code_changes[0].old_symbol, "rest_forbidden");
        assert_eq!(
            code_changes[0].new_symbol.as_deref(),
            Some("ability_invalid_permissions")
        );
    }

    #[test]
    fn extract_class_rename() {
        let diff = r#"@@ -1,5 +1,5 @@
-class FlowsCommand extends BaseCommand {
+class FlowCommand extends BaseCommand {
     public function handle() {
"#;
        let changes = extract_changes_from_diff("src/Commands/FlowsCommand.php", diff);

        let cls = changes
            .iter()
            .find(|c| c.change_type == ChangeType::ClassRename)
            .unwrap();
        assert_eq!(cls.old_symbol, "FlowsCommand");
        assert_eq!(cls.new_symbol.as_deref(), Some("FlowCommand"));
    }

    #[test]
    fn extract_rust_fn_rename() {
        let diff = r#"@@ -10,7 +10,7 @@
-pub fn load_config(path: &Path) -> Config {
+pub fn read_config(path: &Path) -> Config {
     let data = fs::read_to_string(path).unwrap();
"#;
        let changes = extract_changes_from_diff("src/config.rs", diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, ChangeType::MethodRename);
        assert_eq!(changes[0].old_symbol, "load_config");
        assert_eq!(changes[0].new_symbol.as_deref(), Some("read_config"));
    }

    #[test]
    fn is_test_path_detection() {
        assert!(is_test_path("tests/Unit/FooTest.php"));
        assert!(is_test_path("tests/integration/bar_test.rs"));
        assert!(!is_test_path("src/Foo.php"));
        assert!(!is_test_path("src/config.rs"));
    }

    #[test]
    fn auto_fixable_detection() {
        let rename = ProductionChange {
            change_type: ChangeType::MethodRename,
            file: "src/Foo.php".into(),
            old_symbol: "oldMethod".into(),
            new_symbol: Some("newMethod".into()),
            line: 10,
        };
        assert!(is_auto_fixable(&rename));

        let removed = ProductionChange {
            change_type: ChangeType::MethodRemoved,
            file: "src/Foo.php".into(),
            old_symbol: "deadMethod".into(),
            new_symbol: None,
            line: 10,
        };
        assert!(!is_auto_fixable(&removed));
    }

    #[test]
    fn generate_rules_from_rename() {
        let report = DriftReport {
            component: "test".into(),
            since: "v1.0".into(),
            production_changes: vec![ProductionChange {
                change_type: ChangeType::MethodRename,
                file: "src/Foo.php".into(),
                old_symbol: "executeRunFlow".into(),
                new_symbol: Some("executeWorkflow".into()),
                line: 10,
            }],
            drifted_tests: Vec::new(),
            total_drifted_files: 0,
            total_drift_references: 0,
            auto_fixable: 1,
        };

        let rules = generate_transform_rules(&report);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].find, r"\bexecuteRunFlow\b");
        assert_eq!(rules[0].replace, "executeWorkflow");
        assert_eq!(rules[0].files, "tests/**/*");
    }

    #[test]
    fn generate_rules_skips_unsafe_generic_string_changes() {
        let report = DriftReport {
            component: "test".into(),
            since: "v1.0".into(),
            production_changes: vec![ProductionChange {
                change_type: ChangeType::ErrorCodeChange,
                file: "src/Foo.php".into(),
                old_symbol: "name".into(),
                new_symbol: Some("assistant".into()),
                line: 10,
            }],
            drifted_tests: Vec::new(),
            total_drifted_files: 0,
            total_drift_references: 0,
            auto_fixable: 1,
        };

        let rules = generate_transform_rules(&report);
        assert!(rules.is_empty());
    }

    #[test]
    fn generate_rules_keeps_safe_error_code_tokens() {
        let report = DriftReport {
            component: "test".into(),
            since: "v1.0".into(),
            production_changes: vec![ProductionChange {
                change_type: ChangeType::ErrorCodeChange,
                file: "src/Foo.php".into(),
                old_symbol: "rest_forbidden".into(),
                new_symbol: Some("ability_invalid_permissions".into()),
                line: 10,
            }],
            drifted_tests: Vec::new(),
            total_drifted_files: 0,
            total_drift_references: 0,
            auto_fixable: 1,
        };

        let rules = generate_transform_rules(&report);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].find, "rest_forbidden");
        assert_eq!(rules[0].replace, "ability_invalid_permissions");
    }

    #[test]
    fn generate_rules_skips_non_error_underscore_tokens() {
        let report = DriftReport {
            component: "test".into(),
            since: "v1.0".into(),
            production_changes: vec![ProductionChange {
                change_type: ChangeType::ErrorCodeChange,
                file: "src/Foo.php".into(),
                old_symbol: "flow_id".into(),
                new_symbol: Some("pipeline_id".into()),
                line: 10,
            }],
            drifted_tests: Vec::new(),
            total_drifted_files: 0,
            total_drift_references: 0,
            auto_fixable: 1,
        };

        let rules = generate_transform_rules(&report);
        assert!(rules.is_empty());
    }

    #[test]
    fn skip_short_symbols() {
        let changes = vec![ProductionChange {
            change_type: ChangeType::MethodRename,
            file: "src/X.php".into(),
            old_symbol: "ab".into(), // too short
            new_symbol: Some("cd".into()),
            line: 1,
        }];

        let test_content = "line with ab in it\n";
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("FooTest.php"), test_content).unwrap();

        let test_files = vec![tests_dir.join("FooTest.php")];
        let drifted = find_drift_references(&changes, &test_files, root);
        assert!(drifted.is_empty()); // Skipped because symbol < 3 chars
    }

    #[test]
    fn find_references_in_test_files() {
        let changes = vec![ProductionChange {
            change_type: ChangeType::MethodRename,
            file: "src/Foo.php".into(),
            old_symbol: "executeRunFlow".into(),
            new_symbol: Some("executeWorkflow".into()),
            line: 10,
        }];

        let test_content = r#"<?php
class FooTest extends TestCase {
    public function testRunFlow() {
        $result = $this->foo->executeRunFlow(1);
        $this->assertNotNull($result);
    }
}
"#;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("FooTest.php"), test_content).unwrap();

        let test_files = vec![tests_dir.join("FooTest.php")];
        let drifted = find_drift_references(&changes, &test_files, root);
        assert_eq!(drifted.len(), 1);
        assert_eq!(drifted[0].line, 4);
        assert!(drifted[0].content.contains("executeRunFlow"));
    }

    #[test]
    fn multiple_changes_multiple_tests() {
        let changes = vec![
            ProductionChange {
                change_type: ChangeType::MethodRename,
                file: "src/A.php".into(),
                old_symbol: "oldMethodA".into(),
                new_symbol: Some("newMethodA".into()),
                line: 5,
            },
            ProductionChange {
                change_type: ChangeType::ErrorCodeChange,
                file: "src/B.php".into(),
                old_symbol: "rest_forbidden".into(),
                new_symbol: Some("access_denied".into()),
                line: 10,
            },
        ];

        let test1 = "<?php\n$this->oldMethodA();\n";
        let test2 = "<?php\nassertEquals('rest_forbidden', $code);\n";

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("ATest.php"), test1).unwrap();
        std::fs::write(tests_dir.join("BTest.php"), test2).unwrap();

        let test_files = vec![tests_dir.join("ATest.php"), tests_dir.join("BTest.php")];
        let drifted = find_drift_references(&changes, &test_files, root);
        assert_eq!(drifted.len(), 2);
    }

    #[test]
    fn skip_comment_lines() {
        let changes = vec![ProductionChange {
            change_type: ChangeType::MethodRename,
            file: "src/Foo.php".into(),
            old_symbol: "oldMethod".into(),
            new_symbol: Some("newMethod".into()),
            line: 5,
        }];

        let test_content =
            "<?php\n// oldMethod was renamed\n/* oldMethod docs */\n$this->oldMethod();\n";

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("FooTest.php"), test_content).unwrap();

        let test_files = vec![tests_dir.join("FooTest.php")];
        let drifted = find_drift_references(&changes, &test_files, root);
        assert_eq!(drifted.len(), 1); // Only the actual code line, not comments
        assert_eq!(drifted[0].line, 4);
    }
}
