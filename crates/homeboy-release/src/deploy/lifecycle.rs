//! Durable, versioned lifecycle records for resumable multi-target deploys.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use homeboy_core::error::{Error, Result};
use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_core::paths;
use homeboy_core::phase_timing::PhaseTimingReport;

const SCHEMA_VERSION: u32 = 1;

/// A durable activity projection for the single-project deploy lifecycle.
///
/// The observation store is the generic activity surface. The file-backed
/// multi-project lifecycle below remains the resumable aggregate checkpoint.
pub(super) struct DeployObservation {
    store: ObservationStore,
    run_id: String,
    metadata: serde_json::Value,
    finished: bool,
}

impl DeployObservation {
    pub(super) fn start(project_id: &str, source: &str) -> Result<Self> {
        let store = ObservationStore::open_initialized()?;
        let metadata = json!({
            "schema": "homeboy/deploy-lifecycle/v1",
            "source": source,
            "phase": "admitted",
            "phase_history": [{ "phase": "admitted", "at": chrono::Utc::now().to_rfc3339() }],
            "remote_mutation_started": false,
        });
        let run = store.start_run(
            NewRunRecord::builder("deploy")
                .component_id(project_id)
                .command(format!("homeboy deploy {project_id}"))
                .current_homeboy_version()
                .metadata(metadata.clone())
                .build(),
        )?;
        Ok(Self {
            store,
            run_id: run.id,
            metadata,
            finished: false,
        })
    }

    pub(super) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(super) fn phase(&mut self, phase: &str, remote_mutation_started: bool) -> Result<()> {
        let at = chrono::Utc::now().to_rfc3339();
        let object = self
            .metadata
            .as_object_mut()
            .expect("deploy metadata object");
        object.insert("phase".to_string(), json!(phase));
        if remote_mutation_started {
            object.insert("remote_mutation_started".to_string(), json!(true));
        }
        object
            .get_mut("phase_history")
            .and_then(serde_json::Value::as_array_mut)
            .expect("deploy phase history")
            .push(json!({ "phase": phase, "at": at }));
        self.store
            .update_run_metadata(&self.run_id, self.metadata.clone())?;
        Ok(())
    }

    pub(super) fn finish(&mut self, status: RunStatus, error: Option<String>) {
        if self.finished {
            return;
        }
        let _ = self.phase(
            if status == RunStatus::Pass {
                "completed"
            } else {
                "failed"
            },
            false,
        );
        if let Some(error) = error {
            self.metadata["error"] = json!(error);
        }
        let _ = self
            .store
            .finish_running_run(&self.run_id, status, Some(self.metadata.clone()));
        self.finished = true;
    }
}

impl Drop for DeployObservation {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(
                RunStatus::Error,
                Some("deploy process ended before a terminal result; inspect this run before retrying".to_string()),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRunIdentity {
    pub source: String,
    pub artifact: String,
    pub components: Vec<String>,
    pub targets: Vec<String>,
    pub policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployTargetStatus {
    Planned,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployTargetLifecycle {
    pub target: String,
    pub status: DeployTargetStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timings: Option<PhaseTimingReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployLifecycleRun {
    pub schema_version: u32,
    pub id: String,
    pub identity: DeployRunIdentity,
    pub targets: Vec<DeployTargetLifecycle>,
}

impl DeployLifecycleRun {
    pub fn new(id: String, identity: DeployRunIdentity) -> Self {
        let targets = identity
            .targets
            .iter()
            .cloned()
            .map(|target| DeployTargetLifecycle {
                target,
                status: DeployTargetStatus::Planned,
                error: None,
                phase_timings: None,
            })
            .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            identity,
            targets,
        }
    }

    pub fn resume(&mut self, identity: &DeployRunIdentity) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::validation_invalid_argument(
                "resume",
                format!(
                    "Deploy run '{}' uses unsupported schema version {}",
                    self.id, self.schema_version
                ),
                None,
                None,
            ));
        }
        if &self.identity != identity {
            return Err(Error::validation_invalid_argument(
                "resume",
                format!("Deploy run '{}' identity does not exactly match the requested source, artifact, components, targets, and policy", self.id),
                None,
                None,
            ));
        }
        for target in &mut self.targets {
            if target.status == DeployTargetStatus::Running {
                // A process death leaves no reliable remote completion signal. Retry it.
                target.status = DeployTargetStatus::Planned;
                target.error = Some(
                    "Recovered interrupted target; retrying from its durable checkpoint"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    pub fn target_is_succeeded(&self, target: &str) -> bool {
        self.targets
            .iter()
            .any(|entry| entry.target == target && entry.status == DeployTargetStatus::Succeeded)
    }

    pub fn update_target(
        &mut self,
        target: &str,
        status: DeployTargetStatus,
        error: Option<String>,
        phase_timings: Option<PhaseTimingReport>,
    ) {
        if let Some(entry) = self.targets.iter_mut().find(|entry| entry.target == target) {
            entry.status = status;
            entry.error = error;
            entry.phase_timings = phase_timings;
        }
    }
}

pub(super) fn lifecycle_path(id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("deploy-runs")
        .join(format!("{id}.json")))
}

pub(super) fn load(id: &str) -> Result<DeployLifecycleRun> {
    let path = lifecycle_path(id)?;
    let contents = fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read deploy run {}", path.display())),
        )
    })?;
    serde_json::from_str(&contents).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("parse deploy run {}", path.display())),
        )
    })
}

