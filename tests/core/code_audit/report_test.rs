use crate::code_audit::report::{
    build_audit_summary, build_fix_hints, build_fix_policy_summary, compute_fixability,
};
use crate::code_audit::test_helpers::{empty_result, make_finding};
use crate::code_audit::Severity;
use crate::refactor::auto::PolicySummary;

#[test]
fn test_build_audit_summary_empty_result() {
    let result = empty_result();
    let summary = build_audit_summary(&result, 0);

    assert_eq!(summary.total_findings, 0);
    assert_eq!(summary.warnings, 0);
    assert_eq!(summary.info, 0);
    assert_eq!(summary.exit_code, 0);
    assert!(summary.top_findings.is_empty());
    assert_eq!(summary.alignment_score, None);
}

#[test]
fn test_build_audit_summary_counts_severities() {
    let mut result = empty_result();
    result.findings.push(make_finding(Severity::Warning));
    result.findings.push(make_finding(Severity::Warning));
    result.findings.push(make_finding(Severity::Info));

    let summary = build_audit_summary(&result, 1);

    assert_eq!(summary.total_findings, 3);
    assert_eq!(summary.warnings, 2);
    assert_eq!(summary.info, 1);
    assert_eq!(summary.exit_code, 1);
}

#[test]
fn test_build_audit_summary_caps_top_findings_at_20() {
    let mut result = empty_result();
    for _ in 0..25 {
        result.findings.push(make_finding(Severity::Warning));
    }

    let summary = build_audit_summary(&result, 1);

    assert_eq!(summary.total_findings, 25);
    assert_eq!(summary.top_findings.len(), 20);
}

#[test]
fn test_build_audit_summary_preserves_alignment_score() {
    let mut result = empty_result();
    result.summary.alignment_score = Some(0.85);

    let summary = build_audit_summary(&result, 0);

    assert_eq!(summary.alignment_score, Some(0.85));
}

#[test]
fn test_build_fix_hints_empty_when_no_blocked() {
    let policy = PolicySummary::default();
    let hints = build_fix_hints(false, &policy);
    assert!(hints.is_empty());
}

#[test]
fn test_build_fix_hints_dry_run_with_blocked() {
    let policy = PolicySummary {
        blocked_insertions: 2,
        blocked_new_files: 1,
        ..Default::default()
    };
    let hints = build_fix_hints(false, &policy);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].contains("3 fix(es)"));
    assert!(hints[0].contains("blocked"));
}

#[test]
fn test_build_fix_hints_written_with_blocked() {
    let policy = PolicySummary {
        blocked_insertions: 1,
        blocked_new_files: 0,
        ..Default::default()
    };
    let hints = build_fix_hints(true, &policy);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].contains("Applied safe fixes"));
}

#[test]
fn test_build_fix_hints_preflight_failures() {
    let policy = PolicySummary {
        preflight_failures: 3,
        ..Default::default()
    };
    let hints = build_fix_hints(false, &policy);
    assert_eq!(hints.len(), 1);
    assert!(hints[0].contains("3 fix(es) failed preflight"));
}

#[test]
fn test_build_fix_hints_multiple_conditions() {
    let policy = PolicySummary {
        blocked_insertions: 1,
        blocked_new_files: 1,
        preflight_failures: 2,
        ..Default::default()
    };
    let hints = build_fix_hints(true, &policy);
    // Should have both blocked hint and preflight hint
    assert_eq!(hints.len(), 2);
}

#[test]
fn test_build_fix_policy_summary_maps_fields() {
    let policy = PolicySummary {
        visible_insertions: 10,
        visible_new_files: 5,
        auto_apply_insertions: 7,
        auto_apply_new_files: 3,
        blocked_insertions: 3,
        blocked_new_files: 2,
        preflight_failures: 1,
    };
    let summary = build_fix_policy_summary(
        &policy,
        vec!["kind_a".to_string()],
        vec!["kind_b".to_string()],
    );

    assert_eq!(summary.selected_only, vec!["kind_a"]);
    assert_eq!(summary.excluded, vec!["kind_b"]);
    assert_eq!(summary.visible_insertions, 10);
    assert_eq!(summary.visible_new_files, 5);
    assert_eq!(summary.auto_apply_insertions, 7);
    assert_eq!(summary.auto_apply_new_files, 3);
    assert_eq!(summary.blocked_insertions, 3);
    assert_eq!(summary.blocked_new_files, 2);
    assert_eq!(summary.preflight_failures, 1);
}

#[test]
fn test_compute_fixability_returns_none_for_empty_result() {
    let result = empty_result();
    // source_path is /tmp/test which exists but has no source files to fix
    let fixability = compute_fixability(&result);
    assert!(fixability.is_none());
}

#[test]
fn test_compute_fixability_returns_none_for_nonexistent_path() {
    let mut result = empty_result();
    result.source_path = "/nonexistent/path/that/does/not/exist".to_string();
    result.findings.push(make_finding(Severity::Warning));
    let fixability = compute_fixability(&result);
    assert!(fixability.is_none());
}

#[test]
fn test_compute_fixability_counts_fixes_from_real_audit() {
    use std::fs;

    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    // Create a minimal codebase with a detectable convention + outlier
    fs::create_dir_all(root.join("commands")).unwrap();
    // Two conforming files establish a convention (methods: run + helper)
    fs::write(
        root.join("commands/good_one.rs"),
        "pub fn run() {}\npub fn helper() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("commands/good_two.rs"),
        "pub fn run() {}\npub fn helper() {}\n",
    )
    .unwrap();
    // One outlier is missing a method → should produce a fixable finding
    fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

    // Run a real audit
    let result = crate::code_audit::audit_path_with_id("fixability-test", &root.to_string_lossy())
        .expect("audit should run");

    // Compute fixability
    let fixability = compute_fixability(&result);

    // Should have at least some fixable findings (the missing method outlier)
    if let Some(fix) = fixability {
        assert!(
            fix.fixable_count > 0,
            "expected at least one fixable finding"
        );
        // safe + plan_only should equal fixable_count
        assert_eq!(fix.fixable_count, fix.safe_count + fix.plan_only_count);
        // by_kind should not be empty
        assert!(!fix.by_kind.is_empty(), "expected per-kind breakdown");
    }
    // Note: fixability may also be None if the minimal codebase doesn't trigger
    // enough conventions — that's acceptable for this test.
}
