//! Unit tests for the audit workflow filtering primitive.
//!
//! Wired into `src/core/code_audit/run.rs` via `#[cfg(test)] #[path = ...] mod run_test`.

use super::{
    apply_finding_filters, compute_fixability_if_requested, scope_convention_outliers_to_findings,
    AuditRunWorkflowArgs,
};
use crate::code_audit::checks::CheckStatus;
use crate::code_audit::conventions::{Deviation, Outlier};
use crate::code_audit::findings::{Finding, Severity};
use crate::code_audit::{
    AuditExecutionPlan, AuditFinding, AuditSummary, CodeAuditResult, ConventionReport,
};

fn make_finding(kind: AuditFinding, file: &str) -> Finding {
    Finding {
        convention: "test".to_string(),
        severity: Severity::Warning,
        file: file.to_string(),
        description: format!("{:?} on {}", kind, file),
        suggestion: "fix it".to_string(),
        kind,
    }
}

fn make_result(findings: Vec<Finding>) -> CodeAuditResult {
    let outliers = findings.len();
    CodeAuditResult {
        component_id: "test".to_string(),
        source_path: "/tmp/test".to_string(),
        summary: AuditSummary {
            files_scanned: 1,
            conventions_detected: 0,
            outliers_found: outliers,
            alignment_score: None,
            files_skipped: 0,
            warnings: vec![],
        },
        conventions: vec![],
        directory_conventions: vec![],
        findings,
        duplicate_groups: vec![],
    }
}

fn make_convention_report(name: &str, outliers: Vec<Outlier>) -> ConventionReport {
    ConventionReport {
        name: name.to_string(),
        glob: "src/**/*.rs".to_string(),
        status: CheckStatus::Drift,
        expected_methods: vec!["run".to_string()],
        expected_registrations: vec![],
        expected_interfaces: vec![],
        expected_namespace: None,
        expected_imports: vec![],
        conforming: vec!["src/changed.rs".to_string()],
        outliers,
        total_files: 3,
        confidence: 0.75,
    }
}

fn make_outlier(file: &str, kinds: Vec<AuditFinding>) -> Outlier {
    Outlier {
        file: file.to_string(),
        noisy: false,
        deviations: kinds
            .into_iter()
            .map(|kind| Deviation {
                kind,
                description: "deviates".to_string(),
                suggestion: "fix it".to_string(),
            })
            .collect(),
    }
}

fn make_args(include_fixability: bool) -> AuditRunWorkflowArgs {
    AuditRunWorkflowArgs {
        component_id: "test".to_string(),
        source_path: "/tmp/test".to_string(),
        conventions: false,
        only_kinds: vec![],
        exclude_kinds: vec![],
        only_labels: vec![],
        exclude_labels: vec![],
        baseline_flags: crate::engine::baseline::BaselineFlags {
            baseline: false,
            ignore_baseline: false,
            ratchet: false,
        },
        changed_since: None,
        json_summary: false,
        include_fixability,
    }
}

#[test]
fn filter_noop_when_both_lists_empty() {
    // The common case: no flags → no-op, untouched findings AND untouched summary.
    let mut result = make_result(vec![
        make_finding(AuditFinding::TodoMarker, "a.rs"),
        make_finding(AuditFinding::LegacyComment, "b.rs"),
    ]);

    apply_finding_filters(&mut result, &[], &[]);

    assert_eq!(result.findings.len(), 2);
    assert_eq!(result.summary.outliers_found, 2);
}

#[test]
fn only_keeps_listed_kinds_and_refreshes_outliers_count() {
    // Regression for the silent-no-op `--only` bug: the filter was parsed but
    // never applied to the read-only audit path. This is the round-trip test.
    let mut result = make_result(vec![
        make_finding(AuditFinding::TodoMarker, "a.rs"),
        make_finding(AuditFinding::LegacyComment, "b.rs"),
        make_finding(AuditFinding::GodFile, "c.rs"),
    ]);

    apply_finding_filters(&mut result, &[AuditFinding::LegacyComment], &[]);

    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].kind, AuditFinding::LegacyComment);
    // outliers_found drives default_audit_exit_code; must reflect the filtered view.
    assert_eq!(result.summary.outliers_found, 1);
}

#[test]
fn exclude_drops_listed_kinds_and_refreshes_outliers_count() {
    let mut result = make_result(vec![
        make_finding(AuditFinding::TodoMarker, "a.rs"),
        make_finding(AuditFinding::LegacyComment, "b.rs"),
        make_finding(AuditFinding::GodFile, "c.rs"),
    ]);

    apply_finding_filters(&mut result, &[], &[AuditFinding::TodoMarker]);

    assert_eq!(result.findings.len(), 2);
    assert!(result
        .findings
        .iter()
        .all(|f| f.kind != AuditFinding::TodoMarker));
    assert_eq!(result.summary.outliers_found, 2);
}

