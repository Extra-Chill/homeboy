use homeboy::utils::autofix::{standard_outcome, AutofixMode};

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
