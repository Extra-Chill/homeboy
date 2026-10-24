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
    pub fn run_phase_guard(&self, run_id: &str, phase: &str) -> RunPhaseGuard<'_> {
        RunPhaseGuard {
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
        if self
            .phase_spans(run_id)?
            .spans
            .iter()
            .any(|span| span.id == phase)
        {
            return Ok(());
        }
        let Some(run) = self.get_run(run_id)? else {
            return Ok(());
        };
        let origin = chrono::DateTime::parse_from_rfc3339(&run.started_at)
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
        self.record_phase_spans(run_id, &format!("whole-run:{phase}"), &report)
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
                instance: None,
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
            report.spans.extend(recorded.spans);
        }
        Ok(report)
    }
}
