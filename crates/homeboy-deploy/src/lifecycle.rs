//! Durable, versioned lifecycle records for resumable multi-target deploys.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use homeboy_core::error::{Error, Result};
use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_core::phase_timing::PhaseTimingReport;

const SCHEMA_VERSION: u32 = 1;

/// A durable activity projection for the single-project deploy lifecycle.
///
/// The observation store is the generic activity surface. The file-backed
/// multi-project lifecycle below remains the resumable aggregate checkpoint.
pub(crate) struct DeployObservation {
    store: ObservationStore,
    run_id: String,
    metadata: serde_json::Value,
    finished: bool,
}

impl DeployObservation {
    pub(crate) fn start(project_id: &str, source: &str) -> Result<Self> {
        Self::start_with_id(None, project_id, source)
    }

    pub(crate) fn start_with_id(
        requested_id: Option<&str>,
        project_id: &str,
        source: &str,
    ) -> Result<Self> {
        Self::start_with_control_plane(requested_id, project_id, source, None, None)
    }

    pub(crate) fn start_with_control_plane(
        requested_id: Option<&str>,
        project_id: &str,
        source: &str,
        lineage: Option<&crate::types::DeployControlPlaneLineage>,
        artifact_sha256: Option<&str>,
    ) -> Result<Self> {
        Self::start_with_control_plane_in_store(
            ObservationStore::open_initialized()?,
            requested_id,
            project_id,
            source,
            lineage,
            artifact_sha256,
        )
    }

    pub(crate) fn start_with_control_plane_in_roots(
        roots: &homeboy_core::paths::PathRoots,
        requested_id: Option<&str>,
        project_id: &str,
        source: &str,
        lineage: Option<&crate::types::DeployControlPlaneLineage>,
        artifact_sha256: Option<&str>,
    ) -> Result<Self> {
        Self::start_with_control_plane_in_store(
            ObservationStore::open_initialized_in_roots(roots)?,
            requested_id,
            project_id,
            source,
            lineage,
            artifact_sha256,
        )
    }

