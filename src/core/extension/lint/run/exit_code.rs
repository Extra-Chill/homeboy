//! Lint exit-code normalization — reconciles runner exit codes with finding
//! counts, producer statuses, and baseline overrides.

use crate::core::finding::{FindingProducerSummary, HomeboyFinding};

pub(super) fn normalize_empty_finding_exit_code(
    exit_code: i32,
    success: bool,
    lint_findings: &[HomeboyFinding],
    producer_summaries: &[FindingProducerSummary],
) -> i32 {
    if lint_findings.is_empty()
        && !success
        && exit_code == 1
        && !producer_summaries
            .iter()
            .any(|summary| summary.status != "passed")
    {
        0
    } else {
        exit_code
    }
}

pub(super) fn normalize_finding_exit_code(exit_code: i32, lint_findings: &[HomeboyFinding]) -> i32 {
    if !lint_findings.is_empty() && exit_code == 0 {
        1
    } else {
        exit_code
    }
}

pub(super) fn effective_lint_exit_code(exit_code: i32, baseline_exit_override: Option<i32>) -> i32 {
    match baseline_exit_override {
        Some(0) if exit_code >= 2 => exit_code,
        Some(override_code) => override_code,
        None => exit_code,
    }
}
