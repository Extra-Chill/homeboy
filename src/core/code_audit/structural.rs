//! Structural complexity analysis — detect god files, high item counts,
//! and other structural issues that convention-based analysis can't catch.
//!
//! Plugs into the audit pipeline as an additional findings source.

use std::collections::HashMap;
use std::path::Path;

use crate::core::component::{grammar_for_extension, LanguageGrammar};
use crate::core::engine::codebase_scan::{CodebaseSnapshot, ExtensionFilter, ScanConfig};

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

pub(crate) fn source_extensions() -> &'static [&'static str] {
    SOURCE_EXTENSIONS
}

pub(crate) fn build_snapshot(root: &Path) -> CodebaseSnapshot {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(
            SOURCE_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
        ),
        ..Default::default()
    };

    CodebaseSnapshot::build(root, &config)
}

/// Run structural analysis on all source files under a root directory.
///
/// Returns findings for files that exceed structural thresholds.
pub(crate) fn analyze_structure(root: &Path, grammars: &[LanguageGrammar]) -> Vec<Finding> {
    let snapshot = build_snapshot(root);
    analyze_snapshot(root, &snapshot, grammars)
}

/// Run structural analysis from an already-loaded codebase snapshot.
pub(crate) fn analyze_snapshot(
    root: &Path,
    snapshot: &CodebaseSnapshot,
    grammars: &[LanguageGrammar],
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut dir_source_counts: HashMap<String, usize> = HashMap::new();

    for (path, content) in snapshot.iter() {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SOURCE_EXTENSIONS.contains(&ext) {
            continue;
        }

        let parent_rel = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        *dir_source_counts.entry(parent_rel).or_insert(0) += 1;

        let relative = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());

        // Count top-level items before line-count severity so god-file warnings
        // require more than size alone.
        let item_count = count_top_level_items(content, ext, grammars);

        // Check line count
        let line_count = content.lines().count();
        if line_count > GOD_FILE_LINE_THRESHOLD {
            let has_actionable_shape = item_count > HIGH_ITEM_COUNT_THRESHOLD;
            let severity = if has_actionable_shape {
                Severity::Warning
            } else {
                Severity::Info
            };
            let suggestion = if has_actionable_shape {
                format!(
                    "Review whether the file's {} top-level items cross a real responsibility boundary before extracting focused modules.",
                    item_count
                )
            } else {
                "Review-only: line count alone is not enough evidence to extract modules."
                    .to_string()
            };
            findings.push(Finding {
                convention: "structural".to_string(),
                severity,
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

/// Count top-level items in a source file using component-supplied grammars.
///
/// Core is language-agnostic: it looks up the grammar whose `file_extensions`
/// contains `ext` and applies it generically. Files with no matching grammar
/// get no item count (returns 0).
fn count_top_level_items(content: &str, ext: &str, grammars: &[LanguageGrammar]) -> usize {
    match grammar_for_extension(grammars, ext) {
        Some(grammar) => grammar.count_items(content),
        None => 0,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only grammars mirroring the rust/php/js item-counting behavior that
    /// production reads from `homeboy.json`. Kept here (not in core production)
    /// so structural.rs core logic stays language-agnostic.
    fn test_grammars() -> Vec<LanguageGrammar> {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        vec![
            LanguageGrammar {
                file_extensions: s(&["rs"]),
                item_declaration_prefixes: s(&[
                    "fn ", "struct ", "enum ", "const ", "static ", "type ", "trait ", "impl ",
                    "impl<",
                ]),
                visibility_prefixes: s(&["pub(crate) ", "pub(super) ", "pub "]),
                modifier_prefixes: s(&["async ", "unsafe "]),
                ignore_after_line_equals: s(&["#[cfg(test)]"]),
            },
            LanguageGrammar {
                file_extensions: s(&["php"]),
                item_declaration_prefixes: s(&[
                    "function ",
                    "class ",
                    "interface ",
                    "trait ",
                    "const ",
                ]),
                visibility_prefixes: s(&["public ", "protected ", "private "]),
                modifier_prefixes: s(&["static ", "abstract ", "final "]),
                ignore_after_line_equals: Vec::new(),
            },
            LanguageGrammar {
                file_extensions: s(&["js", "jsx", "mjs", "ts", "tsx"]),
                item_declaration_prefixes: s(&[
                    "function ",
                    "class ",
                    "const ",
                    "let ",
                    "var ",
                    "interface ",
                    "type ",
                    "enum ",
                ]),
                visibility_prefixes: s(&["export default ", "export "]),
                modifier_prefixes: Vec::new(),
                ignore_after_line_equals: Vec::new(),
            },
        ]
    }

    fn rust_grammar() -> LanguageGrammar {
        test_grammars()
            .into_iter()
            .find(|g| g.matches_extension("rs"))
            .unwrap()
    }

    fn php_grammar() -> LanguageGrammar {
        test_grammars()
            .into_iter()
            .find(|g| g.matches_extension("php"))
            .unwrap()
    }

    fn js_grammar() -> LanguageGrammar {
        test_grammars()
            .into_iter()
            .find(|g| g.matches_extension("js"))
            .unwrap()
    }

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
        let count = rust_grammar().count_items(content);
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
        assert_eq!(rust_grammar().count_items(content), 4);
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
        assert_eq!(php_grammar().count_items(content), 3);
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
        assert_eq!(js_grammar().count_items(content), 4);
    }

    #[test]
    fn count_items_unknown_extension_returns_zero() {
        let content = "fn a() {}\nclass B {}\nfunction c() {}\n";
        // No grammar matches an unknown extension, so nothing is counted.
        assert_eq!(count_top_level_items(content, "py", &test_grammars()), 0);
        // Empty grammar set also yields zero for a known-shaped file.
        assert_eq!(count_top_level_items(content, "rs", &[]), 0);
    }

    #[test]
    fn count_items_routes_by_extension() {
        let rust = "pub fn a() {}\nstruct B {}\n";
        assert_eq!(count_top_level_items(rust, "rs", &test_grammars()), 2);

        let php = "<?php\nclass User {}\nfunction helper() {}\n";
        assert_eq!(count_top_level_items(php, "php", &test_grammars()), 2);

        let js = "export function a() {}\nconst B = {};\n";
        assert_eq!(count_top_level_items(js, "js", &test_grammars()), 2);
        // TS shares the JS grammar.
        assert_eq!(count_top_level_items(js, "ts", &test_grammars()), 2);
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

        let findings = analyze_structure(&dir, &test_grammars());
        let god_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::GodFile)
            .collect();

        assert_eq!(god_findings.len(), 1, "Should flag big.rs as god file");
        assert_eq!(god_findings[0].file, "big.rs");
        assert!(god_findings[0].description.contains("1600 lines"));
        assert_eq!(god_findings[0].severity, Severity::Warning);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_analysis_matches_root_analysis() {
        let dir = std::env::temp_dir().join("homeboy_structural_snapshot_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let mut content = String::new();
        for i in 0..35 {
            content.push_str(&format!("fn item_{}() {{}}\n", i));
        }
        std::fs::write(dir.join("many.rs"), &content).unwrap();
        std::fs::write(dir.join("readme.md"), "# Not source\n").unwrap();

        let snapshot = build_snapshot(&dir);
        let broad_snapshot = CodebaseSnapshot::build(&dir, &ScanConfig::default());
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            serde_json::to_value(analyze_snapshot(&dir, &snapshot, &test_grammars())).unwrap(),
            serde_json::to_value(analyze_structure(&dir, &test_grammars())).unwrap()
        );
        assert_eq!(
            serde_json::to_value(analyze_snapshot(&dir, &broad_snapshot, &test_grammars()))
                .unwrap(),
            serde_json::to_value(analyze_structure(&dir, &test_grammars())).unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_only_god_file_is_info() {
        let dir = std::env::temp_dir().join("homeboy_structural_count_only_god_test");
        let _ = std::fs::create_dir_all(&dir);

        let mut content = String::from("fn cohesive() {\n");
        for i in 0..1600 {
            content.push_str(&format!("    let line_{} = {};\n", i, i));
        }
        content.push_str("}\n");
        std::fs::write(dir.join("large.rs"), content).unwrap();

        let findings = analyze_structure(&dir, &test_grammars());
        let god_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::GodFile)
            .collect();

        assert_eq!(
            god_findings.len(),
            1,
            "Should keep low-confidence visibility"
        );
        assert_eq!(god_findings[0].file, "large.rs");
        assert_eq!(god_findings[0].severity, Severity::Info);
        assert!(god_findings[0].suggestion.contains("line count alone"));
        assert!(
            findings
                .iter()
                .all(|finding| finding.kind != AuditFinding::HighItemCount),
            "Count-only god files should not be backed by a high-item-count signal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_only_structural_smells_below_actionable_threshold_are_ignored() {
        let dir = std::env::temp_dir().join("homeboy_structural_low_confidence_test");
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

        let findings = analyze_structure(&dir, &test_grammars());
        assert!(
            findings.is_empty(),
            "Moderate count-only smells should stay below the audit finding threshold"
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

        let findings = analyze_structure(&dir, &test_grammars());
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

        let findings = analyze_structure(&dir, &test_grammars());
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

        let findings = analyze_structure(&dir, &test_grammars());
        assert!(
            findings.is_empty(),
            "Clean files should produce no findings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