pub(super) fn save(run: &DeployLifecycleRun) -> Result<()> {
    let path = lifecycle_path(&run.id)?;
    let parent = path.parent().expect("deploy lifecycle path has parent");
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::to_vec_pretty(run).map_err(|error| {
        Error::internal_io(error.to_string(), Some("serialize deploy run".to_string()))
    })?;
    fs::write(&temporary, contents).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("commit {}", path.display())),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;

    fn identity() -> DeployRunIdentity {
        DeployRunIdentity {
            source: "refs/tags/v1.2.3@abc".to_string(),
            artifact: "sha256:123".to_string(),
            components: vec!["plugin".to_string()],
            targets: vec!["a".to_string(), "b".to_string()],
            policy: "tagged=true".to_string(),
        }
    }

    #[test]
    fn success_failure_and_skip_are_durable() {
        let mut run = DeployLifecycleRun::new("run".to_string(), identity());
        run.update_target("a", DeployTargetStatus::Succeeded, None, None);
        run.update_target(
            "b",
            DeployTargetStatus::Failed,
            Some("boom".to_string()),
            None,
        );
        assert!(run.target_is_succeeded("a"));
        assert!(!run.target_is_succeeded("b"));
        assert_eq!(run.targets[1].error.as_deref(), Some("boom"));
    }

    #[test]
    fn resume_recovers_interrupted_targets_without_redeploying_successes() {
        let mut run = DeployLifecycleRun::new("run".to_string(), identity());
        run.update_target("a", DeployTargetStatus::Succeeded, None, None);
        run.update_target("b", DeployTargetStatus::Running, None, None);
        run.resume(&identity()).expect("matching resume");
        assert!(run.target_is_succeeded("a"));
        assert_eq!(run.targets[1].status, DeployTargetStatus::Planned);
    }

    #[test]
    fn resume_refuses_any_identity_mismatch() {
        let run = DeployLifecycleRun::new("run".to_string(), identity());
        let mut changed = identity();
        changed.artifact = "sha256:changed".to_string();
        let error = run.clone().resume(&changed).expect_err("must fail closed");
        assert!(error.message.contains("does not exactly match"));
    }

    #[test]
    fn durable_record_round_trips_target_state_and_partial_timing_evidence() {
        with_isolated_home(|_| {
            let mut run = DeployLifecycleRun::new("run".to_string(), identity());
            let mut timer = homeboy_core::phase_timing::PhaseTimer::new();
            timer.record_failed("transfer", std::time::Duration::from_millis(1));
            run.update_target(
                "b",
                DeployTargetStatus::Failed,
                Some("connection lost".to_string()),
                Some(timer.into_report()),
            );
            save(&run).expect("persist run before retryable remote work");

            let restored = load("run").expect("read durable run");
            assert_eq!(restored.schema_version, SCHEMA_VERSION);
            assert_eq!(restored.targets[1].status, DeployTargetStatus::Failed);
            assert_eq!(
                restored.targets[1].error.as_deref(),
                Some("connection lost")
            );
            assert_eq!(
                restored.targets[1]
                    .phase_timings
                    .as_ref()
                    .and_then(|report| report.span("transfer"))
                    .map(|span| span.status),
                Some(homeboy_core::phase_timing::PhaseStatus::Failed)
            );
        });
    }

    #[test]
    fn deploy_observation_persists_phase_progress_and_terminal_success() {
        with_isolated_home(|_| {
            let mut observation = DeployObservation::start("site", "HEAD").expect("admit run");
            let run_id = observation.run_id().to_string();
            observation
                .phase("artifact_preparation", false)
                .expect("persist preparation phase");
            observation
                .phase("transfer", true)
                .expect("persist transfer phase");
            observation.finish(RunStatus::Pass, None);

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run");
            assert_eq!(run.status, RunStatus::Pass.as_str());
            assert_eq!(run.metadata_json["phase"], "completed");
            assert_eq!(run.metadata_json["remote_mutation_started"], true);
            assert_eq!(
                run.metadata_json["phase_history"].as_array().map(Vec::len),
                Some(4)
            );
        });
    }

    #[test]
    fn dropped_deploy_observation_terminalizes_before_remote_mutation() {
        with_isolated_home(|_| {
            let run_id = {
                let mut observation =
                    DeployObservation::start("site", "refs/heads/fix").expect("admit run");
                observation
                    .phase("artifact_preparation", false)
                    .expect("persist preparation phase");
                observation.run_id().to_string()
            };

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run");
            assert_eq!(run.status, RunStatus::Error.as_str());
            assert_eq!(run.metadata_json["remote_mutation_started"], false);
            assert!(run.metadata_json["error"]
                .as_str()
                .is_some_and(|error| error.contains("terminal result")));
        });
    }
}
