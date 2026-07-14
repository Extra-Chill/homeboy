//! Durable handoff and run-location evidence for detached Lab jobs.

use crate::core::run_outcome_envelope::RunOutcomeEnvelope;
use crate::core::runner_execution_envelope::{
    PathMaterializationPlan, RunnerExecutionArtifactRef, RunnerExecutionNextAction,
    RunnerExecutionRecord,
};

use super::RunnerWorkloadArtifactRef;

pub const RUNNER_HANDOFF_ENVELOPE_SCHEMA: &str = "homeboy/runner-exec-handoff/v1";
pub const RUN_LOCATION_INDEX_SCHEMA: &str = "homeboy/run-location-index/v1";
pub const RUNNER_ARTIFACT_MANIFEST_REF_NAME: &str = "runner-artifact-manifest-ref";
pub const RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA: &str = "homeboy/runner-artifact-manifest-ref/v1";
pub const RUNNER_ARTIFACT_MANIFEST_SCHEMA: &str = crate::core::artifacts::ARTIFACT_MANIFEST_SCHEMA;
pub const RUNNER_ARTIFACT_MANIFEST_FILE: &str = crate::core::artifacts::ARTIFACT_MANIFEST_FILE;
pub const RUNNER_ARTIFACT_ROOT_DIR_SUFFIX: &str = "-homeboy-artifacts";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerHandoffEnvelope {
    pub schema: String,
    pub status: String,
    pub execution_location: String,
    #[serde(default)]
    pub identity: AgentTaskDispatchIdentity,
    pub runner_id: String,
    pub job_id: String,
    pub durable_run_id: Option<String>,
    pub persisted_run_id: Option<String>,
    pub mirror_run_id: Option<String>,
    pub remote_cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_materialization_plan: Option<PathMaterializationPlan>,
    pub artifact_manifest: RunnerHandoffArtifactManifestRef,
    pub run_location_index: RunLocationIndex,
    pub evidence: RunnerHandoffEvidence,
    pub follow_commands: RunnerHandoffFollowCommands,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerHandoffArtifactManifestRef {
    pub schema: String,
    pub manifest_schema: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerHandoffEvidence {
    pub schema: String,
    pub status: String,
    pub runner_id: String,
    pub runner_job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    pub remote_cwd: String,
    pub artifact_manifest: RunnerHandoffArtifactManifestRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerWorkloadArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_commands: Vec<RunnerHandoffNextCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerHandoffNextCommand {
    pub label: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskDispatchIdentity {
    pub runner_id: String,
    pub runner_job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunnerHandoffFollowCommands {
    pub job_logs: String,
    pub job_cancel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunLocationIndex {
    pub schema: String,
    pub run_id: String,
    pub controller_location: String,
    pub runner_id: String,
    pub remote_job_id: String,
    pub remote_cwd: String,
    pub artifact_manifest_ref: RunnerHandoffArtifactManifestRef,
    pub liveness_heartbeat_timestamp: String,
    pub follow_commands: RunnerHandoffFollowCommands,
}

impl RunnerHandoffEnvelope {
    pub fn detached_lab_offload(
        runner_id: &str,
        job_id: &str,
        remote_cwd: String,
        path_materialization_plan: Option<PathMaterializationPlan>,
        mirror_run_id: Option<String>,
        liveness_heartbeat_timestamp: String,
    ) -> Self {
        let job_logs_command = vec![
            "homeboy".to_string(),
            "runner".to_string(),
            "job".to_string(),
            "logs".to_string(),
            runner_id.to_string(),
            job_id.to_string(),
            "--follow".to_string(),
        ];
        let job_cancel_command = vec![
            "homeboy".to_string(),
            "runner".to_string(),
            "job".to_string(),
            "cancel".to_string(),
            runner_id.to_string(),
            job_id.to_string(),
        ];
        let mut next_commands = vec![
            RunnerHandoffNextCommand {
                label: "runner_job_logs".to_string(),
                command: job_logs_command.clone(),
            },
            RunnerHandoffNextCommand {
                label: "runner_job_cancel".to_string(),
                command: job_cancel_command.clone(),
            },
        ];
        if let Some(run_id) = mirror_run_id.as_ref() {
            next_commands.extend([
                RunnerHandoffNextCommand {
                    label: "run_status".to_string(),
                    command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "status".to_string(),
                        run_id.clone(),
                    ],
                },
                RunnerHandoffNextCommand {
                    label: "run_logs".to_string(),
                    command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "logs".to_string(),
                        run_id.clone(),
                    ],
                },
                RunnerHandoffNextCommand {
                    label: "run_artifacts".to_string(),
                    command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "artifacts".to_string(),
                        run_id.clone(),
                    ],
                },
            ]);
        }
        let artifact_manifest = RunnerHandoffArtifactManifestRef::for_remote_cwd(&remote_cwd);
        let run_location_index_path = run_location_index_path(&remote_cwd);
        let follow_commands = RunnerHandoffFollowCommands {
            job_logs: format!("homeboy runner job logs {runner_id} {job_id} --follow"),
            job_cancel: format!("homeboy runner job cancel {runner_id} {job_id}"),
            status: mirror_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task status {run_id}")),
            logs: mirror_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task logs {run_id}")),
            artifacts: mirror_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task artifacts {run_id}")),
        };
        let run_location_index = RunLocationIndex {
            schema: RUN_LOCATION_INDEX_SCHEMA.to_string(),
            run_id: mirror_run_id
                .clone()
                .unwrap_or_else(|| format!("runner:{runner_id}:job:{job_id}")),
            controller_location: "controller:local".to_string(),
            runner_id: runner_id.to_string(),
            remote_job_id: job_id.to_string(),
            remote_cwd: remote_cwd.clone(),
            artifact_manifest_ref: artifact_manifest.clone(),
            liveness_heartbeat_timestamp,
            follow_commands: follow_commands.clone(),
        };
        let evidence = RunnerHandoffEvidence {
            schema: "homeboy/runner-handoff-evidence/v1".to_string(),
            status: "running".to_string(),
            runner_id: runner_id.to_string(),
            runner_job_id: job_id.to_string(),
            run_id: mirror_run_id.clone(),
            persisted_run_id: mirror_run_id.clone(),
            mirror_run_id: mirror_run_id.clone(),
            remote_cwd: remote_cwd.clone(),
            artifact_manifest: artifact_manifest.clone(),
            artifact_refs: vec![
                RunnerWorkloadArtifactRef {
                    id: "artifact_manifest".to_string(),
                    name: Some("runner artifact manifest".to_string()),
                    path: Some(artifact_manifest.path.clone()),
                    url: None,
                },
                RunnerWorkloadArtifactRef {
                    id: "run_location_index".to_string(),
                    name: Some("run location index".to_string()),
                    path: Some(run_location_index_path),
                    url: None,
                },
            ],
            next_commands,
        };
        Self {
            schema: RUNNER_HANDOFF_ENVELOPE_SCHEMA.to_string(),
            status: "running".to_string(),
            execution_location: format!("runner:{runner_id}"),
            identity: AgentTaskDispatchIdentity {
                runner_id: runner_id.to_string(),
                runner_job_id: job_id.to_string(),
                persisted_run_id: mirror_run_id.clone(),
                run_id: mirror_run_id.clone(),
                handoff_id: Some(format!("runner:{runner_id}:job:{job_id}")),
            },
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            durable_run_id: mirror_run_id.clone(),
            persisted_run_id: mirror_run_id.clone(),
            mirror_run_id,
            artifact_manifest,
            run_location_index,
            remote_cwd,
            path_materialization_plan,
            evidence,
            follow_commands,
        }
    }

    pub fn runner_execution_record(&self) -> RunnerExecutionRecord {
        let record = if self.status == "running" {
            RunnerExecutionRecord::in_flight(self.job_id.clone(), self.runner_id.clone(), "daemon")
        } else {
            RunnerExecutionRecord::terminal(
                self.job_id.clone(),
                self.runner_id.clone(),
                "daemon",
                if self.status == "succeeded" { 0 } else { 1 },
            )
        };
        record
            .with_job_id(self.job_id.clone())
            .with_mirror_run_id(
                self.mirror_run_id
                    .clone()
                    .or_else(|| self.persisted_run_id.clone())
                    .or_else(|| self.durable_run_id.clone()),
            )
            .with_path_materialization_plan(self.path_materialization_plan.clone())
            .with_artifact_refs(self.evidence.artifact_refs.iter().map(|artifact| {
                RunnerExecutionArtifactRef {
                    id: artifact.id.clone(),
                    name: artifact.name.clone(),
                    path: artifact.path.clone(),
                    url: artifact.url.clone(),
                }
            }))
            .with_next_actions(self.evidence.next_commands.iter().map(|command| {
                RunnerExecutionNextAction {
                    label: command.label.clone(),
                    command: command.command.clone(),
                }
            }))
    }

    pub fn run_outcome_envelope(&self) -> RunOutcomeEnvelope {
        RunOutcomeEnvelope::from_runner_execution_record(&self.runner_execution_record())
    }
}

impl RunnerHandoffArtifactManifestRef {
    pub fn for_remote_cwd(remote_cwd: &str) -> Self {
        Self {
            schema: RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA.to_string(),
            manifest_schema: RUNNER_ARTIFACT_MANIFEST_SCHEMA.to_string(),
            path: format!(
                "{}{RUNNER_ARTIFACT_ROOT_DIR_SUFFIX}/{RUNNER_ARTIFACT_MANIFEST_FILE}",
                remote_cwd.trim_end_matches('/')
            ),
        }
    }
}

pub fn run_location_index_path(remote_cwd: &str) -> String {
    format!(
        "{}{RUNNER_ARTIFACT_ROOT_DIR_SUFFIX}/homeboy-run-location-index.json",
        remote_cwd.trim_end_matches('/')
    )
}
