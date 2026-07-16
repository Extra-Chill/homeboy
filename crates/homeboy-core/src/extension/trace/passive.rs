//! Typed passive trace capture shared by standalone observation and trace workflows.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::{
    ActiveTraceProbes, TraceArtifact, TraceEvent, TraceProbeConfig, TraceResults, TraceStatus,
};

#[derive(Debug, Clone)]
pub struct PassiveTraceCapture {
    pub duration: Duration,
    pub probes: Vec<TraceProbeConfig>,
}

impl PassiveTraceCapture {
    pub fn capture(&self, component_id: String, scenario_id: String) -> Result<TraceResults> {
        if self.probes.is_empty() {
            return Err(Error::validation_invalid_argument(
                "probe",
                "passive trace capture requires at least one probe",
                None,
                None,
            ));
        }

        let started_at = Instant::now();
        let probes = ActiveTraceProbes::start(&self.probes)?;
        thread::sleep(self.duration);
        let mut timeline = vec![event(0, "trace.passive", "started")];
        timeline.extend(probes.stop());
        timeline.push(event(elapsed_ms(started_at), "trace.passive", "finished"));
        timeline.sort_by_key(|event| event.t_ms);

        Ok(TraceResults {
            component_id,
            scenario_id,
            status: TraceStatus::Pass,
            summary: Some("Passive trace timeline".to_string()),
            failure: None,
            rig: None,
            evidence: None,
            timeline,
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::<TraceArtifact>::new(),
            metrics: Default::default(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            preview: None,
        })
    }
}

fn event(t_ms: u64, source: &str, event: &str) -> TraceEvent {
    TraceEvent {
        t_ms,
        source: source.to_string(),
        event: event.to_string(),
        data: BTreeMap::new(),
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_rejects_an_empty_probe_set() {
        let error = PassiveTraceCapture {
            duration: Duration::ZERO,
            probes: Vec::new(),
        }
        .capture("homeboy".to_string(), "passive".to_string())
        .expect_err("capture should require a probe");

        assert!(error.to_string().contains("at least one probe"));
    }

    #[test]
    fn capture_emits_typed_probe_and_lifecycle_events() {
        let pattern = std::env::current_exe()
            .expect("current executable")
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("homeboy")
            .to_string();
        let result = PassiveTraceCapture {
            duration: Duration::from_millis(25),
            probes: vec![TraceProbeConfig::ProcessSnapshot {
                pattern,
                interval_ms: Some(5),
            }],
        }
        .capture("homeboy".to_string(), "passive".to_string())
        .expect("capture passive trace");

        assert_eq!(result.scenario_id, "passive");
        assert!(result
            .timeline
            .iter()
            .any(|event| { event.source == "trace.passive" && event.event == "started" }));
        assert!(result
            .timeline
            .iter()
            .any(|event| { event.source == "process.snapshot" && event.event == "proc.list" }));
    }
}
