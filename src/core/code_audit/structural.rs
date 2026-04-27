//! Structural complexity analysis — detect god files, high item counts,
//! and other structural issues that convention-based analysis can't catch.
//!
//! Plugs into the audit pipeline as an additional findings source.

use std::collections::HashMap;
use std::path::Path;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

/// Thresholds for structural findings.
const GOD_FILE_LINE_THRESHOLD: usize = 1500;
const HIGH_ITEM_COUNT_THRESHOLD: usize = 30;
const DIRECTORY_SPRAWL_FILE_THRESHOLD: usize = 50;

/// Known source file extensions for structural analysis.
/// Matches the walker's known extensions so we analyze the same files.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "php", "js", "ts", "jsx", "tsx", "mjs", "py", "go", "java", "rb", "swift", "kt", "c",
    "cpp", "h",
];

/// Run structural analysis on all source files under a root directory.
///
/// Returns findings for files that exceed structural thresholds.
pub(crate) fn analyze_structure(root: &Path) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(
            SOURCE_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
        ),
        ..Default::default()
    };
    let files = codebase_scan::walk_files(root, &config);

    let mut findings = Vec::new();
    let mut dir_source_counts: HashMap<String, usize> = HashMap::new();

    for path in files {
        let parent_rel = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        *dir_source_counts.entry(parent_rel).or_insert(0) += 1;

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let relative = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());

        // Check line count
        let line_count = content.lines().count();
        if line_count > GOD_FILE_LINE_THRESHOLD {
            let suggestion = "Review whether the file has crossed a real responsibility boundary before extracting focused modules.".to_string();
            findings.push(Finding {
                convention: "structural".to_string(),
                severity: Severity::Warning,
                file: relative.clone(),
                description: format!(
                    "File has {} lines (threshold: {})",
                    line_count, GOD_FILE_LINE_THRESHOLD
                ),
                suggestion,
                kind: AuditFinding::GodFile,
            });
        }

        // Count top-level items (functions, structs, enums, consts, etc.)
        let item_count = count_top_level_items(&content, ext);
        if item_count > HIGH_ITEM_COUNT_THRESHOLD {
            findings.push(Finding {
                convention: "structural".to_string(),
                severity: Severity::Info,
                file: relative,
                description: format!(
                    "File has {} top-level items (threshold: {})",
                    item_count, HIGH_ITEM_COUNT_THRESHOLD
                ),
                suggestion: "Review whether the top-level items represent multiple responsibilities before extracting focused modules".to_string(),
                kind: AuditFinding::HighItemCount,
            });
        }
    }

    for (dir, count) in dir_source_counts {
        if count <= DIRECTORY_SPRAWL_FILE_THRESHOLD {
            continue;
        }

        let dir_label = if dir.is_empty() { ".".to_string() } else { dir };
        findings.push(Finding {
            convention: "structural".to_string(),
            severity: Severity::Info,
            file: dir_label,
            description: format!(
                "Directory has {} source files (threshold: {})",
                count, DIRECTORY_SPRAWL_FILE_THRESHOLD
            ),
            suggestion:
                "Review whether the directory contains multiple discoverable subdomains before adding subdirectories"
                    .to_string(),
            kind: AuditFinding::DirectorySprawl,
        });
    }

    // Sort by file path for deterministic output
    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
}

/// Count top-level items in a source file.
///
/// Uses lightweight pattern matching rather than full parsing — we just need
/// approximate counts for threshold detection, not exact ASTs.
fn count_top_level_items(content: &str, ext: &str) -> usize {
    match ext {
        "rs" => count_rust_items(content),
        "php" => count_php_items(content),
        "js" | "jsx" | "mjs" | "ts" | "tsx" => count_js_items(content),
        _ => 0, // Unknown languages get no item count findings
    }
}

/// Count top-level items in Rust source.
///
/// Matches: fn, struct, enum, const, static, type, trait, impl at zero indentation.
fn count_rust_items(content: &str) -> usize {
    let mut count = 0;
    let mut in_test_module = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip items inside test modules (everything after #[cfg(test)])
        if trimmed == "#[cfg(test)]" {
            in_test_module = true;
            continue;
        }
        if in_test_module {
            continue;
        }

        // Only count items at top level (zero indentation)
        let indent = line.len() - line.trim_start().len();
        if indent > 0 {
            continue;
        }

        if is_rust_item_declaration(trimmed) {
            count += 1;
        }
    }

    count
}

