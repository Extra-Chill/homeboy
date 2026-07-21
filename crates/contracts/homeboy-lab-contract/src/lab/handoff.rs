//! Durable handoff and run-location evidence for detached Lab jobs.

use crate::path_materialization::PathMaterializationPlan;

use super::workload::LabRunnerWorkloadArtifactRef;

pub const LAB_RUNNER_HANDOFF_ENVELOPE_SCHEMA: &str = "homeboy/runner-exec-handoff/v1";
pub const RUN_LOCATION_INDEX_SCHEMA: &str = "homeboy/run-location-index/v1";
pub const RUNNER_ARTIFACT_MANIFEST_REF_NAME: &str = "runner-artifact-manifest-ref";
pub const RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA: &str = "homeboy/runner-artifact-manifest-ref/v1";
// Canonical artifact-manifest identifiers. These MUST stay in sync with
// `core::artifact_manifest::{ARTIFACT_MANIFEST_SCHEMA, ARTIFACT_MANIFEST_FILE}`;
// a compile-time assertion in that module guards against drift. Defined locally
// here so this lab-contract type layer carries no upward dependency on core.
pub const ARTIFACT_MANIFEST_SCHEMA: &str = "homeboy/artifact-manifest/v1";
pub const ARTIFACT_MANIFEST_FILE: &str = "homeboy-artifact-manifest.json";
pub const RUNNER_ARTIFACT_MANIFEST_SCHEMA: &str = ARTIFACT_MANIFEST_SCHEMA;
pub const RUNNER_ARTIFACT_MANIFEST_FILE: &str = ARTIFACT_MANIFEST_FILE;
pub const RUNNER_ARTIFACT_ROOT_DIR_SUFFIX: &str = "-homeboy-artifacts";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerHandoffEnvelope {
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
    pub artifact_manifest: LabRunnerHandoffArtifactManifestRef,
    pub run_location_index: RunLocationIndex,
    pub evidence: LabRunnerHandoffEvidence,
    pub follow_commands: LabRunnerHandoffFollowCommands,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerHandoffArtifactManifestRef {
    pub schema: String,
    pub manifest_schema: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerHandoffEvidence {
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
    pub artifact_manifest: LabRunnerHandoffArtifactManifestRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<LabRunnerWorkloadArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_commands: Vec<LabRunnerHandoffNextCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerHandoffNextCommand {
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

/// The core run/runner/job identity a Lab-offloaded job carries across the
/// controller, the runner snapshot, and terminal lifecycle events.
///
/// This is the canonical shape the whole handoff lifecycle validates against —
/// previously each call site hand-rolled the same `run_id`/`runner_id`/
/// `runner_job_id` tuple comparison with subtly different edge-case handling,
/// which is why identity-binding bugs landed one path at a time (empty-string
/// compares, missing-id-vs-mismatch confusion, etc). Centralizing the
/// comparison here gives every consumer one definition of "same job".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerJobIdentity {
    pub run_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
}

impl RunnerJobIdentity {
    pub fn new(
        run_id: impl Into<String>,
        runner_id: impl Into<String>,
        runner_job_id: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            runner_id: runner_id.into(),
            runner_job_id: runner_job_id.into(),
        }
    }

    /// Whether every identity field is populated. A partially-established
    /// identity (e.g. a controller run before its accepted runner job id is
    /// bound) is not a mismatch — callers should surface "identity not
    /// established" rather than compare against an empty field (#9240).
    pub fn is_complete(&self) -> bool {
        !self.run_id.trim().is_empty()
            && !self.runner_id.trim().is_empty()
            && !self.runner_job_id.trim().is_empty()
    }

    /// Whether two identities name the same run + runner + job.
    pub fn matches(&self, other: &RunnerJobIdentity) -> bool {
        self.run_id == other.run_id
            && self.runner_id == other.runner_id
            && self.runner_job_id == other.runner_job_id
    }

    /// A stable, human-readable description used in mismatch diagnostics.
    pub fn describe(&self) -> String {
        format!(
            "run '{}', runner '{}', job '{}'",
            self.run_id, self.runner_id, self.runner_job_id
        )
    }
}

impl RunnerJobIdentity {
    /// Project a serialized dispatch-identity JSON object onto the canonical
    /// tuple. Accepts the same field names as [`AgentTaskDispatchIdentity`]
    /// (`run_id`/`persisted_run_id`, `runner_id`, `runner_job_id`), preferring
    /// the persisted run id. Returns `None` when the value is not a
    /// dispatch-identity object. Used to compare handoff identities that are
    /// still carried as raw JSON without depending on exact `Value` equality.
    pub fn from_dispatch_value(value: &serde_json::Value) -> Option<RunnerJobIdentity> {
        let object = value.as_object()?;
        let string = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
        let run_id = string("persisted_run_id")
            .or_else(|| string("run_id"))?
            .to_string();
        let runner_id = string("runner_id")?.to_string();
        let runner_job_id = string("runner_job_id")?.to_string();
        Some(RunnerJobIdentity {
            run_id,
            runner_id,
            runner_job_id,
        })
    }
}

impl AgentTaskDispatchIdentity {
    /// Project this dispatch identity onto the canonical run/runner/job tuple.
    /// Prefers the persisted run id (the controller's durable run) and falls
    /// back to the transport `run_id`.
    pub fn runner_job_identity(&self) -> RunnerJobIdentity {
        RunnerJobIdentity {
            run_id: self
                .persisted_run_id
                .clone()
                .or_else(|| self.run_id.clone())
                .unwrap_or_default(),
            runner_id: self.runner_id.clone(),
            runner_job_id: self.runner_job_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRunnerHandoffFollowCommands {
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
    pub artifact_manifest_ref: LabRunnerHandoffArtifactManifestRef,
    pub liveness_heartbeat_timestamp: String,
    pub follow_commands: LabRunnerHandoffFollowCommands,
}

impl LabRunnerHandoffEnvelope {
    pub fn detached_lab_offload(
        runner_id: &str,
        job_id: &str,
        remote_cwd: String,
        path_materialization_plan: Option<PathMaterializationPlan>,
        accepted_run_id: Option<String>,
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
            LabRunnerHandoffNextCommand {
                label: "runner_job_logs".to_string(),
                command: job_logs_command.clone(),
            },
            LabRunnerHandoffNextCommand {
                label: "runner_job_cancel".to_string(),
                command: job_cancel_command.clone(),
            },
        ];
        let actionable_run_id = accepted_run_id.as_ref().or(mirror_run_id.as_ref());
        if let Some(run_id) = actionable_run_id {
            next_commands.extend([
                LabRunnerHandoffNextCommand {
                    label: "run_status".to_string(),
                    command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "status".to_string(),
                        run_id.clone(),
                    ],
                },
                LabRunnerHandoffNextCommand {
                    label: "run_logs".to_string(),
                    command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "logs".to_string(),
                        run_id.clone(),
                    ],
                },
                LabRunnerHandoffNextCommand {
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
        let artifact_manifest = LabRunnerHandoffArtifactManifestRef::for_remote_cwd(&remote_cwd);
        let run_location_index_path = run_location_index_path(&remote_cwd);
        let follow_commands = LabRunnerHandoffFollowCommands {
            job_logs: format!("homeboy runner job logs {runner_id} {job_id} --follow"),
            job_cancel: format!("homeboy runner job cancel {runner_id} {job_id}"),
            status: actionable_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task status {run_id}")),
            logs: actionable_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task logs {run_id}")),
            artifacts: actionable_run_id
                .as_ref()
                .map(|run_id| format!("homeboy agent-task artifacts {run_id}")),
        };
        let run_location_index = RunLocationIndex {
            schema: RUN_LOCATION_INDEX_SCHEMA.to_string(),
            run_id: actionable_run_id
                .cloned()
                .unwrap_or_else(|| format!("runner:{runner_id}:job:{job_id}")),
            controller_location: "controller:local".to_string(),
            runner_id: runner_id.to_string(),
            remote_job_id: job_id.to_string(),
            remote_cwd: remote_cwd.clone(),
            artifact_manifest_ref: artifact_manifest.clone(),
            liveness_heartbeat_timestamp,
            follow_commands: follow_commands.clone(),
        };
        let evidence = LabRunnerHandoffEvidence {
            schema: "homeboy/runner-handoff-evidence/v1".to_string(),
            // The daemon has accepted this job even though its execution remains
            // in flight. Keep that acceptance distinct from terminal state.
            status: "accepted".to_string(),
            runner_id: runner_id.to_string(),
            runner_job_id: job_id.to_string(),
            run_id: actionable_run_id.cloned(),
            persisted_run_id: accepted_run_id.clone(),
            mirror_run_id: mirror_run_id.clone(),
            remote_cwd: remote_cwd.clone(),
            artifact_manifest: artifact_manifest.clone(),
            artifact_refs: vec![
                LabRunnerWorkloadArtifactRef {
                    id: "artifact_manifest".to_string(),
                    name: Some("runner artifact manifest".to_string()),
                    path: Some(artifact_manifest.path.clone()),
                    url: None,
                },
                LabRunnerWorkloadArtifactRef {
                    id: "run_location_index".to_string(),
                    name: Some("run location index".to_string()),
                    path: Some(run_location_index_path),
                    url: None,
                },
            ],
            next_commands,
        };
        Self {
            schema: LAB_RUNNER_HANDOFF_ENVELOPE_SCHEMA.to_string(),
            status: "running".to_string(),
            execution_location: format!("runner:{runner_id}"),
            identity: AgentTaskDispatchIdentity {
                runner_id: runner_id.to_string(),
                runner_job_id: job_id.to_string(),
                persisted_run_id: accepted_run_id.clone(),
                run_id: actionable_run_id.cloned(),
                handoff_id: Some(format!("runner:{runner_id}:job:{job_id}")),
            },
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            durable_run_id: accepted_run_id.clone(),
            persisted_run_id: accepted_run_id,
            mirror_run_id,
            artifact_manifest,
            run_location_index,
            remote_cwd,
            path_materialization_plan,
            evidence,
            follow_commands,
        }
    }
}

impl LabRunnerHandoffArtifactManifestRef {
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

#[cfg(test)]
mod runner_job_identity_tests {
    use super::{AgentTaskDispatchIdentity, RunnerJobIdentity};

    #[test]
    fn matches_requires_all_three_fields_to_agree() {
        let a = RunnerJobIdentity::new("run-1", "runner-a", "job-1");
        assert!(a.matches(&RunnerJobIdentity::new("run-1", "runner-a", "job-1")));
        assert!(!a.matches(&RunnerJobIdentity::new("run-2", "runner-a", "job-1")));
        assert!(!a.matches(&RunnerJobIdentity::new("run-1", "runner-b", "job-1")));
        assert!(!a.matches(&RunnerJobIdentity::new("run-1", "runner-a", "job-2")));
    }

    #[test]
    fn is_complete_flags_a_partially_established_identity() {
        // #9240: a controller run before its accepted runner job id is bound has
        // an empty job id — that is "not established", not a mismatch. Callers
        // rely on this to surface the right diagnostic instead of comparing
        // against an empty field.
        assert!(RunnerJobIdentity::new("run-1", "runner-a", "job-1").is_complete());
        assert!(!RunnerJobIdentity::new("run-1", "runner-a", "").is_complete());
        assert!(!RunnerJobIdentity::new("run-1", "", "job-1").is_complete());
        assert!(!RunnerJobIdentity::new("", "runner-a", "job-1").is_complete());
        assert!(!RunnerJobIdentity::new("run-1", "runner-a", "   ").is_complete());
    }

    #[test]
    fn dispatch_identity_prefers_persisted_run_id() {
        let identity = AgentTaskDispatchIdentity {
            runner_id: "runner-a".to_string(),
            runner_job_id: "job-1".to_string(),
            persisted_run_id: Some("persisted-run".to_string()),
            run_id: Some("transport-run".to_string()),
            handoff_id: None,
        };
        let projected = identity.runner_job_identity();
        assert_eq!(projected.run_id, "persisted-run");
        assert_eq!(projected.runner_id, "runner-a");
        assert_eq!(projected.runner_job_id, "job-1");
    }

    #[test]
    fn dispatch_identity_falls_back_to_transport_run_id() {
        let identity = AgentTaskDispatchIdentity {
            runner_id: "runner-a".to_string(),
            runner_job_id: "job-1".to_string(),
            persisted_run_id: None,
            run_id: Some("transport-run".to_string()),
            handoff_id: None,
        };
        assert_eq!(identity.runner_job_identity().run_id, "transport-run");
    }

    #[test]
    fn from_dispatch_value_projects_the_canonical_tuple() {
        let value = serde_json::json!({
            "runner_id": "runner-a",
            "runner_job_id": "job-1",
            "persisted_run_id": "persisted-run",
            "run_id": "transport-run",
        });
        let identity = RunnerJobIdentity::from_dispatch_value(&value).expect("projectable");
        assert_eq!(identity.run_id, "persisted-run");
        assert_eq!(identity.runner_id, "runner-a");
        assert_eq!(identity.runner_job_id, "job-1");
    }

    #[test]
    fn from_dispatch_value_matches_across_cosmetic_field_differences() {
        // Two serialized identities that name the same job but differ in
        // incidental fields (an extra handoff_id, a missing transport run_id)
        // must still match on the canonical tuple — this is the robustness the
        // typed comparison buys over brittle raw-`Value` equality.
        let stored = serde_json::json!({
            "runner_id": "runner-a",
            "runner_job_id": "job-1",
            "persisted_run_id": "persisted-run",
            "run_id": "persisted-run",
        });
        let replayed = serde_json::json!({
            "runner_id": "runner-a",
            "runner_job_id": "job-1",
            "persisted_run_id": "persisted-run",
            "handoff_id": "handoff-xyz",
        });
        assert_ne!(stored, replayed, "raw Value equality would reject this pair");
        let stored_identity = RunnerJobIdentity::from_dispatch_value(&stored).expect("stored");
        let replayed_identity = RunnerJobIdentity::from_dispatch_value(&replayed).expect("replayed");
        assert!(stored_identity.matches(&replayed_identity));
    }

    #[test]
    fn from_dispatch_value_rejects_a_non_identity_object() {
        assert!(RunnerJobIdentity::from_dispatch_value(&serde_json::json!("not-an-object")).is_none());
        assert!(
            RunnerJobIdentity::from_dispatch_value(&serde_json::json!({ "runner_id": "a" }))
                .is_none(),
            "missing runner_job_id / run_id must not project"
        );
    }
}
