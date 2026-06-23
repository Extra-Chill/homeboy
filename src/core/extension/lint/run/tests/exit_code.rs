use super::super::exit_code::{
    effective_lint_exit_code, normalize_empty_finding_exit_code, normalize_finding_exit_code,
};
use super::lint_finding;
use crate::core::finding::FindingProducerSummary;

#[test]
fn empty_filtered_findings_turn_lint_finding_exit_into_pass() {
    let exit_code = normalize_empty_finding_exit_code(1, false, &[], &[]);

    assert_eq!(exit_code, 0);
}

#[test]
fn failed_zero_finding_producer_keeps_lint_failure() {
    let producer_summaries = vec![
        FindingProducerSummary::new("phpcs", "passed").finding_count(0),
        FindingProducerSummary::new("phpstan", "failed").finding_count(0),
    ];
    let exit_code = normalize_empty_finding_exit_code(1, false, &[], &producer_summaries);

    assert_eq!(exit_code, 1);
}

#[test]
fn empty_filtered_findings_do_not_hide_infrastructure_errors() {
    let exit_code = normalize_empty_finding_exit_code(2, false, &[], &[]);

    assert_eq!(exit_code, 2);
}

#[test]
fn findings_force_failure_when_runner_exits_cleanly() {
    let exit_code = normalize_finding_exit_code(0, &[lint_finding("a", "security", "rule")]);

    assert_eq!(exit_code, 1);
}

#[test]
fn baseline_clean_override_honors_known_findings_but_not_infrastructure_errors() {
    assert_eq!(effective_lint_exit_code(1, Some(0)), 0);
    assert_eq!(effective_lint_exit_code(2, Some(0)), 2);
}
