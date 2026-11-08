//! The shared write/read path for durable phase telemetry. No metadata copies
//! or artifact scans are needed; reads never reconcile or contact a runner.
use homeboy_control_plane_contract::{
    ControlPlaneEventAppendRequest, ControlPlaneEventSource, RunId,
    CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
};
use homeboy_engine_primitives::phase_timing::{
    PhaseTimingReport, MAX_PHASE_SPANS_PER_EVENT, PHASE_SPANS_EVENT_KIND,
};

use super::ObservationStore;
use crate::{Error, Result};

/// Best-effort scope timing, including early returns. Never changes domain results.
pub struct RunPhaseGuard<'a> {
    store: &'a ObservationStore,
    run_id: String,
    phase: String,
    started_at: String,
    _context: super::phase_timing::Scope,
}

impl Drop for RunPhaseGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.store.finish_run_phase(
            &self.run_id,
            &self.phase,
            &self.started_at,
            std::thread::panicking(),
        ) {
            crate::log_status!("observe", "phase timing persistence failed: {}", error);
        }
    }
}

impl ObservationStore {
    /// Import runner timing evidence onto the controller owner timeline. Event
    /// identities make replay idempotent; callers validate the accepted job
    /// binding before importing.
    pub fn record_runner_phase_spans(
        &self,
        owner_run_id: &str,
        job_id: &str,
        events: &[crate::api_jobs::JobEvent],
    ) -> Result<()> {
        for event in events {
            let Some(data) = event.data.as_ref() else {
                continue;
            };
            let Some(value) = data.get("phase_spans") else {
                continue;
            };
            let report: PhaseTimingReport = serde_json::from_value(value.clone())
                .map_err(|error| Error::internal_unexpected(error.to_string()))?;
            let identity = format!("runner:{job_id}:{}", event.sequence);
            self.record_phase_spans(owner_run_id, &identity, &report)?;
        }
        Ok(())
    }

    pub fn run_phase_guard(&self, run_id: &str, phase: &str) -> RunPhaseGuard<'_> {
        RunPhaseGuard {
            _context: super::phase_timing::for_run(self, run_id),
            store: self,
            run_id: run_id.into(),
            phase: phase.into(),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }

    /// Time an operation and append its outcome even when the operation fails.
    /// Persistence failure is reported separately and never masks a workflow
    /// error. Absolute timestamps share a timeline across controller and workers.
    pub fn time_run_phase<T>(
        &self,
        run_id: &str,
        phase: &str,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.time_run_phase_with_outcome(run_id, phase, operation, |_| false)
    }

    /// As `time_run_phase`, for domain results that encode failure in `Ok`.
    pub fn time_run_phase_with_outcome<T>(
        &self,
        run_id: &str,
        phase: &str,
        operation: impl FnOnce() -> Result<T>,
        failed: impl FnOnce(&T) -> bool,
    ) -> Result<T> {
        // A run-aware execution boundary also owns the context of all leaves.
        let _context = super::phase_timing::for_run(self, run_id);
        let started = chrono::Utc::now();
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::with_version(
            crate::build_identity::current().version,
        );
        let result = timer.time(phase, operation);
        let mut report = timer.into_report();
        if result.as_ref().is_ok_and(failed) {
            report.spans[0].status = homeboy_engine_primitives::phase_timing::PhaseStatus::Failed;
        }
        report.spans[0].started_at = started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        // Distinct executions are distinct samples; exactly-once delivery uses
        // the stable report identity below rather than a phase name alone.
        let identity = format!("{phase}:{}:{}", std::process::id(), started.to_rfc3339());
        if let Err(error) = self.record_phase_spans(run_id, &identity, &report) {
            crate::log_status!("observe", "phase timing persistence failed: {}", error);
        }
        result
    }