/// Check if a trimmed line starts a Rust item declaration.
fn is_rust_item_declaration(trimmed: &str) -> bool {
    // Strip visibility prefix
    let rest = if let Some(r) = trimmed.strip_prefix("pub(crate) ") {
        r
    } else if let Some(r) = trimmed.strip_prefix("pub(super) ") {
        r
    } else if let Some(r) = trimmed.strip_prefix("pub ") {
        r
    } else {
        trimmed
    };

    // Strip async/unsafe/const modifiers for functions
    let rest = if let Some(r) = rest.strip_prefix("async ") {
        r
    } else {
        rest
    };
    let rest = if let Some(r) = rest.strip_prefix("unsafe ") {
        r
    } else {
        rest
    };

    rest.starts_with("fn ")
        || rest.starts_with("struct ")
        || rest.starts_with("enum ")
        || rest.starts_with("const ")
        || rest.starts_with("static ")
        || rest.starts_with("type ")
        || rest.starts_with("trait ")
        || rest.starts_with("impl ")
        || rest.starts_with("impl<")
}

/// Count top-level items in PHP source.
///
/// Matches: function, class, interface, trait, const at zero indentation.
fn count_php_items(content: &str) -> usize {
    let mut count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if indent > 0 {
            continue;
        }

        // Strip visibility
        let rest = trimmed
            .strip_prefix("public ")
            .or_else(|| trimmed.strip_prefix("protected "))
            .or_else(|| trimmed.strip_prefix("private "))
            .unwrap_or(trimmed);
        let rest = rest
            .strip_prefix("static ")
            .or_else(|| rest.strip_prefix("abstract "))
            .or_else(|| rest.strip_prefix("final "))
            .unwrap_or(rest);

        if rest.starts_with("function ")
            || rest.starts_with("class ")
            || rest.starts_with("interface ")
            || rest.starts_with("trait ")
            || rest.starts_with("const ")
        {
            count += 1;
        }
    }

    count
}

