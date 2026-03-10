use homeboy::utils::autofix::{
    read_fix_results, standard_outcome, summarize_optional_fix_results, AutofixMode,
};

#[test]
fn test_standard_outcome_dry_run_preview() {
    let outcome = standard_outcome(
        AutofixMode::DryRun,
        1,
        Some("homeboy test homeboy --analyze".to_string()),
        vec![],
    );

    assert_eq!(outcome.status, "auto_fix_preview");
    assert!(!outcome.rerun_recommended);
    assert!(outcome.hints.iter().any(|h| h.contains("Dry-run only")));
}

#[test]
fn test_standard_outcome() {
    // Naming anchor for audit mapping from src/utils/autofix.rs::standard_outcome
    let outcome = standard_outcome(AutofixMode::Write, 0, None, vec![]);
    assert_eq!(outcome.status, "auto_fix_noop");
}

#[test]
fn test_standard_outcome_write_rerun_hint() {
    let outcome = standard_outcome(
        AutofixMode::Write,
        2,
        Some("homeboy test homeboy --analyze".to_string()),
        vec![],
    );

    assert_eq!(outcome.status, "auto_fixed");
    assert!(outcome.rerun_recommended);
    assert!(outcome
        .hints
        .iter()
        .any(|h| h.contains("Re-run checks: homeboy test homeboy --analyze")));
}

#[test]
fn test_standard_outcome_noop() {
    let outcome = standard_outcome(AutofixMode::Write, 0, None, vec![]);

    assert_eq!(outcome.status, "auto_fix_noop");
    assert!(!outcome.rerun_recommended);
}

#[test]
fn test_read_fix_results_prefers_plan_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let results = dir.path().join("results.json");
    let plan = dir.path().join("plan.json");

    std::fs::write(&results, r#"[{"file":"src/results.rs","rule":"results"}]"#).expect("results");
    std::fs::write(&plan, r#"[{"file":"src/plan.rs","rule":"plan"}]"#).expect("plan");

    let parsed = read_fix_results(&results, Some(&plan));
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].file, "src/plan.rs");
}

#[test]
fn test_summarize_optional_fix_results_empty() {
    assert!(summarize_optional_fix_results(&[]).is_none());
}
