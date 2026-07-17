use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::parsing::BenchResults;
pub use homeboy_extension_contract::bench_diagnostics::{
    BenchPhaseEvent, BenchPhaseFailureClassification, BenchPhaseSummary,
};

pub fn evaluate_phase_events(results: &mut BenchResults) {
    results.phase_summaries = summarize_phase_events(&results.phase_events);
    if let Some(failure_classification) = classify_phase_failure(&results.phase_events) {
        results.failure_classification = Some(failure_classification);
    }
}

fn summarize_phase_events(events: &[BenchPhaseEvent]) -> Vec<BenchPhaseSummary> {
    let mut summaries: Vec<BenchPhaseSummary> = Vec::new();
    for event in events {
        let Some(summary) = summaries
            .iter_mut()
            .find(|summary| summary.phase == event.phase)
        else {
            summaries.push(summary_from_phase_event(event));
            continue;
        };
        update_phase_summary(summary, event);
    }
    summaries
}

fn summary_from_phase_event(event: &BenchPhaseEvent) -> BenchPhaseSummary {
    let mut summary = BenchPhaseSummary {
        phase: event.phase.clone(),
        status: normalized_phase_status(&event.status),
        first_t_ms: event.t_ms,
        last_t_ms: event.t_ms,
        started_at: event.started_at.clone(),
        ended_at: event.ended_at.clone(),
        duration_ms: event.duration_ms,
        heartbeat_count: 0,
        diagnostic_count: event.diagnostics.len(),
        message: event.message.clone(),
    };
    if is_heartbeat_status(&event.status) {
        summary.heartbeat_count = 1;
    }
    summary
}

fn update_phase_summary(summary: &mut BenchPhaseSummary, event: &BenchPhaseEvent) {
    summary.status = normalized_phase_status(&event.status);
    if let Some(t_ms) = event.t_ms {
        summary.first_t_ms = Some(summary.first_t_ms.map_or(t_ms, |first| first.min(t_ms)));
        summary.last_t_ms = Some(summary.last_t_ms.map_or(t_ms, |last| last.max(t_ms)));
    }
    if summary.started_at.is_none() {
        summary.started_at = event.started_at.clone();
    }
    if event.ended_at.is_some() {
        summary.ended_at = event.ended_at.clone();
    }
    if event.duration_ms.is_some() {
        summary.duration_ms = event.duration_ms;
    } else if let (Some(first), Some(last)) = (summary.first_t_ms, summary.last_t_ms) {
        summary.duration_ms = Some(last.saturating_sub(first));
    }
    if is_heartbeat_status(&event.status) {
        summary.heartbeat_count += 1;
    }
    summary.diagnostic_count += event.diagnostics.len();
    if event.message.is_some() {
        summary.message = event.message.clone();
    }
}

fn classify_phase_failure(events: &[BenchPhaseEvent]) -> Option<BenchPhaseFailureClassification> {
    events
        .iter()
        .find(|event| is_timeout_status(&event.status))
        .map(|event| classification_from_event("timeout", event))
        .or_else(|| {
            events
                .iter()
                .find(|event| is_failure_status(&event.status))
                .map(|event| classification_from_event("failure", event))
        })
}

fn classification_from_event(
    kind: &str,
    event: &BenchPhaseEvent,
) -> BenchPhaseFailureClassification {
    BenchPhaseFailureClassification {
        kind: kind.to_string(),
        phase: event.phase.clone(),
        status: normalized_phase_status(&event.status),
        message: event.message.clone(),
    }
}

fn normalized_phase_status(status: &str) -> String {
    status.trim().to_ascii_lowercase().replace('-', "_")
}

fn is_heartbeat_status(status: &str) -> bool {
    normalized_phase_status(status) == "heartbeat"
}

fn is_timeout_status(status: &str) -> bool {
    matches!(
        normalized_phase_status(status).as_str(),
        "timeout" | "timed_out"
    )
}

fn is_failure_status(status: &str) -> bool {
    matches!(
        normalized_phase_status(status).as_str(),
        "failed" | "failure" | "error"
    )
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_phase_events_and_classifies_timeout() {
        let events = vec![
            BenchPhaseEvent {
                phase: "dependency_preparation".to_string(),
                status: "started".to_string(),
                t_ms: Some(0),
                ..BenchPhaseEvent::default()
            },
            BenchPhaseEvent {
                phase: "dependency_preparation".to_string(),
                status: "heartbeat".to_string(),
                t_ms: Some(1000),
                message: Some("installing".to_string()),
                ..BenchPhaseEvent::default()
            },
            BenchPhaseEvent {
                phase: "dependency_preparation".to_string(),
                status: "timeout".to_string(),
                t_ms: Some(2000),
                message: Some("dependency install exceeded budget".to_string()),
                diagnostics: BTreeMap::from([("budget_ms".to_string(), serde_json::json!(2000))]),
                payload: BTreeMap::from([(
                    "operation".to_string(),
                    serde_json::json!("dependency_install"),
                )]),
                ..BenchPhaseEvent::default()
            },
        ];

        let summaries = summarize_phase_events(&events);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, "timeout");
        assert_eq!(summaries[0].duration_ms, Some(2000));
        assert_eq!(summaries[0].heartbeat_count, 1);
        assert_eq!(summaries[0].diagnostic_count, 1);

        let classification = classify_phase_failure(&events).expect("classification");
        assert_eq!(classification.kind, "timeout");
        assert_eq!(classification.phase, "dependency_preparation");
        assert_eq!(
            classification.message.as_deref(),
            Some("dependency install exceeded budget")
        );
    }
}
