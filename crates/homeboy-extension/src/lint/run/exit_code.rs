//! Lint exit-code normalization — reconciles runner exit codes with finding
//! counts, producer statuses, and baseline overrides.

use homeboy_core::finding::{FindingProducerSummary, HomeboyFinding};

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

pub(super) fn normalize_producer_exit_code(
    exit_code: i32,
    producer_summaries: &[FindingProducerSummary],
) -> i32 {
    if exit_code >= 2 || producer_summaries.is_empty() {
        return exit_code;
    }

    if producer_summaries
        .iter()
        .all(|summary| summary.status == "passed")
    {
        0
    } else if exit_code == 0 {
        1
    } else {
        exit_code
    }
}

/// Apply the baseline ratchet to a lint exit code.
///
/// `Some(0)` here rewrites a non-zero lint exit to zero, which is what makes
/// the ratchet a ratchet: standing debt recorded in the baseline does not block
/// a change that did not add to it. `hard_error` is the existing floor, so an
/// infrastructure failure can never be ratcheted away.
///
/// **This function is not measurement-guarded, deliberately and knowingly**
/// (#10685). Two states reach `Some(0)` from `process_baseline` and neither is
/// distinguishable in the rendered verdict:
///
///   * findings identical to the baseline — "No change from baseline". The
///     result is `status: passed` with the standing findings still present and
///     nothing labelling them. This is the same defect the differential gate
///     had (#10657): a ratchet is defensible, but rendering it as `pass` rather
///     than as its own status means the standing breakage is never reported.
///   * an empty findings set against a non-empty baseline — reported as
///     "Drift reduced: N finding(s) resolved" and rendered green. A lint
///     runner that exits 0 having written an empty findings sidecar produces
///     exactly this shape, so an *absence* of findings is actively read as
///     *evidence of improvement*.
///
/// The shared predicate is not applied here because doing so correctly means
/// changing what a lint baseline verdict *is*, not just guarding it — and the
/// release lint gate became baseline-aware in #10678, so the blast radius of
/// getting that wrong is every release. Recorded rather than patched.
pub(super) fn effective_lint_exit_code(
    exit_code: i32,
    baseline_exit_override: Option<i32>,
    hard_error: bool,
) -> i32 {
    match baseline_exit_override {
        Some(0) if hard_error => exit_code.max(1),
        Some(override_code) => override_code,
        None => exit_code,
    }
}
