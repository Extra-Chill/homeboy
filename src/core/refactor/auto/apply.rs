mod apply;
mod apply_fixes;
mod apply_new;
mod helpers;
mod insert;
mod normalize_import_line;

pub use apply::*;
pub use apply_fixes::*;
pub use apply_new::*;
pub use helpers::*;
pub use insert::*;
pub use normalize_import_line::*;

use crate::code_audit::conventions::Language;
use crate::core::refactor::decompose;
use crate::core::refactor::plan::generate::primary_type_name_from_declaration;
use crate::core::refactor::plan::verify::rewrite_callers_after_dedup;

use crate::engine::undo::InMemoryRollback;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use regex::Regex;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_from_single_line_pub_use() {
        let mut lines: Vec<String> = vec![
            "pub use planner::{analyze_stage_overlaps, build_refactor_plan, normalize_sources};"
                .into(),
        ];
        remove_from_pub_use_block(&mut lines, "analyze_stage_overlaps");
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("analyze_stage_overlaps"));
        assert!(lines[0].contains("build_refactor_plan"));
        assert!(lines[0].contains("normalize_sources"));
    }

    #[test]
    fn remove_last_item_deletes_entire_line() {
        let mut lines: Vec<String> = vec!["pub use planner::{only_function};".into()];
        remove_from_pub_use_block(&mut lines, "only_function");
        assert!(lines.is_empty(), "Empty pub use should be removed entirely");
    }

    #[test]
    fn remove_from_multiline_pub_use() {
        let mut lines: Vec<String> = vec![
            "pub use module::{".into(),
            "    alpha,".into(),
            "    beta,".into(),
            "    gamma,".into(),
            "};".into(),
        ];
        remove_from_pub_use_block(&mut lines, "beta");
        let joined = lines.join("\n");
        assert!(!joined.contains("beta"), "beta should be removed");
        assert!(joined.contains("alpha"), "alpha should remain");
        assert!(joined.contains("gamma"), "gamma should remain");
    }

    #[test]
    fn remove_does_not_touch_unrelated_pub_use() {
        let mut lines: Vec<String> = vec!["pub use other::{foo, bar};".into()];
        remove_from_pub_use_block(&mut lines, "baz");
        assert_eq!(lines[0], "pub use other::{foo, bar};");
    }

    #[test]
    fn insert_import_skips_identical_existing_rust_import() {
        let content = "use std::collections::HashMap;\n\npub fn run() {}\n";
        let result = insert_import(content, "use std::collections::HashMap;", &Language::Rust);
        assert_eq!(result, content);
    }

    #[test]
    fn insert_import_skips_equivalent_rust_import_with_spacing_differences() {
        let content = "use std::path::{Path, PathBuf};\n\npub fn run() {}\n";
        let result = insert_import(
            content,
            "use  std::path::{Path,   PathBuf};",
            &Language::Rust,
        );
        assert_eq!(result, content);
    }

    #[test]
    fn merge_same_file_insertions_combines_removals() {
        // Simulate the temp.rs scenario: 3 orphaned tests in the same file,
        // each generating a separate Fix with one FunctionRemoval.
        use crate::code_audit::AuditFinding;
        use crate::refactor::auto::FixSafetyTier;

        fn removal_insertion(start: usize, end: usize, desc: &str) -> Insertion {
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: start,
                    end_line: end,
                },
                finding: AuditFinding::OrphanedTest,
                code: String::new(),
                description: desc.into(),
                safety_tier: FixSafetyTier::Safe,
                auto_apply: true,
                blocked_reason: None,
                preflight: None,
            }
        }

        let mut fixes = vec![
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(108, 111, "Remove orphaned test env_lock")],
                applied: false,
            },
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(
                    151,
                    175,
                    "Remove orphaned test prune_removes",
                )],
                applied: false,
            },
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(
                    177,
                    197,
                    "Remove orphaned test prune_ignores",
                )],
                applied: false,
            },
            Fix {
                file: "src/other.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(10, 20, "Some other file fix")],
                applied: false,
            },
        ];

        merge_same_file_insertions(&mut fixes);

        // The first temp.rs fix should have all 3 insertions merged into it
        let temp_fixes_with_insertions: Vec<_> = fixes
            .iter()
            .filter(|f| f.file == "src/engine/temp.rs" && !f.insertions.is_empty())
            .collect();
        assert_eq!(
            temp_fixes_with_insertions.len(),
            1,
            "Only one temp.rs fix should have insertions"
        );
        assert_eq!(
            temp_fixes_with_insertions[0].insertions.len(),
            3,
            "merged fix should have all 3 insertions"
        );

        // Donor fixes should be emptied (insertions drained)
        let empty_temp_fixes = fixes
            .iter()
            .filter(|f| f.file == "src/engine/temp.rs" && f.insertions.is_empty())
            .count();
        assert_eq!(empty_temp_fixes, 2, "donor fixes should be emptied");

        // The other.rs fix should be untouched
        let other_fixes: Vec<_> = fixes.iter().filter(|f| f.file == "src/other.rs").collect();
        assert_eq!(other_fixes.len(), 1);
        assert_eq!(other_fixes[0].insertions.len(), 1);
    }

    #[test]
    fn multiple_removals_same_file_preserve_braces() {
        // Reproduce the temp.rs brace corruption: a test module with multiple
        // test functions removed. The mod tests closing brace must survive.
        let content = "\
fn source_fn() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> i32 {
        42
    }

    #[test]
    fn test_alpha() {
        assert_eq!(helper(), 42);
    }

    #[test]
    fn test_beta() {
        let x = 1;
        assert_eq!(x, 1);
    }

    #[test]
    fn test_gamma() {
        let y = 2;
        assert_eq!(y, 2);
    }
}
";
        use crate::code_audit::AuditFinding;
        use crate::refactor::auto::FixSafetyTier;

        fn removal(start: usize, end: usize, desc: &str) -> Insertion {
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: start,
                    end_line: end,
                },
                finding: AuditFinding::OrphanedTest,
                code: String::new(),
                description: desc.into(),
                safety_tier: FixSafetyTier::Safe,
                auto_apply: true,
                blocked_reason: None,
                preflight: None,
            }
        }

        // Remove all three test functions (lines are 1-indexed):
        // test_alpha: #[test] at 11, fn at 12, body 13, } at 14
        // test_beta:  #[test] at 16, fn at 17, body 18-19, } at 20
        // test_gamma: #[test] at 22, fn at 23, body 24-25, } at 26
        let insertions = vec![
            removal(11, 14, "Remove test_alpha"),
            removal(16, 20, "Remove test_beta"),
            removal(22, 26, "Remove test_gamma"),
        ];

        let result = apply_insertions_to_content(content, &insertions, &Language::Rust);

        // The mod tests block must still be properly closed
        let open_braces = result.matches('{').count();
        let close_braces = result.matches('}').count();
        assert_eq!(
            open_braces, close_braces,
            "Braces must be balanced after removal.\nResult:\n{result}"
        );
        assert!(
            result.contains("mod tests {"),
            "mod tests should still exist"
        );
        // helper() should survive — it wasn't removed
        assert!(
            result.contains("fn helper()"),
            "helper function should survive"
        );
    }
}
