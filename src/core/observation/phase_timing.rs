use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const PHASE_TIMING_SCHEMA: &str = "homeboy/phase-timing/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseTimingSpanStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseTimingSpan {
    pub id: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub duration_ms: u64,
    pub status: PhaseTimingSpanStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseTimingReport {
    pub schema: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub spans: Vec<PhaseTimingSpan>,
    pub timeline: Vec<super::timeline::ObservationEvent>,
}

#[derive(Debug)]
pub struct PhaseTimingRecorder {
    started_at: DateTime<Utc>,
    started_instant: Instant,
    spans: Vec<PhaseTimingSpan>,
}

#[derive(Debug)]
pub struct ActivePhaseTimingSpan {
    id: String,
    started_at_ms: u64,
    started_instant: Instant,
}

impl PhaseTimingRecorder {
    pub fn start() -> Self {
        Self {
            started_at: Utc::now(),
            started_instant: Instant::now(),
            spans: Vec::new(),
        }
    }

    pub fn begin(&self, id: impl Into<String>) -> ActivePhaseTimingSpan {
        ActivePhaseTimingSpan {
            id: id.into(),
            started_at_ms: elapsed_ms(self.started_instant),
            started_instant: Instant::now(),
        }
    }

    pub fn finish_span(&mut self, span: ActivePhaseTimingSpan) {
        self.finish_span_with_status(span, PhaseTimingSpanStatus::Success);
    }

    pub fn fail_span(&mut self, span: ActivePhaseTimingSpan) {
        self.finish_span_with_status(span, PhaseTimingSpanStatus::Error);
    }

    pub fn finish_span_with_status(
        &mut self,
        span: ActivePhaseTimingSpan,
        status: PhaseTimingSpanStatus,
    ) {
        let duration_ms = elapsed_ms(span.started_instant);
        self.spans.push(PhaseTimingSpan {
            id: span.id,
            started_at_ms: span.started_at_ms,
            finished_at_ms: span.started_at_ms.saturating_add(duration_ms),
            duration_ms,
            status,
        });
    }

    pub fn snapshot(&self) -> PhaseTimingReport {
        self.report(Utc::now(), elapsed_ms(self.started_instant))
    }

    pub fn finish(&self) -> PhaseTimingReport {
        self.snapshot()
    }

    fn report(&self, finished_at: DateTime<Utc>, duration_ms: u64) -> PhaseTimingReport {
        let mut timeline = Vec::new();
        for span in &self.spans {
            timeline.push(phase_event(
                span.started_at_ms,
                "start",
                &span.id,
                &span.status,
            ));
            timeline.push(phase_event(
                span.finished_at_ms,
                "finish",
                &span.id,
                &span.status,
            ));
        }

        PhaseTimingReport {
            schema: PHASE_TIMING_SCHEMA.to_string(),
            started_at: self.started_at.to_rfc3339(),
            finished_at: finished_at.to_rfc3339(),
            duration_ms,
            spans: self.spans.clone(),
            timeline,
        }
    }
}

pub fn merge_phase_timing(
    mut metadata: serde_json::Value,
    timing: PhaseTimingReport,
) -> serde_json::Value {
    if !metadata.is_object() {
        metadata = serde_json::json!({
            "homeboy_original_metadata": metadata,
        });
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("phase_timing".to_string(), serde_json::json!(timing));
    }
    metadata
}

fn phase_event(
    t_ms: u64,
    event: &str,
    id: &str,
    status: &PhaseTimingSpanStatus,
) -> super::timeline::ObservationEvent {
    let mut data = std::collections::BTreeMap::new();
    data.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    data.insert("status".to_string(), serde_json::json!(status));
    super::timeline::ObservationEvent {
        t_ms,
        source: "phase".to_string(),
        event: event.to_string(),
        data,
    }
}

fn elapsed_ms(instant: Instant) -> u64 {
    u64::try_from(instant.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_success_and_error_spans() {
        let mut recorder = PhaseTimingRecorder::start();
        let ok = recorder.begin("resolve");
        recorder.finish_span(ok);
        let err = recorder.begin("execute");
        recorder.fail_span(err);

        let report = recorder.finish();
        assert_eq!(report.schema, PHASE_TIMING_SCHEMA);
        assert_eq!(report.spans.len(), 2);
        assert_eq!(report.spans[0].id, "resolve");
        assert_eq!(report.spans[0].status, PhaseTimingSpanStatus::Success);
        assert_eq!(report.spans[1].id, "execute");
        assert_eq!(report.spans[1].status, PhaseTimingSpanStatus::Error);
        assert_eq!(report.timeline.len(), 4);
    }

    #[test]
    fn merge_phase_timing_preserves_existing_metadata() {
        let recorder = PhaseTimingRecorder::start();
        let merged = merge_phase_timing(
            serde_json::json!({ "source": "homeboy audit" }),
            recorder.finish(),
        );

        assert_eq!(merged["source"], "homeboy audit");
        assert_eq!(merged["phase_timing"]["schema"], PHASE_TIMING_SCHEMA);
    }
}