/// Count top-level items in JavaScript/TypeScript source.
///
/// Matches: function, class, const, let, var, export at zero indentation.
fn count_js_items(content: &str) -> usize {
    let mut count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if indent > 0 {
            continue;
        }

        let rest = trimmed
            .strip_prefix("export default ")
            .or_else(|| trimmed.strip_prefix("export "))
            .unwrap_or(trimmed);

        if rest.starts_with("function ")
            || rest.starts_with("class ")
            || rest.starts_with("const ")
            || rest.starts_with("let ")
            || rest.starts_with("var ")
            || rest.starts_with("interface ")
            || rest.starts_with("type ")
            || rest.starts_with("enum ")
        {
            count += 1;
        }
    }

    count
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_rust_items_basic() {
        let content = r#"
use std::path::Path;

pub struct Foo {
    name: String,
}

fn helper() -> bool {
    true
}

pub fn main_logic() {
    // ...
}

impl Foo {
    pub fn new() -> Self {
        Self { name: String::new() }
    }
}

const MAX: usize = 100;

#[cfg(test)]
mod tests {
    fn test_something() {}
    fn test_another() {}
}
"#;
        // Should count: struct Foo, fn helper, pub fn main_logic, impl Foo, const MAX = 5
        // Should NOT count: use, items inside #[cfg(test)]
        let count = count_rust_items(content);
        assert_eq!(count, 5, "Expected 5 top-level items");
    }

    #[test]
    fn count_rust_items_with_visibility() {
        let content = r#"
pub(crate) fn internal() {}
pub struct Public {}
pub(super) const X: i32 = 1;
pub async fn async_handler() {}
"#;
        assert_eq!(count_rust_items(content), 4);
    }

    #[test]
    fn count_php_items_basic() {
        let content = r#"<?php
namespace App\Models;

class User {
    public function getName() {}
    public function getEmail() {}
}

function helper() {}

interface Cacheable {
    public function cache();
}
"#;
        // class User, function helper, interface Cacheable = 3
        // Methods inside class are indented, so not counted
        assert_eq!(count_php_items(content), 3);
    }

    #[test]
    fn count_js_items_basic() {
        let content = r#"
import { foo } from './bar';

export function processData() {}

export class DataProcessor {
    transform() {}
}

const CONFIG = {};

export default function main() {}
"#;
        // export function, export class, const CONFIG, export default function = 4
        assert_eq!(count_js_items(content), 4);
    }

    #[test]
    fn god_file_detected_at_actionable_threshold() {
        let dir = std::env::temp_dir().join("homeboy_structural_god_test");
        let _ = std::fs::create_dir_all(&dir);

        // Create a file above the actionable threshold.
        let mut content = String::new();
        for i in 0..1600 {
            content.push_str(&format!("fn func_{}() {{}}\n", i));
        }
        std::fs::write(dir.join("big.rs"), &content).unwrap();

        // Create a small file (under threshold)
        std::fs::write(dir.join("small.rs"), "fn tiny() {}\n").unwrap();

        let findings = analyze_structure(&dir);
        let god_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::GodFile)
            .collect();

        assert_eq!(god_findings.len(), 1, "Should flag big.rs as god file");
        assert_eq!(god_findings[0].file, "big.rs");
        assert!(god_findings[0].description.contains("1600 lines"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_only_structural_smells_below_actionable_threshold_are_ignored() {
        let dir = std::env::temp_dir().join("homeboy_structural_review_only_test");
        let root = dir.join("src/core");
        let _ = std::fs::create_dir_all(&root);

        let mut large_but_not_actionable = String::new();
        large_but_not_actionable.push_str("fn large() {\n");
        for i in 0..1200 {
            large_but_not_actionable.push_str(&format!("    let line_{} = {};\n", i, i));
        }
        large_but_not_actionable.push_str("}\n");
        std::fs::write(root.join("large.rs"), large_but_not_actionable).unwrap();

        let mut many_but_not_actionable = String::new();
        for i in 0..25 {
            many_but_not_actionable.push_str(&format!("fn item_{}() {{}}\n", i));
        }
        std::fs::write(root.join("many_items.rs"), many_but_not_actionable).unwrap();

        for i in 0..40 {
            std::fs::write(root.join(format!("module_{}.rs", i)), "pub fn run() {}\n").unwrap();
        }

        let findings = analyze_structure(&dir);
        assert!(
            findings.is_empty(),
            "Moderate count-only smells should stay review-only instead of producing audit findings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_non_source_files() {
        let dir = std::env::temp_dir().join("homeboy_structural_skip_test");
        let _ = std::fs::create_dir_all(&dir);

        // A big non-source file should not be flagged
        let mut content = String::new();
        for _ in 0..1000 {
            content.push_str("some data line\n");
        }
        std::fs::write(dir.join("data.csv"), &content).unwrap();
        std::fs::write(dir.join("readme.md"), &content).unwrap();

        let findings = analyze_structure(&dir);
        assert!(
            findings.is_empty(),
            "Non-source files should not produce findings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_vendor_directories() {
        let dir = std::env::temp_dir().join("homeboy_structural_vendor_test");
        let vendor = dir.join("vendor");
        let _ = std::fs::create_dir_all(&vendor);

        let mut content = String::new();
        for i in 0..600 {
            content.push_str(&format!("fn func_{}() {{}}\n", i));
        }
        std::fs::write(vendor.join("big.rs"), &content).unwrap();

        let findings = analyze_structure(&dir);
        assert!(findings.is_empty(), "Files in vendor/ should be skipped");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn under_threshold_no_findings() {
        let dir = std::env::temp_dir().join("homeboy_structural_clean_test");
        let _ = std::fs::create_dir_all(&dir);

        // A reasonable 100-line file with 5 items
        let mut content = String::new();
        for i in 0..5 {
            content.push_str(&format!("/// Doc for func_{}\n", i));
            content.push_str(&format!("pub fn func_{}() {{\n", i));
            for j in 0..15 {
                content.push_str(&format!("    let x{} = {};\n", j, j));
            }
            content.push_str("}\n\n");
        }
        std::fs::write(dir.join("clean.rs"), &content).unwrap();

        let findings = analyze_structure(&dir);
        assert!(
            findings.is_empty(),
            "Clean files should produce no findings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
