use homeboy::core::extension::lint::baseline as lint_baseline;
use homeboy::core::finding::HomeboyFinding;
use std::path::Path;

fn lint_finding(id: &str, category: &str, message: &str) -> HomeboyFinding {
    HomeboyFinding::builder("lint", message)
        .category(category)
        .fingerprint(id)
        .build()
}

#[test]
fn test_save_baseline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let findings = vec![
        lint_finding("a", "cat1", "m1"),
        lint_finding("b", "cat2", "m2"),
    ];

    let saved = lint_baseline::save_baseline(dir.path(), "homeboy", &findings)
        .expect("save baseline should succeed");
    assert!(saved.exists());
}

#[test]
fn test_load_baseline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let findings = vec![lint_finding("a", "cat1", "m1")];
    lint_baseline::save_baseline(dir.path(), "homeboy", &findings).expect("baseline saved");

    let loaded = lint_baseline::load_baseline(dir.path()).expect("baseline should load");
    assert_eq!(loaded.context_id, "homeboy");
    assert_eq!(loaded.item_count, 1);
}

#[test]
fn test_compare() {
    let dir = tempfile::tempdir().expect("temp dir");
    let base = vec![lint_finding("a", "cat1", "m1")];
    lint_baseline::save_baseline(dir.path(), "homeboy", &base).expect("baseline saved");
    let loaded = lint_baseline::load_baseline(dir.path()).expect("baseline should load");

    let current = vec![base[0].clone(), lint_finding("b", "cat2", "m2")];

    let comparison = lint_baseline::compare(&current, &loaded);
    assert!(comparison.drift_increased);
    assert_eq!(comparison.new_items.len(), 1);
}

#[test]
fn test_parse_findings_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("lint-findings.json");
    std::fs::write(
        &file,
        r#"[{"tool":"eslint","message":"m1","category":"cat1","fingerprint":"a","file":"src/lib.rs","line":12,"source":{"kind":"sidecar","label":"custom","path":"custom.json"}}]"#,
    )
        .expect("should write JSON");

    let parsed = lint_baseline::parse_findings_file(&file).expect("should parse findings");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].tool, "eslint");
    assert_eq!(parsed[0].fingerprint.as_deref(), Some("a"));
    assert_eq!(parsed[0].location.file.as_deref(), Some("src/lib.rs"));
    assert_eq!(parsed[0].location.line, Some(12));
    assert_eq!(
        parsed[0].source.as_ref().unwrap().label.as_deref(),
        Some("custom")
    );
    assert_eq!(parsed[0].metadata["source_sidecar"], "lint-findings");
}

#[test]
fn test_parse_findings_file_missing() {
    let parsed = lint_baseline::parse_findings_file(Path::new(
        "/tmp/definitely-missing-lint-baseline-test.json",
    ))
    .expect("missing file should parse to empty");
    assert!(parsed.is_empty());
}
