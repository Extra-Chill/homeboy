//! Operator-facing rendering of a differential verdict.
//!
//! The block is deliberately four short lines, because it is read *while
//! waiting on something else*:
//!
//! ```text
//! candidate: 202 passed, 3 failed
//! baseline:  197 passed, 3 failed   (cached, origin/main @ 1a2b3c4de)
//! verdict:   baseline_red — the 3 failures reproduce unchanged on origin/main
//!            new failures: 0
//! ```
//!
//! Two things are load-bearing about that shape:
//!
//! * The baseline line always says whether it came from cache and which
//!   revision it describes. A comparison against an unnamed baseline is not
//!   checkable, and an unnamed *stale* baseline is worse than none.
//! * `new failures` is always printed, including when it is zero. Zero new
//!   failures is the answer the reader came for; leaving it implicit means
//!   inferring it from two counts, which is exactly the manual arithmetic this
//!   whole feature exists to remove.

use super::{
    ComparisonBasis, DifferentialReport, DifferentialVerdict, RunOutcome, TestMeasurement,
};

/// Label column, padded to a common width so the two count lines align.
const CANDIDATE_LABEL: &str = "candidate: ";
const BASELINE_LABEL: &str = "baseline:  ";
const VERDICT_LABEL: &str = "verdict:   ";
/// Continuation indent, the same width as the labels above.
const CONTINUATION_INDENT: &str = "           ";

/// Longest failure-name list printed before it is truncated with a count.
const MAX_LISTED_FAILURES: usize = 10;

pub fn render_report(report: &DifferentialReport) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "{CANDIDATE_LABEL}{}",
        describe_measurement(&report.candidate)
    ));
    lines.push(format!("{BASELINE_LABEL}{}", describe_baseline(report)));
    lines.push(format!(
        "{VERDICT_LABEL}{} — {}",
        report.verdict.as_str(),
        report.explanation
    ));
    lines.push(format!(
        "{CONTINUATION_INDENT}new failures: {}",
        report.new_failures.len()
    ));

    for line in name_lines(&report.new_failures) {
        lines.push(format!("{CONTINUATION_INDENT}  {line}"));
    }
    if !report.fixed_failures.is_empty() {
        lines.push(format!(
            "{CONTINUATION_INDENT}fixed on this branch: {}",
            report.fixed_failures.len()
        ));
    }
    if report.basis == ComparisonBasis::FailureCounts {
        lines.push(format!(
            "{CONTINUATION_INDENT}compared by count only — at least one side did not name its \
             failures"
        ));
    }
    if report.verdict == DifferentialVerdict::NoBaseline {
        lines.push(format!(
            "{CONTINUATION_INDENT}record a baseline from a checkout of {} to make this \
             comparison cheap for every branch cut from that revision",
            report.reference
        ));
    }

    lines.join("\n")
}

fn describe_measurement(measurement: &TestMeasurement) -> String {
    match (&measurement.counts, measurement.outcome) {
        (Some(counts), _) => format!("{} passed, {} failed", counts.passed, counts.failed),
        (None, RunOutcome::TimedOut) => {
            format!("no counts (timed out, exit {})", measurement.exit_code)
        }
        (None, _) if measurement.structured_output => format!(
            "{} failed (no totals reported)",
            measurement.failed_tests.len()
        ),
        (None, _) => format!("no structured counts (exit {})", measurement.exit_code),
    }
}

fn describe_baseline(report: &DifferentialReport) -> String {
    match report.baseline.as_ref() {
        Some(evidence) => format!(
            "{}   (cached, {} @ {}, recorded {})",
            describe_measurement(&evidence.measurement),
            report.reference,
            short_revision(&evidence.revision),
            evidence.recorded_at
        ),
        None => format!(
            "none cached for {}{}",
            report.reference,
            requested_suffix(report)
        ),
    }
}

fn requested_suffix(report: &DifferentialReport) -> String {
    match report.requested_revision.as_deref() {
        Some(revision) => format!(" @ {}", short_revision(revision)),
        None => String::new(),
    }
}

fn name_lines(names: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = names
        .iter()
        .take(MAX_LISTED_FAILURES)
        .map(|name| format!("- {name}"))
        .collect();
    if names.len() > MAX_LISTED_FAILURES {
        lines.push(format!(
            "- ... and {} more",
            names.len() - MAX_LISTED_FAILURES
        ));
    }
    lines
}

/// First nine characters of a revision, matching the short form the rest of
/// Homeboy's human-facing output uses. Sliced by character so a non-hex
/// identifier cannot panic on a byte boundary.
fn short_revision(revision: &str) -> String {
    revision.chars().take(9).collect()
}