    /// Finish a phase whose start must survive process restarts. The owner keeps
    /// only the start timestamp in recovery state and calls this at completion.
    pub fn finish_run_phase(
        &self,
        run_id: &str,
        phase: &str,
        started_at: &str,
        failed: bool,
    ) -> Result<()> {
        let parse = |value: &str| {
            chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|error| Error::internal_unexpected(error.to_string()))
        };
        let started = parse(started_at)?;
        let timestamp = started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if self
            .phase_spans(run_id)?
            .spans
            .iter()
            .any(|span| span.id == phase && span.started_at == timestamp)
        {
            return Ok(());
        }
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::with_version(
            crate::build_identity::current().version,
        );
        let duration = chrono::Utc::now()
            .signed_duration_since(started)
            .to_std()
            .unwrap_or_default();
        if failed {
            timer.record_failed(phase, duration);
        } else {
            timer.record_ok(phase, duration);
        }
        let mut report = timer.into_report();
        report.spans[0].started_at = timestamp;
        self.record_phase_spans(run_id, &format!("{phase}:{started_at}"), &report)
    }

    /// Record a whole-run phase for workflows whose lifecycle spans processes.
    /// Unlike a scoped monotonic timer this uses the durable run origin. It is
    /// explicitly an inclusive owner span, not an additional sequential phase.
    pub fn record_run_phase(&self, run_id: &str, phase: &str, failed: bool) -> Result<()> {
        let spans = self.phase_spans(run_id)?;
        if spans.spans.iter().any(|span| {
            span.id == phase
                && (span.status == homeboy_engine_primitives::phase_timing::PhaseStatus::Failed)
                    == failed
        }) {
            return Ok(());
        }
        let Some(run) = self.get_run(run_id)? else {
            return Ok(());
        };
        let started_at = spans
            .spans
            .iter()
            .find(|span| span.id == phase)
            .map_or(run.started_at.as_str(), |span| span.started_at.as_str());
        let origin = chrono::DateTime::parse_from_rfc3339(started_at)
            .map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let duration = chrono::Utc::now()
            .signed_duration_since(origin)
            .to_std()
            .unwrap_or_default();
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::with_version(
            crate::build_identity::current().version,
        );
        if failed {
            timer.record_failed(phase, duration);
        } else {
            timer.record_ok(phase, duration);
        }
        let mut report = timer.into_report();
        report.spans[0].started_at = origin.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.record_phase_spans(
            run_id,
            &format!("whole-run:{phase}:{}", chrono::Utc::now().to_rfc3339()),
            &report,
        )
    }

    /// Append one completed timing report. The caller supplies a stable identity
    /// for this operation/attempt, so delivery retries cannot double-count it.
    /// Reports are capped per run as well as per event; progress heartbeats do
    /// not evict phase evidence or grow its durable footprint without bound.
    pub fn record_phase_spans(
        &self,
        run_id: &str,
        operation_id: &str,
        report: &PhaseTimingReport,
    ) -> Result<()> {
        if report.is_empty() {
            return Ok(());
        }
        if report.spans.len() > MAX_PHASE_SPANS_PER_EVENT
            || report.spans.iter().any(|span| {
                span.id.is_empty()
                    || span.id.len() > 128
                    || span.homeboy_version.len() > 128
                    || chrono::DateTime::parse_from_rfc3339(&span.started_at).is_err()
                    || span
                        .duration_ms
                        .is_some_and(|ms| !ms.is_finite() || ms < 0.0)
            })
        {
            return Err(Error::validation_invalid_argument(
                "phase_spans",
                "invalid or oversized phase timing report",
                None,
                None,
            ));
        }
        let run =
            RunId::new(run_id).map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let identity = homeboy_engine_primitives::content_hash::sha256_hex(
            format!("phase-spans\0{run_id}\0{operation_id}").as_bytes(),
        );
        let request = ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: format!("phase-spans:{identity}"),
            actor: "homeboy:phase-timer".to_string(),
            kind: PHASE_SPANS_EVENT_KIND.to_string(),
            source: ControlPlaneEventSource {
                component: "phase-timer".to_string(),
                instance: operation_id
                    .starts_with("whole-run:")
                    .then(|| "whole-run".to_string()),
            },
            occurred_at: None,
            task: None,
            attempt: None,
            execution: None,
            data: serde_json::to_value(report)
                .map_err(|error| Error::internal_unexpected(error.to_string()))?,
            artifacts: vec![],
            evidence: vec![],
        };
        let digest = homeboy_engine_primitives::content_hash::sha256_hex(
            &serde_json::to_vec(&request)
                .map_err(|error| Error::internal_unexpected(error.to_string()))?,
        );
        self.append_control_plane_event(&run, &request, &identity, &digest)?;
        Ok(())
    }

    /// Read locally persisted timing reports, including reports older than the
    /// progress retention cursor. This is a projection of the run's ledger.
    pub fn phase_spans(&self, run_id: &str) -> Result<PhaseTimingReport> {
        let run =
            RunId::new(run_id).map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let mut report = PhaseTimingReport::default();
        for event in self.durable_control_plane_events(&run, PHASE_SPANS_EVENT_KIND)? {
            let recorded: PhaseTimingReport = serde_json::from_value(event.data)
                .map_err(|error| Error::internal_unexpected(error.to_string()))?;
            for span in recorded.spans {
                if let Some(previous) = report.spans.iter_mut().find(|previous| {
                    event.source.instance.as_deref() == Some("whole-run")
                        && previous.id == span.id
                        && previous.started_at == span.started_at
                }) {
                    *previous = span;
                } else {
                    report.spans.push(span);
                }
            }
        }
        if let Some(run) = self.get_run(run_id)? {
            project_provider(&mut report, &run.metadata_json);
        }
        Ok(report)
    }
}

