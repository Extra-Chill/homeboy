use super::super::exit_code::{
    effective_lint_exit_code, normalize_empty_finding_exit_code, normalize_producer_exit_code,
};
use homeboy_core::finding::FindingProducerSummary;

/// An empty (post-filter) finding set turns a lint-finding exit into a pass,
/// but never hides a failed or crashed producer or an infrastructure exit.
#[test]
fn empty_findings_pass_only_when_no_producer_failed() {
    let failed_zero_finding = vec![
        FindingProducerSummary::new("phpcs", "passed").finding_count(0),
        FindingProducerSummary::new("phpstan", "failed").finding_count(0),
    ];
    let crashed_zero_finding =
        vec![FindingProducerSummary::new("phpstan", "error").finding_count(0)];

    for (runner_exit, producers, expected) in [
        (1, &[][..], 0),
        (2, &[][..], 2),
        (1, &failed_zero_finding[..], 1),
        (1, &crashed_zero_finding[..], 1),
    ] {
        let exit_code = normalize_empty_finding_exit_code(runner_exit, false, &[], producers);
        assert_eq!(
            normalize_producer_exit_code(exit_code, producers),
            expected,
            "runner exit {runner_exit} with {producers:?}"
        );
    }
}

/// Warnings from passing producers stay non-blocking; one failed producer
/// fails the run even when the runner exited 0.
#[test]
fn failed_producer_amid_warnings_forces_failure() {
    for (eslint_status, expected) in [("passed", 0), ("failed", 1)] {
        let producer_summaries = vec![
            FindingProducerSummary::new("phpcs", "passed").finding_count(49),
            FindingProducerSummary::new("eslint", eslint_status).finding_count(1),
            FindingProducerSummary::new("phpstan", "passed").finding_count(0),
        ];

        assert_eq!(
            normalize_producer_exit_code(0, &producer_summaries),
            expected,
            "eslint {eslint_status}"
        );
    }
}

/// The baseline override honors known findings, but cannot fail a clean
/// current scope from an unrelated baseline context, and never masks
/// infrastructure errors or introduced findings.
#[test]
fn baseline_override_honors_known_findings_but_not_infrastructure_errors() {
    for (runner_exit, baseline_override, hard_error, current_findings_empty, expected) in [
        (1, Some(0), false, false, 0),
        (2, Some(0), true, true, 2),
        (1, Some(0), true, true, 1),
        (0, Some(0), true, false, 1),
        // An unrelated baseline delta cannot fail a clean current scope...
        (0, Some(1), false, true, 0),
        // ...but introduced current findings and infrastructure stay blocking.
        (0, Some(1), false, false, 1),
        (0, Some(1), true, true, 1),
    ] {
        assert_eq!(
            effective_lint_exit_code(
                runner_exit,
                baseline_override,
                hard_error,
                current_findings_empty
            ),
            expected,
            "exit {runner_exit}, override {baseline_override:?}, hard_error {hard_error}, current_findings_empty {current_findings_empty}"
        );
    }
}
