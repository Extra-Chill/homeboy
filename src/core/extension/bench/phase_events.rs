use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::parsing::BenchResults;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseEvent {
    /// Generic phase identifier. Core treats this as a label, not a closed
    /// enum, so extensions can model their own lifecycle.
    pub phase: String,
    /// Event status, for example `started`, `heartbeat`, `completed`,
    /// `failed`, or `timeout`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub diagnostics: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payload: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseSummary {
    pub phase: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub heartbeat_count: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub diagnostic_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseFailureClassification {
    /// `timeout` or `failure` for classified terminal events.
    pub kind: String,
    pub phase: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn evaluate_phase_events(results: &mut BenchResults) {
    results.phase_summaries = summarize_phase_events(&results.phase_events);
    results.failure_classification = classify_phase_failure(&results.phase_events);
}

pub fn summarize_phase_events(events: &[BenchPhaseEvent]) -> Vec<BenchPhaseSummary> {
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
