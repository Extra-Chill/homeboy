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
    pub fn start(record: NewRunRecord) -> crate::core::Result<Self> {
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

    let Some(owner_pid) = run_owner_pid(run) else {
        return Some(
            "running status has no owner metadata; run may predate reconciliation support"
                .to_string(),
        );
    };

    if crate::core::process::pid_is_running(owner_pid) {
        None
    } else {
        Some(
            "owner process is not running; run may be stale; run `homeboy runs reconcile`"
                .to_string(),
        )
    }
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
    use crate::core::observation::FindingListFilter;
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

    #[test]
    fn merge_metadata_preserves_initial_and_overwrites_finish_keys() {
        let merged = merge_metadata(
            serde_json::json!({ "status": "running", "component": "homeboy" }),
            serde_json::json!({ "status": "pass", "exit_code": 0 }),
        );

        assert_eq!(merged["component"], "homeboy");
        assert_eq!(merged["status"], "pass");
        assert_eq!(merged["exit_code"], 0);
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
    }

    #[test]
    fn test_run_owner_pid() {
        let run = running_run(serde_json::json!({
            "homeboy_run_owner": { "pid": 1234 }
        }));

        assert_eq!(run_owner_pid(&run), Some(1234));
    }

    #[test]
    fn test_start() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert_eq!(observation.run().kind, "test");
            assert_eq!(observation.run().status, "running");
        });
    }

    #[test]
    fn test_start_best_effort() {
        with_isolated_home(|_| {
            assert!(ActiveObservation::start_best_effort(run_record()).is_some());
        });
    }

    #[test]
    fn test_finish_with_merged_metadata_preserves_initial_command_context() {
        with_isolated_home(|_| {
            let observation = active_observation();
            let run_id = observation.run_id().to_string();

            observation.finish_with_merged_metadata(
                RunStatus::Pass,
                serde_json::json!({
                    "status": "pass",
                    "exit_code": 0,
                }),
            );

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");

            assert_eq!(run.status, "pass");
            assert_eq!(run.metadata_json["phase"], "initial");
            assert_eq!(run.metadata_json["status"], "pass");
            assert_eq!(run.metadata_json["exit_code"], 0);
        });
    }

    #[test]
    fn test_finish_error_with_merged_metadata_sets_error_status() {
        with_isolated_home(|_| {
            let observation = active_observation();
            let run_id = observation.run_id().to_string();

            observation.finish_error_with_merged_metadata(serde_json::json!({
                "status": "error",
                "error": "simulated",
            }));

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");

            assert_eq!(run.status, "error");
            assert_eq!(run.metadata_json["phase"], "initial");
            assert_eq!(run.metadata_json["status"], "error");
            assert_eq!(run.metadata_json["error"], "simulated");
        });
    }

    #[test]
    fn test_store() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert!(observation.store().status().expect("status").exists);
        });
    }

    #[test]
    fn test_run() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert_eq!(observation.run().component_id.as_deref(), Some("homeboy"));
        });
    }

    #[test]
    fn test_run_id() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert!(!observation.run_id().is_empty());
        });
    }

    #[test]
    fn test_component_id() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert_eq!(observation.component_id(), Some("homeboy"));
        });
    }

    #[test]
    fn test_rig_id() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert_eq!(observation.rig_id(), Some("studio"));
        });
    }

    #[test]
    fn test_initial_metadata() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert_eq!(observation.initial_metadata()["status"], "running");
        });
    }

    #[test]
    fn test_finish() {
        with_isolated_home(|_| {
            let observation = active_observation();
            let run_id = observation.run_id().to_string();
            observation.finish(RunStatus::Pass, Some(serde_json::json!({ "exit_code": 0 })));

            let run = observation
                .store()
                .get_run(&run_id)
                .expect("read run")
                .expect("run");
            assert_eq!(run.status, "pass");
            assert_eq!(run.metadata_json["exit_code"], 0);
        });
    }

    #[test]
    fn test_finish_error() {
        with_isolated_home(|_| {
            let observation = active_observation();
            let run_id = observation.run_id().to_string();
            observation.finish_error(Some(serde_json::json!({ "error": "boom" })));

            let run = observation
                .store()
                .get_run(&run_id)
                .expect("read run")
                .expect("run");
            assert_eq!(run.status, "error");
            assert_eq!(run.metadata_json["error"], "boom");
        });
    }

    #[test]
    fn test_record_findings() {
        with_isolated_home(|_| {
            let observation = active_observation();
            observation.record_findings(&[NewFindingRecord {
                run_id: observation.run_id().to_string(),
                tool: "test".to_string(),
                rule: Some("rule".to_string()),
                file: Some("src/lib.rs".to_string()),
                line: Some(1),
                severity: Some("error".to_string()),
                fingerprint: Some("fingerprint".to_string()),
                message: "message".to_string(),
                fixable: Some(false),
                metadata_json: serde_json::json!({}),
            }]);

            let findings = observation
                .store()
                .list_findings(FindingListFilter {
                    run_id: Some(observation.run_id().to_string()),
                    ..FindingListFilter::default()
                })
                .expect("findings");
            assert_eq!(findings.len(), 1);
        });
    }

    #[test]
    fn test_record_artifact_if_file() {
        with_isolated_home(|home| {
            let observation = active_observation();
            let artifact = home.path().join("artifact.json");
            fs::write(&artifact, b"{}").expect("write artifact");

            observation.record_artifact_if_file("artifact", &artifact);

            let artifacts = observation
                .store()
                .list_artifacts(observation.run_id())
                .expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "artifact");
        });
    }

    #[test]
    fn test_store_path() {
        with_isolated_home(|_| {
            let observation = active_observation();

            assert!(observation.store_path().ends_with("homeboy.sqlite"));
        });
    }
}