    pub(crate) fn resume_with_control_plane_in_roots(
        roots: &homeboy_core::paths::PathRoots,
        run_id: &str,
    ) -> Result<Self> {
        let store = ObservationStore::open_initialized_in_roots(roots)?;
        let current = store.get_run(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "deploy observation run not found",
                Some(run_id.to_string()),
                None,
            )
        })?;
        if current.kind != "deploy" {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "deploy observation run has a different kind",
                Some(run_id.to_string()),
                None,
            ));
        }
        let mut metadata = current.metadata_json;
        metadata["phase"] = json!("resuming");
        metadata["control_plane"]["phase"] = json!("resuming");
        store.resume_run(run_id, "deploy", metadata.clone())?;
        Ok(Self {
            store,
            run_id: run_id.to_string(),
            metadata,
            finished: false,
        })
    }

    fn start_with_control_plane_in_store(
        store: ObservationStore,
        requested_id: Option<&str>,
        project_id: &str,
        source: &str,
        lineage: Option<&crate::types::DeployControlPlaneLineage>,
        artifact_sha256: Option<&str>,
    ) -> Result<Self> {
        let metadata = json!({
            "schema": "homeboy/deploy-lifecycle/v1",
            "source": source,
            "phase": "admitted",
            "phase_history": [{ "phase": "admitted", "at": chrono::Utc::now().to_rfc3339() }],
            "remote_mutation_started": false,
            "targets": {},
            "homeboy_run_owner": { "pid": std::process::id() },
            "recovery": {
                "reconcile_command": "homeboy runs reconcile",
                "pre_upload_guarantee": "remote_mutation_started=false means no remote mutation began",
            },
            "control_plane": {
                "kind": "deploy",
                "phase": "admitted",
                "parent_run": lineage.map(|lineage| lineage.release_run_id.as_str()),
                "recovery_component_id": lineage.and_then(|lineage| lineage.recovery_component_id.as_deref()),
                "actions": [],
                "tasks": [],
                "artifacts": artifact_sha256.map(|sha256| vec![json!({
                    "id": format!("sha256-{sha256}"),
                    "kind": "release-package",
                    "uri": format!("sha256:{sha256}"),
                })]).unwrap_or_default(),
            },
        });
        let builder = NewRunRecord::builder("deploy")
            .component_id(project_id)
            .command(format!("homeboy deploy {project_id}"))
            .current_homeboy_version()
            .metadata(metadata.clone());
        let run = match (requested_id, lineage) {
            (Some(id), Some(lineage)) => store.start_run_with_id_in_mission(
                builder.build(),
                id.to_string(),
                &lineage.mission_id,
            )?,
            (None, Some(lineage)) => {
                store.start_run_in_mission(builder.build(), &lineage.mission_id)?
            }
            (Some(id), None) => store.start_run_with_id(builder.build(), id.to_string())?,
            (None, None) => store.start_run(builder.build())?,
        };
        Ok(Self {
            store,
            run_id: run.id,
            metadata,
            finished: false,
        })
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn phase(&mut self, phase: &str, remote_mutation_started: bool) -> Result<()> {
        let at = chrono::Utc::now().to_rfc3339();
        let object = self
            .metadata
            .as_object_mut()
            .expect("deploy metadata object");
        if object.get("phase").and_then(serde_json::Value::as_str) == Some(phase) {
            if remote_mutation_started {
                object.insert("remote_mutation_started".to_string(), json!(true));
            }
            self.store
                .update_run_metadata(&self.run_id, self.metadata.clone())?;
            return Ok(());
        }
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

    pub(crate) fn finish(&mut self, status: RunStatus, error: Option<String>) {
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
        self.metadata["control_plane"]["phase"] = json!(if status == RunStatus::Pass {
            "completed"
        } else {
            "failed"
        });
        self.metadata["control_plane"]["actions"] = if status != RunStatus::Pass
            && self.metadata["control_plane"]["recovery_component_id"].is_string()
        {
            json!([{
                "action": "resume",
                "availability": "available",
                "reason": "release deployment has a durable recovery checkpoint",
                "confirmation": "none",
                "required_inputs": [],
                "idempotent": true,
                "requires_revalidation": true,
                "result_resource_type": "run"
            }])
        } else {
            json!([])
        };
        if let Some(error) = error {
            self.metadata["error"] = json!(error);
        }
        let _ = self
            .store
            .finish_running_run(&self.run_id, status, Some(self.metadata.clone()));
        self.finished = true;
    }

    pub(crate) fn link_target(&mut self, project_id: &str, run_id: &str) -> Result<()> {
        let targets = self.metadata["targets"]
            .as_object_mut()
            .expect("deploy target metadata object");
        targets.insert(project_id.to_string(), json!(run_id));
        self.store
            .update_run_metadata(&self.run_id, self.metadata.clone())
            .map(|_| ())
    }

    pub(crate) fn project_target_tasks(
        &mut self,
        projects: &[crate::types::ProjectDeployResult],
    ) -> Result<()> {
        self.metadata["control_plane"]["tasks"] = json!(projects
            .iter()
            .map(|project| json!({
                "id": project.project_id,
                "state": match project.status.as_str() {
                    "deployed" => "succeeded",
                    "planned" | "skipped" => "skipped",
                    "applied_unverified" => "partial_failure",
                    _ => "failed",
                },
            }))
            .collect::<Vec<_>>());
        self.store
            .update_run_metadata(&self.run_id, self.metadata.clone())
            .map(|_| ())
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
pub(crate) struct DeployRunIdentity {
    pub(crate) source: String,
    pub(crate) artifact: String,
    pub(crate) components: Vec<String>,
    pub(crate) targets: Vec<String>,
    pub(crate) policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeployTargetStatus {
    Planned,
    Running,
    /// A process died after dispatching a target but before durable completion.
    /// Provider evidence must reconcile it; resume never reissues the command.
    Unknown,
    Succeeded,
    AppliedUnverified,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeployTargetLifecycle {
    pub(crate) target: String,
    pub(crate) status: DeployTargetStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) phase_timings: Option<PhaseTimingReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeployLifecycleRun {
    pub(crate) schema_version: u32,
    pub(crate) id: String,
    pub(crate) identity: DeployRunIdentity,
    pub(crate) targets: Vec<DeployTargetLifecycle>,
}

impl DeployLifecycleRun {
    pub(crate) fn new(id: String, identity: DeployRunIdentity) -> Self {
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

    pub(crate) fn resume(&mut self, identity: &DeployRunIdentity) -> Result<()> {
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
                // The external command may have completed before the process
                // died. Preserve that ambiguity for explicit reconciliation.
                target.status = DeployTargetStatus::Unknown;
                target.error = Some(
                    "Recovery required: target lease expired after dispatch; reconcile authoritative provider evidence before retrying"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn target_skips_mutation_retry(&self, target: &str) -> bool {
        self.targets.iter().any(|entry| {
            entry.target == target
                && matches!(
                    entry.status,
                    DeployTargetStatus::Succeeded
                        | DeployTargetStatus::AppliedUnverified
                        | DeployTargetStatus::Unknown
                )
        })
    }

    pub(crate) fn target_status(&self, target: &str) -> Option<DeployTargetStatus> {
        self.targets
            .iter()
            .find(|entry| entry.target == target)
            .map(|entry| entry.status.clone())
    }

    pub(crate) fn update_target(
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

/// The deploy lifecycle checkpoint below an explicitly injected data root.
///
/// Rooted rather than wrapped: `run_multi` resolves once and every checkpoint
/// read and write in that run addresses the same home. The ambient form
/// resolved the data root independently on each of the seven calls a multi
/// target deploy makes, so a repoint mid-run could have resumed from one home's
/// checkpoint and then recorded target outcomes into another's — the resumed
/// run would silently redo targets it had already succeeded (#7505).
fn lifecycle_path_in_roots(data_root: &Path, id: &str) -> PathBuf {
    data_root.join("deploy-runs").join(format!("{id}.json"))
}

pub(super) fn load_in_roots(data_root: &Path, id: &str) -> Result<DeployLifecycleRun> {
    let path = lifecycle_path_in_roots(data_root, id);
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

pub(super) fn save_in_roots(data_root: &Path, run: &DeployLifecycleRun) -> Result<()> {
    let path = lifecycle_path_in_roots(data_root, &run.id);
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
        assert!(run.target_skips_mutation_retry("a"));
        assert!(!run.target_skips_mutation_retry("b"));
        assert_eq!(run.targets[1].error.as_deref(), Some("boom"));
    }

    #[test]
    fn resume_marks_interrupted_targets_unknown_without_redeploying() {
        let mut run = DeployLifecycleRun::new("run".to_string(), identity());
        run.update_target("a", DeployTargetStatus::Succeeded, None, None);
        run.update_target("b", DeployTargetStatus::Running, None, None);
        run.resume(&identity()).expect("matching resume");
        assert!(run.target_skips_mutation_retry("a"));
        assert_eq!(run.targets[1].status, DeployTargetStatus::Unknown);
        assert!(run.target_skips_mutation_retry("b"));
    }

    #[test]
    fn applied_unverified_targets_are_terminal_without_mutation_retry() {
        let mut run = DeployLifecycleRun::new("run".to_string(), identity());
        run.update_target(
            "a",
            DeployTargetStatus::AppliedUnverified,
            Some("post-deploy verification unavailable".to_string()),
            None,
        );
        run.resume(&identity()).expect("matching resume");
        assert!(run.target_skips_mutation_retry("a"));
        assert_eq!(
            run.target_status("a"),
            Some(DeployTargetStatus::AppliedUnverified)
        );
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
        // Deliberately no `with_isolated_home`. Every durable call below already
        // takes a data root, so this test can name its own instead of mutating
        // `HOME` and reading it straight back out through `from_environment`.
        //
        // That is what #7505 is for. A test that owns its roots needs neither
        // the environment nor the process-global mutex `HomeGuard` takes to
        // protect it, so it can run beside any other test instead of queueing
        // behind all of them.
        {
            let data_root = tempfile::tempdir().expect("data root");
            let data_root = data_root.path();
            let mut run = DeployLifecycleRun::new("run".to_string(), identity());
            let mut timer = homeboy_core::phase_timing::PhaseTimer::new();
            timer.record_failed("transfer", std::time::Duration::from_millis(1));
            run.update_target(
                "b",
                DeployTargetStatus::Failed,
                Some("connection lost".to_string()),
                Some(timer.into_report()),
            );
            save_in_roots(data_root, &run).expect("persist run before retryable remote work");

            let restored = load_in_roots(data_root, "run").expect("read durable run");
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
        }
    }

    #[test]
    fn deploy_observation_persists_phase_progress_and_terminal_success() {
        with_isolated_home(|_| {
            let mut observation = DeployObservation::start("site", "HEAD").expect("admit run");
            let run_id = observation.run_id().to_string();
            observation
                .phase("build", false)
                .expect("persist build phase");
            observation
                .phase("package", false)
                .expect("persist package phase");
            observation
                .phase("transfer", true)
                .expect("persist transfer phase");
            observation
                .phase("extract", true)
                .expect("persist extract phase");
            observation
                .phase("verify", true)
                .expect("persist verify phase");
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
                Some(7)
            );
        });
    }

    #[test]
    fn release_triggered_deploy_projects_lineage_artifact_and_target_tasks() {
        with_isolated_home(|_| {
            let digest = "c".repeat(64);
            let lineage = crate::types::DeployControlPlaneLineage {
                mission_id: "release-mission-13697".to_string(),
                release_run_id: "release-run-13697".to_string(),
                recovery_component_id: Some("fixture".to_string()),
            };
            let mut observation = DeployObservation::start_with_control_plane(
                Some("deploy-run-13697"),
                "multi",
                "v1.2.3",
                Some(&lineage),
                Some(&digest),
            )
            .expect("admit release deployment");
            observation
                .project_target_tasks(&[
                    crate::types::ProjectDeployResult {
                        project_id: "target-a".to_string(),
                        status: "deployed".to_string(),
                        error: None,
                        results: Vec::new(),
                        summary: crate::types::DeploySummary {
                            total: 1,
                            succeeded: 1,
                            failed: 0,
                            skipped: 0,
                        },
                        phase_timings: None,
                        observation_run_id: None,
                    },
                    crate::types::ProjectDeployResult {
                        project_id: "target-b".to_string(),
                        status: "failed".to_string(),
                        error: Some("verification failed".to_string()),
                        results: Vec::new(),
                        summary: crate::types::DeploySummary {
                            total: 1,
                            succeeded: 0,
                            failed: 1,
                            skipped: 0,
                        },
                        phase_timings: None,
                        observation_run_id: None,
                    },
                ])
                .expect("project target tasks");
            observation.finish(RunStatus::Fail, Some("target failed".to_string()));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run("deploy-run-13697")
                .expect("read deploy")
                .expect("deploy run");
            assert_eq!(
                store.get_run_mission(&run.id).expect("mission").as_deref(),
                Some(lineage.mission_id.as_str())
            );
            assert_eq!(
                run.metadata_json["control_plane"]["parent_run"],
                lineage.release_run_id
            );
            assert_eq!(
                run.metadata_json["control_plane"]["artifacts"][0]["uri"],
                format!("sha256:{digest}")
            );
            assert_eq!(
                run.metadata_json["control_plane"]["tasks"][0]["state"],
                "succeeded"
            );
            assert_eq!(
                run.metadata_json["control_plane"]["tasks"][1]["state"],
                "failed"
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
                    .phase("package", false)
                    .expect("persist pre-upload package phase");
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

    #[test]
    fn admission_binds_process_recovery_and_target_links() {
        with_isolated_home(|_| {
            let mut observation =
                DeployObservation::start_with_id(Some("aggregate-run"), "site", "HEAD")
                    .expect("admit aggregate run");
            observation
                .link_target("target-a", "target-run")
                .expect("link target run");
            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(observation.run_id())
                .expect("read run")
                .expect("run");

            assert_eq!(
                run.metadata_json["homeboy_run_owner"]["pid"],
                std::process::id()
            );
            assert_eq!(
                run.metadata_json["recovery"]["reconcile_command"],
                "homeboy runs reconcile"
            );
            assert_eq!(run.metadata_json["targets"]["target-a"], "target-run");
            let activity = homeboy_core::activity::show_activity("aggregate-run")
                .expect("aggregate deploy run resolves through activity");
            assert_eq!(activity.items[0].id, "aggregate-run");
        });
    }

    #[test]
    fn active_deploy_observation_is_available_through_activity_show() {
        with_isolated_home(|_| {
            let observation = DeployObservation::start("site", "HEAD").expect("admit run");
            let report = homeboy_core::activity::show_activity(observation.run_id())
                .expect("activity show active deploy");

            assert_eq!(report.items.len(), 1);
            assert_eq!(report.items[0].id, observation.run_id());
            assert_eq!(
                report.items[0].state,
                homeboy_core::activity::ActivityState::Running
            );
        });
    }
}
