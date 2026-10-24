//! Durable, versioned lifecycle records for resumable multi-target deploys.

use serde::{Deserialize, Serialize};
use serde_json::json;

use homeboy_core::error::{Error, Result};
use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_core::phase_timing::PhaseTimingReport;

const SCHEMA_VERSION: u32 = 1;

/// Metadata key on the aggregate deploy `runs` row that carries the resumable
/// multi-target checkpoint. The deploy id is the run id, so the checkpoint has
/// no storage of its own.
const CHECKPOINT_KEY: &str = "checkpoint";

/// A durable activity projection for the deploy lifecycle.
///
/// The observation `runs` row is the only durable home for deploy state: the
/// activity metadata and, for multi-target deploys, the resumable checkpoint
/// under [`CHECKPOINT_KEY`].
pub(crate) struct DeployObservation {
    store: ObservationStore,
    run_id: String,
    metadata: serde_json::Value,
    finished: bool,
    phase_started: std::time::Instant,
    phase_started_at: chrono::DateTime<chrono::Utc>,
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

    /// Admit a multi-target deploy whose run row carries `checkpoint`. The
    /// checkpoint id becomes the run id.
    pub(crate) fn start_checkpointed_in_roots(
        roots: &homeboy_core::paths::PathRoots,
        checkpoint: &DeployLifecycleRun,
        project_id: &str,
        lineage: Option<&crate::types::DeployControlPlaneLineage>,
        artifact_sha256: Option<&str>,
    ) -> Result<Self> {
        Self::start_with_control_plane_in_store_with_checkpoint(
            ObservationStore::open_initialized_in_roots(roots)?,
            Some(&checkpoint.id),
            project_id,
            &checkpoint.identity.source,
            lineage,
            artifact_sha256,
            Some(checkpoint_value(checkpoint)?),
        )
    }

