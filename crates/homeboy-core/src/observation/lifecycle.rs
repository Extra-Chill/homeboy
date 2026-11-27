use std::path::Path;

use super::{NewFindingRecord, NewRunRecord, ObservationStore, RunRecord, RunStatus};

const RUN_OWNER_METADATA_KEY: &str = "homeboy_run_owner";
pub const ACTIVE_RUN_ID_ENV: &str = "HOMEBOY_ACTIVE_RUN_ID";

pub struct ActiveObservation {
    store: ObservationStore,
    run: RunRecord,
    initial_metadata: serde_json::Value,
}

impl ActiveObservation {
    pub fn start(record: NewRunRecord) -> crate::Result<Self> {
        let store = ObservationStore::open_initialized()?;
        let initial_metadata = record.metadata_json.clone();
        let run = store.start_run(record)?;
        std::env::set_var(ACTIVE_RUN_ID_ENV, &run.id);

        Ok(Self {
            store,
            run,
            initial_metadata,
        })
    }

    pub fn start_best_effort(record: NewRunRecord) -> Option<Self> {
        Self::start(record).ok()
    }

    /// Start an observation under a caller-owned, stable run id.
    ///
    /// Re-running the same id restarts the row as a fresh attempt (see
    /// [`ObservationStore::restart_run_with_id`]); an id owned by a run of
    /// another kind is refused.
    pub fn start_with_stable_id(record: NewRunRecord, run_id: &str) -> crate::Result<Self> {
        let store = ObservationStore::open_initialized()?;
        let initial_metadata = record.metadata_json.clone();
        let run = store.restart_run_with_id(record, run_id.to_string())?;
        std::env::set_var(ACTIVE_RUN_ID_ENV, &run.id);

        Ok(Self {
            store,
            run,
            initial_metadata,
        })
    }

    /// Reopen a durable running observation in a detached worker.
    pub fn resume(run_id: &str) -> crate::Result<Self> {
        let store = ObservationStore::open_initialized()?;
        let run = store.get_run(run_id)?.ok_or_else(|| {
            crate::Error::validation_invalid_argument(
                "run_id",
                "run record not found",
                Some(run_id.to_string()),
                None,
            )
        })?;
        if run.status != RunStatus::Running.as_str() {
            return Err(crate::Error::validation_invalid_argument(
                "run_id",
                "only a running observation can be resumed",
                Some(run_id.to_string()),
                None,
            ));
        }
        Ok(Self {
            initial_metadata: run.metadata_json.clone(),
            store,
            run,
        })
    }

    pub fn store(&self) -> &ObservationStore {
        &self.store
    }

    pub fn run(&self) -> &RunRecord {
        &self.run
    }

    pub fn run_id(&self) -> &str {
        &self.run.id
    }

    pub fn component_id(&self) -> Option<&str> {
        self.run.component_id.as_deref()
    }

    pub fn rig_id(&self) -> Option<&str> {
        self.run.rig_id.as_deref()
    }

    pub fn initial_metadata(&self) -> &serde_json::Value {
        &self.initial_metadata
    }

    pub fn finish(&self, status: RunStatus, metadata: Option<serde_json::Value>) {
        let _ = self.store.finish_run(self.run_id(), status, metadata);
    }

    pub fn finish_with_merged_metadata(&self, status: RunStatus, metadata: serde_json::Value) {
        self.finish(
            status,
            Some(merge_metadata(self.initial_metadata.clone(), metadata)),
        );
    }

    /// Terminalize the run with merged metadata and surface a persistence
    /// failure, for workloads whose evidence contract makes the terminal write
    /// part of the result rather than a best-effort side effect.
    pub fn try_finish_with_merged_metadata(
        &self,
        status: RunStatus,
        metadata: serde_json::Value,
    ) -> crate::Result<RunRecord> {
        self.store.finish_run(
            self.run_id(),
            status,
            Some(merge_metadata(self.initial_metadata.clone(), metadata)),
        )
    }

    pub fn finish_error(&self, metadata: Option<serde_json::Value>) {
        self.finish(RunStatus::Error, metadata);
    }

    pub fn finish_error_with_merged_metadata(&self, metadata: serde_json::Value) {
        self.finish_with_merged_metadata(RunStatus::Error, metadata);
    }

