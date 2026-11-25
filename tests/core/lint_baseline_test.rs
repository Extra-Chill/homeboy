use crate::extension::lint::baseline as lint_baseline;
use homeboy_core::finding::HomeboyFinding;

fn lint_finding(id: &str, category: &str, message: &str) -> HomeboyFinding {
    HomeboyFinding::builder("lint", message)
        .category(category)
        .fingerprint(id)
        .build()
}

fn full_scope() -> lint_baseline::LintBaselineProvenance {
    lint_baseline::LintBaselineProvenance::new(
        Vec::new(),
        vec!["lint".into()],
        "full",
        None,
        false,
        None,
        None,
    )
}

#[test]
fn test_saved_baseline_reloads_and_flags_new_findings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let base = vec![lint_finding("a", "cat1", "m1")];
    lint_baseline::save_baseline_for_scope(dir.path(), "homeboy", &base, &full_scope())
        .expect("baseline saved");
    let loaded = lint_baseline::load_baseline_for_scope(dir.path(), &full_scope())
        .expect("baseline should load");
    assert_eq!(loaded.context_id, "homeboy");
    assert_eq!(loaded.item_count, 1);

    let current = vec![base[0].clone(), lint_finding("b", "cat2", "m2")];

    let comparison = lint_baseline::compare(&current, &loaded);
    assert!(comparison.drift_increased);
    assert_eq!(comparison.new_items.len(), 1);
}

#[test]
fn test_parse_findings_file_reads_sidecar_and_treats_missing_as_empty() {
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

    let missing = lint_baseline::parse_findings_file(&dir.path().join("missing.json"))
        .expect("missing file should parse to empty");
    assert!(missing.is_empty());
}
