//! Lab phase spans and non-timing selection metadata.
use homeboy_core::observation::ObservationStore;
use homeboy_core::phase_timing::PhaseTimer;

#[derive(Debug)]
pub(crate) struct OffloadObservation {
    pub(crate) timer: PhaseTimer,
    started_at: chrono::DateTime<chrono::Utc>,
    run_id: Option<String>,
    attempted: Option<serde_json::Value>,
    fallback_reason: Option<String>,
}

impl OffloadObservation {
    #[cfg(test)]
    pub(crate) fn start() -> Self {
        Self::for_run(None)
    }

    pub(crate) fn for_run(run_id: Option<String>) -> Self {
        Self {
            started_at: chrono::Utc::now(),
            timer: PhaseTimer::new(),
            run_id,
            attempted: None,
            fallback_reason: None,
        }
    }

    pub(crate) fn set_attempted(&mut self, runner_id: &str, source: &str, mode: Option<&str>) {
        self.attempted = Some(serde_json::json!({
            "runner_id": runner_id, "source": source, "mode": mode,
        }));
    }

    pub(crate) fn set_fallback_reason(&mut self, reason: &str) {
        self.fallback_reason = Some(reason.to_string());
    }
}

pub(crate) fn attach_offload_selection(
    metadata: &mut serde_json::Value,
    observation: &OffloadObservation,
) {
    if let Some(map) = metadata.as_object_mut() {
        if let Some(attempted) = &observation.attempted {
            map.insert("attempted_selection".to_string(), attempted.clone());
        }
        if let Some(reason) = &observation.fallback_reason {
            map.insert("fallback_reason".to_string(), serde_json::json!(reason));
        }
    }
}

impl Drop for OffloadObservation {
    fn drop(&mut self) {
        let Some(run_id) = &self.run_id else { return };
        let result = (|| -> homeboy_core::Result<()> {
            let store = ObservationStore::open_initialized()?;
            let report = self.timer.report();
            store.record_phase_spans(
                run_id,
                &format!(
                    "lab:{}:{}",
                    std::process::id(),
                    self.started_at.to_rfc3339()
                ),
                &report,
            )
        })();
        if let Err(error) = result {
            homeboy_core::log_status!("observe", "Lab phase timing persistence failed: {}", error);
        }
    }
}