    pub fn record_findings(&self, records: &[NewFindingRecord]) {
        let _ = self.store.record_findings(records);
    }

    pub fn record_artifact_if_file(&self, kind: &str, path: &Path) {
        if path.is_file() {
            let _ = self.store.record_artifact(self.run_id(), kind, path);
        }
    }

    pub fn store_path(&self) -> String {
        self.store
            .status()
            .map(|status| status.path)
            .unwrap_or_else(|_| "<unavailable>".to_string())
    }
}

/// Best-effort persistence of a run's terminal state.
///
/// Encapsulates the `ObservationStore::finish_run` call plus the deliberate
/// drop of its result so command/orchestration layers can hand off a computed
/// status and finalized metadata without re-implementing the
/// observation-persistence primitive. Failures to persist are intentionally
/// swallowed: finishing a run is an observability side effect that must never
/// abort the workflow it is recording.
pub fn finish_run_best_effort(
    store: &ObservationStore,
    run_id: &str,
    status: RunStatus,
    metadata: Option<serde_json::Value>,
) {
    let _ = store.finish_run(run_id, status, metadata);
}

pub fn merge_metadata(
    mut initial: serde_json::Value,
    finish: serde_json::Value,
) -> serde_json::Value {
    if let (Some(initial), Some(finish)) = (initial.as_object_mut(), finish.as_object()) {
        for (key, value) in finish {
            initial.insert(key.clone(), value.clone());
        }
    }
    initial
}

pub fn running_status_note(run: &RunRecord) -> Option<String> {
    if run.status != RunStatus::Running.as_str() {
        return None;
    }

    if run_has_active_remote_job(run) {
        return None;
    }

    if run
        .metadata_json
        .pointer("/homeboy_ownership_handoff/state")
        .and_then(serde_json::Value::as_str)
        == Some("transferring")
    {
        return Some(
            "detached worker ownership transfer is pending until its handoff deadline".to_string(),
        );
    }

    let Some(owner_pid) = run_owner_pid(run) else {
        return Some(
            "running status has no owner metadata; run may predate reconciliation support"
                .to_string(),
        );
    };

    if crate::process::pid_is_running(owner_pid) {
        None
    } else {
        Some(
            "owner process is not running; run may be stale; run `homeboy runs reconcile`"
                .to_string(),
        )
    }
}

