use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use homeboy::extension::ParsedItem;
use homeboy::refactor::{self, DecomposeGroup, DecomposePlan};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-decompose-{name}-{nanos}"))
}

#[test]
fn test_build_plan() {
    let root = tmp_dir("build-plan");
    fs::create_dir_all(root.join("src")).expect("create source dir");
    fs::write(root.join("src/mod.rs"), "pub fn run() {}\n").expect("write source file");

    let plan = refactor::build_plan("src/mod.rs", &root, "grouped", true).expect("build plan");
    assert_eq!(plan.file, "src/mod.rs");
    assert_eq!(plan.strategy, "grouped");
    assert!(plan.audit_safe);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_apply_plan_skeletons() {
    let root = tmp_dir("apply-skeletons");
    fs::create_dir_all(&root).expect("create root");

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

    let created = refactor::apply_plan_skeletons(&plan, &root).expect("apply skeletons");
    assert_eq!(created, vec!["src/core/deploy/execution.inc".to_string()]);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_apply_plan_empty_groups() {
    let root = tmp_dir("apply-plan-empty");
    fs::create_dir_all(&root).expect("create root");

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

    let preview = refactor::apply_plan(&plan, &root, false).expect("preview apply");
    assert!(preview.is_empty());

    let applied = refactor::apply_plan(&plan, &root, true).expect("apply");
    assert!(applied.is_empty());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_group_items() {
    let root = tmp_dir("group-items");
    fs::create_dir_all(root.join("src/core")).expect("create source dir");
    fs::write(
        root.join("src/core/deploy.rs"),
        "pub struct Config {}\nfn run() {}\n",
    )
    .expect("write source file");

    let plan = refactor::build_plan("src/core/deploy.rs", &root, "grouped", true)
        .expect("build grouped plan");
    let groups = plan.groups;
    assert!(groups.iter().any(|g| g.name == "types"));
    assert!(groups.iter().any(|g| g.name == "execution"));
    assert!(groups.iter().all(|g| g.suggested_target.ends_with(".inc")));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_group_items_dedupes_duplicate_names() {
    let root = tmp_dir("group-items-dedup");
    fs::create_dir_all(root.join("src/core")).expect("create source dir");
    fs::write(
        root.join("src/core/upgrade.rs"),
        "pub enum InstallMethod { A }\npub enum InstallMethod { A }\n",
    )
    .expect("write source file");

    let plan = refactor::build_plan("src/core/upgrade.rs", &root, "grouped", true)
        .expect("build grouped plan");
    let groups = plan.groups;
    let types = groups
        .iter()
        .find(|group| group.name == "types")
        .expect("types group");
    assert_eq!(types.item_names, vec!["InstallMethod".to_string()]);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_parse_items() {
    // Unknown extension should return None without trying extension scripts.
    let root = tmp_dir("parse-items-unknown");
    fs::create_dir_all(root.join("src")).expect("create source dir");
    fs::write(root.join("src/example.unknown"), "content\n").expect("write source file");

    let plan = refactor::build_plan("src/example.unknown", &root, "grouped", true)
        .expect("build plan");
    assert_eq!(plan.total_items, 0);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("No refactor parser available"))
    );

    let _ = fs::remove_dir_all(root);

    #[test]
    fn test_apply_plan_write() {
        let plan = Default::default();
        let root = Path::new("");
        let write = true;
        let _result = apply_plan(&plan, &root, write);
    }

    #[test]
    fn test_apply_plan_write_2() {
        let plan = Default::default();
        let root = Path::new("");
        let write = true;
        let _result = apply_plan(&plan, &root, write);
    }

    #[test]
    fn test_apply_plan_write_3() {
        let plan = Default::default();
        let root = Path::new("");
        let write = false;
        let result = apply_plan(&plan, &root, write);
        assert!(result.is_ok(), "expected Ok for: !write");
    }

    #[test]
    fn test_apply_plan_default_path() {
        let plan = Default::default();
        let root = Path::new("");
        let write = false;
        let _result = apply_plan(&plan, &root, write);
    }

    #[test]
    fn test_apply_plan_results_iter_any_r_r_applied() {
        let plan = Default::default();
        let root = Path::new("");
        let write = false;
        let result = apply_plan(&plan, &root, write);
        assert!(result.is_ok(), "expected Ok for: results.iter().any(|r| r.applied)");
    }

}
