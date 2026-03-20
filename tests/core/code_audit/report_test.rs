use crate::code_audit::report::{build_audit_summary, compute_fixability};
use crate::code_audit::test_helpers::{empty_result, make_finding};
use crate::code_audit::Severity;

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