    /// Reopen a terminal multi-target deploy run and its checkpoint.
    ///
    /// The reopen is the run API's guarded `resume_run` (only a non-running
    /// row can be reopened), so two concurrent resumers cannot both own the
    /// run: exactly one wins and the other fails closed.
    pub(crate) fn resume_checkpointed_in_roots(
        roots: &homeboy_core::paths::PathRoots,
        run_id: &str,
        identity: &DeployRunIdentity,
    ) -> Result<(Self, DeployLifecycleRun)> {
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
        let mut checkpoint: DeployLifecycleRun = current
            .metadata_json
            .get(CHECKPOINT_KEY)
            .cloned()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "resume",
                    "deploy run has no resumable checkpoint",
                    Some(run_id.to_string()),
                    None,
                )
            })
            .and_then(|value| {
                serde_json::from_value(value).map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some(format!("parse deploy checkpoint for run {run_id}")),
                    )
                })
            })?;
        checkpoint.id = run_id.to_string();
        checkpoint.resume(identity)?;
        let mut metadata = current.metadata_json;
        metadata["phase"] = json!("resuming");
        metadata["control_plane"]["phase"] = json!("resuming");
        metadata[CHECKPOINT_KEY] = checkpoint_value(&checkpoint)?;
        store.resume_run(run_id, "deploy", metadata.clone())?;
        Ok((
            Self {
                store,
                run_id: run_id.to_string(),
                metadata,
                finished: false,
                phase_started: std::time::Instant::now(),
                phase_started_at: chrono::Utc::now(),
            },
            checkpoint,
        ))
    }

    /// Persist the multi-target checkpoint on this run's row. Fenced on the
    /// run still being active, so a late writer can never rewrite a run some
    /// other owner already terminalized.
    pub(crate) fn save_checkpoint(&mut self, checkpoint: &DeployLifecycleRun) -> Result<()> {
        self.metadata[CHECKPOINT_KEY] = checkpoint_value(checkpoint)?;
        self.store
            .update_running_run_metadata(&self.run_id, self.metadata.clone())?
            .map(|_| ())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "run_id",
                    "deploy run is no longer active; checkpoint not recorded",
                    Some(self.run_id.clone()),
                    None,
                )
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
        Self::start_with_control_plane_in_store_with_checkpoint(
            store,
            requested_id,
            project_id,
            source,
            lineage,
            artifact_sha256,
            None,
        )
    }

    fn start_with_control_plane_in_store_with_checkpoint(
        store: ObservationStore,
        requested_id: Option<&str>,
        project_id: &str,
        source: &str,
        lineage: Option<&crate::types::DeployControlPlaneLineage>,
        artifact_sha256: Option<&str>,
        checkpoint: Option<serde_json::Value>,
    ) -> Result<Self> {
        let mut metadata = json!({
            "schema": "homeboy/deploy-lifecycle/v1",
            "source": source,
            "phase": "admitted",
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
        if let Some(checkpoint) = checkpoint {
            metadata[CHECKPOINT_KEY] = checkpoint;
        }
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
            phase_started: std::time::Instant::now(),
            phase_started_at: chrono::Utc::now(),
        })
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn phase(&mut self, phase: &str, remote_mutation_started: bool) -> Result<()> {
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
        // Telemetry must never prevent the mutation safety checkpoint.
        if let Err(error) = self.record_current_phase(phase == "failed") {
            homeboy_core::log_status!(
                "observe",
                "deploy phase timing persistence failed: {}",
                error
            );
        }
        let object = self
            .metadata
            .as_object_mut()
            .expect("deploy metadata object");
        object.insert("phase".to_string(), json!(phase));
        if remote_mutation_started {
            object.insert("remote_mutation_started".to_string(), json!(true));
        }
        self.store
            .update_run_metadata(&self.run_id, self.metadata.clone())?;
        Ok(())
    }

    fn record_current_phase(&mut self, failed: bool) -> Result<()> {
        let phase = self.metadata["phase"].as_str().expect("deploy phase");
        let mut timer = homeboy_core::phase_timing::PhaseTimer::with_version(
            homeboy_core::build_identity::current().version,
        );
        if failed {
            timer.record_failed(
                format!("stage.deploy.{phase}"),
                self.phase_started.elapsed(),
            );
        } else {
            timer.record_ok(
                format!("stage.deploy.{phase}"),
                self.phase_started.elapsed(),
            );
        }
        let mut report = timer.into_report();
        report.spans[0].started_at = self
            .phase_started_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.store.record_phase_spans(
            &self.run_id,
            &format!("deploy:{phase}:{}", self.phase_started_at.to_rfc3339()),
            &report,
        )?;
        self.phase_started = std::time::Instant::now();
        self.phase_started_at = chrono::Utc::now();
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

fn checkpoint_value(checkpoint: &DeployLifecycleRun) -> Result<serde_json::Value> {
    serde_json::to_value(checkpoint).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize deploy checkpoint".to_string()),
        )
    })
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

    fn checkpoint_on_row(
        roots: &homeboy_core::paths::PathRoots,
        run_id: &str,
    ) -> (homeboy_core::observation::RunRecord, DeployLifecycleRun) {
        let run = ObservationStore::open_initialized_in_roots(roots)
            .expect("store")
            .get_run(run_id)
            .expect("read run")
            .expect("run row");
        let checkpoint = serde_json::from_value(run.metadata_json[CHECKPOINT_KEY].clone())
            .expect("checkpoint on run metadata");
        (run, checkpoint)
    }

    #[test]
    fn checkpoint_round_trips_through_run_row_across_reopen() {
        // Explicit roots, no `with_isolated_home`: the checkpoint addresses the
        // store under the roots it was given (#7505).
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let roots = context.path_roots();
        let mut run = DeployLifecycleRun::new("deploy-checkpoint-run".to_string(), identity());
        {
            let mut observation =
                DeployObservation::start_checkpointed_in_roots(&roots, &run, "multi", None, None)
                    .expect("admit checkpointed run");
            assert_eq!(observation.run_id(), "deploy-checkpoint-run");
            let mut timer = homeboy_core::phase_timing::PhaseTimer::new();
            timer.record_failed("transfer", std::time::Duration::from_millis(1));
            run.update_target("a", DeployTargetStatus::Running, None, None);
            run.update_target(
                "b",
                DeployTargetStatus::Failed,
                Some("connection lost".to_string()),
                Some(timer.into_report()),
            );
            observation
                .save_checkpoint(&run)
                .expect("persist checkpoint before retryable remote work");
            // Process ends mid-target: drop terminalizes the run.
        }

        let (row, stored) = checkpoint_on_row(&roots, "deploy-checkpoint-run");
        assert_eq!(row.kind, "deploy");
        assert_eq!(row.status, RunStatus::Error.as_str());
        assert_eq!(stored.schema_version, SCHEMA_VERSION);
        assert_eq!(stored.targets[0].status, DeployTargetStatus::Running);

        let (observation, restored) = DeployObservation::resume_checkpointed_in_roots(
            &roots,
            "deploy-checkpoint-run",
            &identity(),
        )
        .expect("reopen checkpointed run");
        assert_eq!(observation.run_id(), "deploy-checkpoint-run");
        assert_eq!(restored.id, "deploy-checkpoint-run");
        assert_eq!(restored.targets[0].status, DeployTargetStatus::Unknown);
        assert!(restored.target_skips_mutation_retry("a"));
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
        let (row, stored) = checkpoint_on_row(&roots, "deploy-checkpoint-run");
        assert_eq!(row.status, RunStatus::Running.as_str());
        assert_eq!(stored.targets[0].status, DeployTargetStatus::Unknown);
    }

    #[test]
    fn concurrent_resumers_of_one_checkpoint_have_exactly_one_winner() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let roots = context.path_roots();
        let run = DeployLifecycleRun::new("deploy-contended".to_string(), identity());
        drop(
            DeployObservation::start_checkpointed_in_roots(&roots, &run, "multi", None, None)
                .expect("admit"),
        );

        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let claimants = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        DeployObservation::resume_checkpointed_in_roots(
                            &roots,
                            "deploy-contended",
                            &identity(),
                        )
                        .map(|(mut observation, _)| {
                            // Keep the winner's run active for the loser to observe.
                            observation.finished = true;
                        })
                    })
                })
                .collect::<Vec<_>>();
            claimants
                .into_iter()
                .map(|claimant| claimant.join().expect("claimant thread"))
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        let (row, _) = checkpoint_on_row(&roots, "deploy-contended");
        assert_eq!(row.status, RunStatus::Running.as_str());
    }

    #[test]
    fn resume_refuses_identity_mismatch_without_reopening_the_run() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let roots = context.path_roots();
        let run = DeployLifecycleRun::new("deploy-mismatch".to_string(), identity());
        drop(
            DeployObservation::start_checkpointed_in_roots(&roots, &run, "multi", None, None)
                .expect("admit"),
        );
        let mut changed = identity();
        changed.policy = "tagged=false".to_string();
        let error =
            DeployObservation::resume_checkpointed_in_roots(&roots, "deploy-mismatch", &changed)
                .err()
                .expect("mismatched identity fails closed");
        assert!(error.message.contains("does not exactly match"));
        let (row, _) = checkpoint_on_row(&roots, "deploy-mismatch");
        assert_eq!(row.status, RunStatus::Error.as_str());
    }

    #[test]
    fn checkpoint_writes_are_fenced_once_the_run_is_terminal() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let roots = context.path_roots();
        let mut run = DeployLifecycleRun::new("deploy-fenced".to_string(), identity());
        let mut observation =
            DeployObservation::start_checkpointed_in_roots(&roots, &run, "multi", None, None)
                .expect("admit");
        observation.finish(RunStatus::Pass, None);
        run.update_target("a", DeployTargetStatus::Running, None, None);
        assert!(observation.save_checkpoint(&run).is_err());
        let (row, stored) = checkpoint_on_row(&roots, "deploy-fenced");
        assert_eq!(row.status, RunStatus::Pass.as_str());
        assert_eq!(stored.targets[0].status, DeployTargetStatus::Planned);
    }

    #[test]
    fn resume_without_a_checkpoint_fails_closed() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let roots = context.path_roots();
        let store = ObservationStore::open_initialized_in_roots(&roots).expect("store");
        drop(
            DeployObservation::start_with_control_plane_in_store(
                store,
                Some("deploy-single"),
                "site",
                "HEAD",
                None,
                None,
            )
            .expect("admit single-project deploy"),
        );
        let error =
            DeployObservation::resume_checkpointed_in_roots(&roots, "deploy-single", &identity())
                .err()
                .expect("no checkpoint");
        assert!(error.message.contains("no resumable checkpoint"));
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
            let report = ObservationStore::open_initialized()
                .expect("store")
                .phase_spans(&run_id)
                .expect("phase spans");
            assert!(report.span("stage.deploy.transfer").is_some());
            assert!(report.span("stage.deploy.verify").is_some());
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
