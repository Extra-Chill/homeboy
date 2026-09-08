use homeboy_control_plane_contract::{MissionId, RunId};
use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_core::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use super::types::{ReleaseControlPlaneContext, ReleaseRun, ReleaseStepStatus};

pub(super) struct ReleaseControlPlaneObservation {
    store: ObservationStore,
    context: ReleaseControlPlaneContext,
    metadata: Value,
    finished: bool,
}

impl ReleaseControlPlaneObservation {
    pub(super) fn start(
        roots: &homeboy_core::paths::PathRoots,
        component_id: &str,
    ) -> Result<Self> {
        let mission_id = MissionId::new(format!("release-mission-{}", Uuid::new_v4()))
            .map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let release_run_id = RunId::new(format!("release-run-{}", Uuid::new_v4()))
            .map_err(|error| Error::internal_unexpected(error.to_string()))?;
        let context = ReleaseControlPlaneContext {
            mission_id: mission_id.to_string(),
            release_run_id: release_run_id.to_string(),
        };
        let metadata = json!({
            "schema": "homeboy/release-control-plane/v1",
            "control_plane": {
                "kind": "release",
                "phase": "admitted",
                "tasks": [],
                "artifacts": [],
            }
        });
        let store = ObservationStore::open_initialized_in_roots(roots)?;
        store.start_run_with_id_in_mission(
            NewRunRecord::builder("release")
                .component_id(component_id)
                .command(format!("homeboy release {component_id}"))
                .current_homeboy_version()
                .metadata(metadata.clone())
                .build(),
            context.release_run_id.clone(),
            &context.mission_id,
        )?;
        Ok(Self {
            store,
            context,
            metadata,
            finished: false,
        })
    }

    pub(super) fn context(&self) -> ReleaseControlPlaneContext {
        self.context.clone()
    }

    pub(super) fn finish(&mut self, run: &ReleaseRun) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.metadata["control_plane"]["phase"] = json!("completed");
        self.metadata["control_plane"]["tasks"] = json!(run
            .result
            .steps
            .iter()
            .map(|step| json!({
                "id": step.id,
                "state": release_step_state(&step.status),
            }))
            .collect::<Vec<_>>());
        self.metadata["control_plane"]["artifacts"] = json!(release_artifacts(run));
        let status = if matches!(run.result.status, ReleaseStepStatus::Success) {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        };
        self.store.finish_running_run(
            &self.context.release_run_id,
            status,
            Some(self.metadata.clone()),
        )?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for ReleaseControlPlaneObservation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.metadata["control_plane"]["phase"] = json!("interrupted");
        self.metadata["control_plane"]["blocker"] =
            json!("release process ended before a terminal result");
        let _ = self.store.finish_running_run(
            &self.context.release_run_id,
            RunStatus::Error,
            Some(self.metadata.clone()),
        );
    }
}

fn release_step_state(status: &ReleaseStepStatus) -> &'static str {
    match status {
        ReleaseStepStatus::Success => "succeeded",
        ReleaseStepStatus::PartialSuccess => "partial_failure",
        ReleaseStepStatus::Failed | ReleaseStepStatus::Missing => "failed",
        ReleaseStepStatus::Skipped => "skipped",
    }
}

fn release_artifacts(run: &ReleaseRun) -> Vec<Value> {
    run.result
        .steps
        .iter()
        .filter_map(|step| step.data.as_ref()?.get("artifacts")?.as_array())
        .flatten()
        .filter(|artifact| {
            artifact["publication_authority"].as_bool().unwrap_or(false)
                && artifact["phase"].as_str().unwrap_or("final") == "final"
        })
        .filter_map(|artifact| {
            let sha256 = artifact["sha256"].as_str()?.trim();
            let path = artifact["durable_path"]
                .as_str()
                .or_else(|| artifact["path"].as_str())?;
            (sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
                json!({
                    "id": format!("sha256-{sha256}"),
                    "kind": "release-package",
                    "uri": format!("sha256:{sha256}"),
                    "path": path,
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::types::{ReleaseRunResult, ReleaseStepResult};
    use homeboy_core::test_support::with_isolated_home;

    #[test]
    fn release_extension_projects_canonical_mission_run_and_artifact_digest() {
        with_isolated_home(|_| {
            let roots = homeboy_core::paths::PathRoots::from_environment().expect("roots");
            let mut observation =
                ReleaseControlPlaneObservation::start(&roots, "homeboy").expect("start release");
            let context = observation.context();
            let digest = "b".repeat(64);
            observation
                .finish(&ReleaseRun {
                    component_id: "homeboy".to_string(),
                    enabled: true,
                    result: ReleaseRunResult {
                        steps: vec![ReleaseStepResult {
                            id: "package".to_string(),
                            step_type: "package".to_string(),
                            status: ReleaseStepStatus::Success,
                            data: Some(json!({
                                "artifacts": [{
                                    "path": "homeboy.tar.xz",
                                    "durable_path": "/artifacts/homeboy.tar.xz",
                                    "phase": "final",
                                    "publication_authority": true,
                                    "sha256": digest,
                                }]
                            })),
                            ..Default::default()
                        }],
                        status: ReleaseStepStatus::Success,
                        warnings: Vec::new(),
                        summary: None,
                        phase_timings: None,
                        rollback: None,
                    },
                })
                .expect("finish release");

            let store = ObservationStore::open_readonly_in_roots(&roots).expect("read store");
            let run = store
                .get_run(&context.release_run_id)
                .expect("read release")
                .expect("release run");
            assert_eq!(run.status, "pass");
            assert_eq!(
                store
                    .get_run_mission(&run.id)
                    .expect("run mission")
                    .as_deref(),
                Some(context.mission_id.as_str())
            );
            assert_eq!(
                run.metadata_json["control_plane"]["artifacts"][0]["uri"],
                format!("sha256:{digest}")
            );
        });
    }
}
