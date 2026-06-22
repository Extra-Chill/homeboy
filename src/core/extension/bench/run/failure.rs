//! Bench failure classification.

use crate::core::extension::bench::phase_events::BenchPhaseFailureClassification;
use crate::core::extension::bench::responsiveness::BenchResponsivenessSummary;

pub(crate) fn classify_bench_failure(
    success: bool,
    exit_code: i32,
    stderr: &str,
    responsiveness: Option<&BenchResponsivenessSummary>,
) -> Option<BenchPhaseFailureClassification> {
    if success {
        return None;
    }

    if responsiveness.is_some_and(BenchResponsivenessSummary::responsiveness_lost) {
        return Some(classification(
            "responsiveness_loss",
            "responsiveness",
            "missed_ping",
            responsiveness.and_then(|summary| {
                summary.last_ping_at.as_ref().map(|last| {
                    format!(
                        "UI responsiveness ping gap exceeded {}ms; last ping at {}",
                        summary.missed_ping_window_ms, last
                    )
                })
            }),
        ));
    }

    let stderr_lower = stderr.to_ascii_lowercase();
    if exit_code == 124 || stderr_lower.contains("timeout") || stderr_lower.contains("timed out") {
        return Some(classification("timeout", "bench", "timeout", None));
    }

    Some(classification("assertion_failure", "bench", "failed", None))
}

pub(crate) fn classification(
    kind: &str,
    phase: &str,
    status: &str,
    message: Option<String>,
) -> BenchPhaseFailureClassification {
    BenchPhaseFailureClassification {
        kind: kind.to_string(),
        phase: phase.to_string(),
        status: status.to_string(),
        message,
    }
}
