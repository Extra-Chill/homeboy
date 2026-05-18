use homeboy::core::extension::lint::baseline::{self as lint_baseline, LintFinding};
use std::path::Path;

#[test]
fn test_save_baseline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let findings = vec![
        LintFinding {
            id: "a".to_string(),
            message: "m1".to_string(),
            category: "cat1".to_string(),
            ..Default::default()
        },
        LintFinding {
            id: "b".to_string(),
            message: "m2".to_string(),
            category: "cat2".to_string(),
            ..Default::default()
        },
    ];

    let saved = lint_baseline::save_baseline(dir.path(), "homeboy", &findings)
        .expect("save baseline should succeed");
    assert!(saved.exists());
}

#[test]
fn test_load_baseline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let findings = vec![LintFinding {
        id: "a".to_string(),
        message: "m1".to_string(),
        category: "cat1".to_string(),
        ..Default::default()
    }];
    lint_baseline::save_baseline(dir.path(), "homeboy", &findings).expect("baseline saved");

    let loaded = lint_baseline::load_baseline(dir.path()).expect("baseline should load");
    assert_eq!(loaded.context_id, "homeboy");
    assert_eq!(loaded.item_count, 1);
}

#[test]
fn test_compare() {
    let dir = tempfile::tempdir().expect("temp dir");
    let base = vec![LintFinding {
        id: "a".to_string(),
        message: "m1".to_string(),
        category: "cat1".to_string(),
        ..Default::default()
    }];
    lint_baseline::save_baseline(dir.path(), "homeboy", &base).expect("baseline saved");
    let loaded = lint_baseline::load_baseline(dir.path()).expect("baseline should load");

    let current = vec![
        base[0].clone(),
        LintFinding {
            id: "b".to_string(),
            message: "m2".to_string(),
            category: "cat2".to_string(),
            ..Default::default()
        },
    ];

    let comparison = lint_baseline::compare(&current, &loaded);
    assert!(comparison.drift_increased);
    assert_eq!(comparison.new_items.len(), 1);
}

#[test]
fn test_parse_findings_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("lint-findings.json");
    std::fs::write(&file, r#"[{"id":"a","message":"m1","category":"cat1"}]"#)
        .expect("should write JSON");

    let parsed = lint_baseline::parse_findings_file(&file).expect("should parse findings");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "a");
}

#[test]
fn test_parse_findings_file_missing() {
    let parsed = lint_baseline::parse_findings_file(Path::new(
        "/tmp/definitely-missing-lint-baseline-test.json",
    ))
    .expect("missing file should parse to empty");
    assert!(parsed.is_empty());
}