pub fn run_has_active_remote_job(run: &RunRecord) -> bool {
    run.metadata_json
        .pointer("/lab/remote_job_status")
        .or_else(|| run.metadata_json.pointer("/lab/remote_job/status"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| matches!(status, "queued" | "running"))
}

pub fn run_owner_pid(run: &RunRecord) -> Option<u32> {
    run.metadata_json
        .get(RUN_OWNER_METADATA_KEY)
        .and_then(|owner| owner.get("pid"))
        .or_else(|| run.metadata_json.get("owner_pid"))
        .or_else(|| run.metadata_json.get("process_id"))
        .and_then(|pid| pid.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::fs;

    fn run_record() -> NewRunRecord {
        NewRunRecord::builder("test")
            .component_id("homeboy")
            .command("homeboy test homeboy")
            .cwd_path(std::path::Path::new("/tmp/homeboy"))
            .current_homeboy_version()
            .git_sha(Some("abc123".to_string()))
            .rig_id("studio")
            .metadata(serde_json::json!({
                "status": "running",
                "phase": "initial",
            }))
            .build()
    }

    fn active_observation() -> ActiveObservation {
        ActiveObservation::start(run_record()).expect("active observation")
    }

    fn running_run(metadata_json: serde_json::Value) -> RunRecord {
        RunRecord {
            id: "run-1".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-05-01T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/tmp".to_string()),
            homeboy_version: Some("test".to_string()),
            git_sha: Some("abc123".to_string()),
            rig_id: Some("studio".to_string()),
            metadata_json,
        }
    }

    #[test]
    fn test_running_status_note() {
        let base = running_run(serde_json::json!({}));
        let unverifiable = running_status_note(&base);
        assert!(unverifiable
            .as_deref()
            .expect("status note")
            .contains("no owner metadata"));

        let dead_owner = running_run(serde_json::json!({
            "homeboy_run_owner": { "pid": u32::MAX }
        }));
        let dead_owner = running_status_note(&dead_owner);
        assert!(dead_owner
            .as_deref()
            .expect("status note")
            .contains("owner process is not running"));

        let active_remote = running_run(serde_json::json!({
            "homeboy_run_owner": { "pid": u32::MAX },
            "lab": { "remote_job_status": "running" }
        }));
        assert!(running_status_note(&active_remote).is_none());

        let transferring = running_run(serde_json::json!({
            "homeboy_run_owner": { "pid": u32::MAX },
            "homeboy_ownership_handoff": { "state": "transferring" }
        }));
        assert!(running_status_note(&transferring)
            .as_deref()
            .expect("transfer status note")
            .contains("ownership transfer is pending"));
    }

    #[test]
    fn test_start_with_stable_id() {
        with_isolated_home(|_| {
            let observation =
                ActiveObservation::start_with_stable_id(run_record(), "stable-run").expect("start");
            assert_eq!(observation.run_id(), "stable-run");
            observation.finish(RunStatus::Pass, None);

            let rerun = ActiveObservation::start_with_stable_id(run_record(), "stable-run")
                .expect("re-run the same stable id");
            assert_eq!(rerun.run().status, "running");
        });
    }

    #[test]
    fn finish_variants_persist_status_and_merge_initial_metadata() {
        type Finish = fn(&ActiveObservation, serde_json::Value);
        let cases: [(&str, Finish, &str, bool); 5] = [
            (
                "finish",
                |o, m| o.finish(RunStatus::Pass, Some(m)),
                "pass",
                false,
            ),
            (
                "finish_error",
                |o, m| o.finish_error(Some(m)),
                "error",
                false,
            ),
            (
                "finish_with_merged_metadata",
                |o, m| o.finish_with_merged_metadata(RunStatus::Pass, m),
                "pass",
                true,
            ),
            (
                "finish_error_with_merged_metadata",
                |o, m| o.finish_error_with_merged_metadata(m),
                "error",
                true,
            ),
            (
                "try_finish_with_merged_metadata",
                |o, m| {
                    let run = o
                        .try_finish_with_merged_metadata(RunStatus::Fail, m)
                        .expect("finish");
                    assert_eq!(run.status, "fail");
                },
                "fail",
                true,
            ),
        ];

        for (name, finish, expected_status, merges) in cases {
            with_isolated_home(|_| {
                let observation = active_observation();
                let run_id = observation.run_id().to_string();
                finish(
                    &observation,
                    serde_json::json!({ "status": expected_status, "exit_code": 7 }),
                );

                let run = ObservationStore::open_initialized()
                    .expect("store")
                    .get_run(&run_id)
                    .expect("read run")
                    .expect("run exists");
                assert_eq!(run.status, expected_status, "{name}");
                assert_eq!(run.metadata_json["status"], expected_status, "{name}");
                assert_eq!(run.metadata_json["exit_code"], 7, "{name}");
                let initial_phase = if merges {
                    serde_json::json!("initial")
                } else {
                    serde_json::Value::Null
                };
                assert_eq!(run.metadata_json["phase"], initial_phase, "{name}");
            });
        }
    }

    #[test]
    fn terminal_reconciliation_preserves_bound_notification_route() {
        with_isolated_home(|_| {
            let route =
                crate::notification_route::NotificationRoute::new("extension", "opaque-route")
                    .expect("route");
            let observation =
                crate::notification_route::with_current(Some(route), || active_observation());
            let run_id = observation.run_id().to_string();

            observation
                .finish_with_merged_metadata(RunStatus::Pass, serde_json::json!({"exit_code": 0}));

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");
            assert_eq!(
                run.metadata_json["notification_route"]["route"],
                "opaque-route"
            );
        });
    }

    #[test]
    fn test_record_artifact_if_file() {
        with_isolated_home(|home| {
            let observation = active_observation();
            let artifact = home.path().join("artifact.json");
            fs::write(&artifact, b"{}").expect("write artifact");

            observation.record_artifact_if_file("artifact", &artifact);
            observation.record_artifact_if_file("missing", &home.path().join("missing.json"));
            observation.record_artifact_if_file("directory", home.path());

            let artifacts = observation
                .store()
                .list_artifacts(observation.run_id())
                .expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "artifact");
        });
    }
}