// Only unambiguous command/outcome pairs can share an aggregate clock. Promotion
// gates already carry their own timestamps and must not be moved inside it.
fn project_provider(report: &mut PhaseTimingReport, metadata: &serde_json::Value) {
    let commands: Vec<_> = report
        .spans
        .iter()
        .filter(|s| s.id == "provider.command")
        .cloned()
        .collect();
    let Some(outcomes) = metadata
        .pointer("/agent_task_aggregate/outcomes")
        .and_then(|v| v.as_array())
    else {
        return;
    };
    if commands.len() != 1 || outcomes.len() != 1 {
        return;
    }
    let Some(measured) = outcomes[0].get("metadata") else {
        return;
    };
    let mut parts = Vec::new();
    if let Some(ms) = measured["model_ms"].as_f64() {
        parts.push(("provider.model".to_string(), ms));
    }
    if let Some(tools) = measured["tool_ms_by_name"].as_object() {
        for (name, ms) in tools {
            if let Some(ms) = ms.as_f64() {
                parts.push((format!("provider.tool.{name}"), ms));
            }
        }
    } else if let Some(ms) = measured["tool_ms"].as_f64() {
        parts.push(("provider.tool.unknown".into(), ms));
    }
    let total: f64 = parts.iter().map(|(_, ms)| ms).sum();
    if parts.iter().any(|(_, ms)| !ms.is_finite() || *ms < 0.0)
        || total > commands[0].duration_ms.unwrap_or_default()
    {
        return;
    }
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(&commands[0].started_at) else {
        return;
    };
    let mut offset = 0.0;
    for (id, ms) in parts {
        if !report.spans.iter().any(|s| s.id == id) {
            let mut span = commands[0].clone();
            span.id = id;
            span.duration_ms = Some(ms);
            span.started_at = (start + chrono::Duration::microseconds((offset * 1000.0) as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
            report.spans.push(span);
        }
        offset += ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_cook_aggregate_splits_provider_through_public_hotspots() {
        use crate::performance_hotspots::{phase_hotspots, PhaseHotspotQuery};
        use homeboy_engine_primitives::phase_timing::PhaseTimer;
        let aggregate: serde_json::Value = serde_json::from_str(include_str!("../../../homeboy-agents/src/agent_task_service/cook_tests/fixtures/incomplete_no_patch.json")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("timing.db")).unwrap();
        let run = store
            .start_run(
                super::super::NewRunRecord::builder("agent-task")
                    .metadata(serde_json::json!({"agent_task_aggregate":aggregate}))
                    .build(),
            )
            .unwrap();
        let mut timer = PhaseTimer::new();
        timer.record_ok("provider.command", std::time::Duration::from_secs(900));
        store
            .record_phase_spans(&run.id, "provider", &timer.into_report())
            .unwrap();
        let first = store.phase_spans(&run.id).unwrap();
        assert_eq!(first, store.phase_spans(&run.id).unwrap());
        let hotspots = phase_hotspots(
            &store,
            &PhaseHotspotQuery {
                kind: Some("agent-task".into()),
                last: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(hotspots
            .phases
            .iter()
            .any(|row| row.phase == "provider.model" && row.timing.p50_ms == 392072.0));
        assert!(hotspots
            .phases
            .iter()
            .any(|row| row.phase.starts_with("provider.tool.") && row.self_ms_total > 0.0));
        let total: f64 = hotspots.phases.iter().map(|row| row.self_ms_total).sum();
        assert!((total - 900000.0).abs() < 1.0);
    }

    #[test]
    fn whole_run_phase_tracks_latest_outcome_without_changing_matching_spans() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("timing.db")).unwrap();
        let run = store
            .start_run(super::super::NewRunRecord::builder("test").build())
            .unwrap();
        for failed in [true, false, true] {
            store.record_run_phase(&run.id, "cook", failed).unwrap();
            let report = store.phase_spans(&run.id).unwrap();
            assert_eq!(report.spans.len(), 1);
            assert_eq!(
                report.spans[0].status
                    == homeboy_engine_primitives::phase_timing::PhaseStatus::Failed,
                failed
            );
            assert_eq!(
                report.spans[0].started_at,
                chrono::DateTime::parse_from_rfc3339(&run.started_at)
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            );
            store.record_run_phase(&run.id, "cook", failed).unwrap();
            assert_eq!(
                serde_json::to_value(store.phase_spans(&run.id).unwrap()).unwrap(),
                serde_json::to_value(report).unwrap()
            );
        }
    }
}