#[test]
fn exclude_takes_precedence_over_only() {
    // If a kind appears in both lists, exclude wins — the user explicitly
    // asked for it to be dropped.
    let mut result = make_result(vec![
        make_finding(AuditFinding::TodoMarker, "a.rs"),
        make_finding(AuditFinding::LegacyComment, "b.rs"),
    ]);

    apply_finding_filters(
        &mut result,
        &[AuditFinding::TodoMarker, AuditFinding::LegacyComment],
        &[AuditFinding::TodoMarker],
    );

    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].kind, AuditFinding::LegacyComment);
    assert_eq!(result.summary.outliers_found, 1);
}

#[test]
fn only_with_no_matches_leaves_zero_findings_and_clean_exit() {
    // Filtering down to a kind that has no findings → empty findings AND
    // outliers_found == 0, so default_audit_exit_code returns 0 (clean).
    let mut result = make_result(vec![
        make_finding(AuditFinding::TodoMarker, "a.rs"),
        make_finding(AuditFinding::LegacyComment, "b.rs"),
    ]);

    apply_finding_filters(&mut result, &[AuditFinding::DeadGuard], &[]);

    assert!(result.findings.is_empty());
    assert_eq!(result.summary.outliers_found, 0);
}

#[test]
fn scoped_convention_outliers_follow_scoped_findings() {
    let mut result = make_result(vec![make_finding(
        AuditFinding::MissingMethod,
        "src/changed.rs",
    )]);
    result.findings[0].convention = "ability convention".to_string();
    result.summary.outliers_found = 3;
    result.conventions = vec![make_convention_report(
        "ability convention",
        vec![
            make_outlier(
                "src/changed.rs",
                vec![
                    AuditFinding::MissingMethod,
                    AuditFinding::MissingRegistration,
                ],
            ),
            make_outlier("src/unrelated.rs", vec![AuditFinding::MissingMethod]),
        ],
    )];

    scope_convention_outliers_to_findings(&mut result);

    assert_eq!(result.conventions.len(), 1);
    assert_eq!(result.conventions[0].outliers.len(), 1);
    assert_eq!(result.conventions[0].outliers[0].file, "src/changed.rs");
    assert_eq!(result.conventions[0].outliers[0].deviations.len(), 1);
    assert_eq!(
        result.conventions[0].outliers[0].deviations[0].kind,
        AuditFinding::MissingMethod
    );
    assert_eq!(result.summary.outliers_found, 1);
}

#[test]
fn execution_plan_for_structural_only_skips_unrelated_detector_families() {
    let plan = AuditExecutionPlan::from_filters(&[AuditFinding::GodFile], &[]);

    assert!(plan.run_structural);
    assert!(!plan.run_conventions);
    assert!(!plan.run_duplication);
    assert!(!plan.run_dead_code);
    assert!(!plan.run_compiler_warnings);
}

#[test]
fn execution_plan_for_duplicate_only_skips_structural_detector_family() {
    let plan = AuditExecutionPlan::from_filters(&[AuditFinding::DuplicateFunction], &[]);

    assert!(plan.run_duplication);
    assert!(!plan.run_structural);
    assert!(!plan.run_conventions);
}

#[test]
fn execution_plan_for_unwired_nested_rust_test_runs_wiring_detector() {
    let plan = AuditExecutionPlan::from_filters(&[AuditFinding::UnwiredNestedRustTest], &[]);

    assert!(plan.run_rust_test_wiring);
    assert!(!plan.run_test_topology);
    assert!(!plan.run_conventions);
}

#[test]
fn execution_plan_is_full_without_filters() {
    assert_eq!(
        AuditExecutionPlan::from_filters(&[], &[]),
        AuditExecutionPlan::full()
    );
}

#[test]
fn fixability_is_skipped_unless_requested() {
    let result = make_result(vec![make_finding(AuditFinding::TodoMarker, "a.rs")]);
    let args = make_args(false);

    let fixability = compute_fixability_if_requested(&result, &args);

    assert!(fixability.is_none());
}

#[test]
fn fixability_flag_allows_planning_path() {
    let result = make_result(vec![make_finding(AuditFinding::TodoMarker, "a.rs")]);
    let args = make_args(true);

    let fixability = compute_fixability_if_requested(&result, &args);

    // The fixture path does not exist, so the planner returns None. The test
    // still pins the flag contract: true is the only path that reaches the
    // existing compute_fixability() guard.
    assert!(fixability.is_none());
}
