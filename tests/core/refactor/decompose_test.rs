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
        checklist: vec![],
        warnings: vec![],
    };

    let created = refactor::apply_plan_skeletons(&plan, &root).expect("apply skeletons");
    assert_eq!(created, vec!["src/core/deploy/execution.inc".to_string()]);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_classify_function() {
    assert_eq!(refactor::classify_function("validate_input"), "validation");
    assert_eq!(refactor::classify_function("parse_output"), "planning");
    assert_eq!(refactor::classify_function("run"), "execution");
    assert_eq!(refactor::classify_function("something_else"), "helpers");
}

#[test]
fn test_group_items() {
    let items = vec![
        ParsedItem {
            kind: "struct".to_string(),
            name: "Config".to_string(),
            start_line: 1,
            end_line: 5,
            source: "pub struct Config {}".to_string(),
            visibility: "pub".to_string(),
        },
        ParsedItem {
            kind: "function".to_string(),
            name: "run".to_string(),
            start_line: 6,
            end_line: 10,
            source: "fn run() {}".to_string(),
            visibility: "".to_string(),
        },
    ];

    let groups = refactor::group_items("src/core/deploy.rs", &items, true);
    assert!(groups.iter().any(|g| g.name == "types"));
    assert!(groups.iter().any(|g| g.name == "execution"));
    assert!(groups.iter().all(|g| g.suggested_target.ends_with(".inc")));
}

#[test]
fn test_parse_items() {
    // Unknown extension should return None without trying extension scripts.
    let result = refactor::parse_items("src/example.unknown", "content");
    assert!(result.is_none());
}
