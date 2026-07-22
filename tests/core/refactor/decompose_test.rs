use crate::extension::ParsedItem;
use crate::refactor::{self, DecomposeGroup, DecomposePlan};
use std::fs;

#[path = "../../support/mod.rs"]
#[allow(dead_code)]
mod support;

#[test]
fn test_build_plan() {
    let root = support::temp_dir("decompose-build-plan");
    fs::create_dir_all(root.path().join("src")).expect("create source dir");
    fs::write(root.path().join("src/mod.rs"), "pub fn run() {}\n").expect("write source file");

    let plan =
        refactor::build_plan("src/mod.rs", root.path(), "grouped", true).expect("build plan");
    assert_eq!(plan.file, "src/mod.rs");
    assert_eq!(plan.strategy, "grouped");
    assert!(plan.audit_safe);
}

#[test]
fn test_build_plan_missing_file_errors() {
    let root = support::temp_dir("decompose-build-plan-missing");
    let result = refactor::build_plan("src/missing.rs", root.path(), "grouped", true);
    assert!(result.is_err());
}

#[test]
fn test_apply_plan_skeletons() {
    let root = support::temp_dir("decompose-apply-skeletons");

    let plan = DecomposePlan {
        file: "src/core/deploy.rs".to_string(),
        strategy: "grouped".to_string(),
        audit_safe: true,
        total_items: 1,
        groups: vec![DecomposeGroup {
            name: "execution".to_string(),
            suggested_target: "src/core/deploy/execution.inc".to_string(),
            item_names: vec!["run".to_string()],
        }],
        projected_audit_impact: refactor::DecomposeAuditImpact {
            estimated_new_files: 1,
            estimated_new_test_files: 0,
            recommended_test_files: vec![],
            likely_findings: vec![],
        },
        checklist: vec![],
        warnings: vec![],
    };

    let created = refactor::apply_plan_skeletons(&plan, root.path()).expect("apply skeletons");
    assert_eq!(created, vec!["src/core/deploy/execution.inc".to_string()]);
}

#[test]
fn test_apply_plan_empty_groups() {
    let root = support::temp_dir("decompose-apply-plan-empty");

    let plan = DecomposePlan {
        file: "src/core/deploy.rs".to_string(),
        strategy: "grouped".to_string(),
        audit_safe: true,
        total_items: 0,
        groups: vec![],
        projected_audit_impact: refactor::DecomposeAuditImpact {
            estimated_new_files: 0,
            estimated_new_test_files: 0,
            recommended_test_files: vec![],
            likely_findings: vec![],
        },
        checklist: vec![],
        warnings: vec![],
    };

    let preview = refactor::apply_plan(&plan, root.path(), false).expect("preview apply");
    assert!(preview.is_empty());

    let applied = refactor::apply_plan(&plan, root.path(), true).expect("apply");
    assert!(applied.is_empty());
}

#[test]
fn test_group_items() {
    let root = support::temp_dir("decompose-group-items");
    fs::create_dir_all(root.path().join("src/core")).expect("create source dir");
    fs::write(
        root.path().join("src/core/deploy.rs"),
        "pub struct Config {}\nfn run() {}\n",
    )
    .expect("write source file");

    let plan = refactor::build_plan("src/core/deploy.rs", root.path(), "grouped", true)
        .expect("build grouped plan");
    let groups = plan.groups;
    assert!(groups.iter().any(|g| g.name == "types"));
    assert!(groups.iter().any(|g| g.name == "execution"));
    assert!(groups.iter().all(|g| g.suggested_target.ends_with(".inc")));
}

#[test]
fn test_group_items_dedupes_duplicate_names() {
    let root = support::temp_dir("decompose-group-items-dedup");
    fs::create_dir_all(root.path().join("src/core")).expect("create source dir");
    fs::write(
        root.path().join("src/core/upgrade.rs"),
        "pub enum InstallMethod { A }\npub enum InstallMethod { A }\n",
    )
    .expect("write source file");

    let plan = refactor::build_plan("src/core/upgrade.rs", root.path(), "grouped", true)
        .expect("build grouped plan");
    let groups = plan.groups;
    let types = groups
        .iter()
        .find(|group| group.name == "types")
        .expect("types group");
    assert_eq!(types.item_names, vec!["InstallMethod".to_string()]);
}

#[test]
fn test_parse_items() {
    // Unknown extension should return None without trying extension scripts.
    let root = support::temp_dir("decompose-parse-items-unknown");
    fs::create_dir_all(root.path().join("src")).expect("create source dir");
    fs::write(root.path().join("src/example.unknown"), "content\n").expect("write source file");

    let plan = refactor::build_plan("src/example.unknown", root.path(), "grouped", true)
        .expect("build plan");
    assert_eq!(plan.total_items, 0);
    assert!(plan
        .warnings
        .iter()
        .any(|warning| warning.contains("No refactor parser available")));
}
