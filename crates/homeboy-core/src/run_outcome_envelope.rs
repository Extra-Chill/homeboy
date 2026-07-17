use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_jobs::JobArtifactMetadata;
use crate::artifact_ref::{artifact_uri, ArtifactRef, EvidenceRef, ARTIFACT_REF_SCHEMA};
use crate::runner_execution_envelope::{RunnerExecutionArtifactRef, RunnerExecutionRecord};
use homeboy_lab_runner_contract::RunnerArtifactRef;

pub const RUN_OUTCOME_ENVELOPE_SCHEMA: &str = "homeboy/run-outcome-envelope/v1";
pub const RUN_OUTCOME_ENVELOPE_FILE: &str = "run-outcome-envelope.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunOutcomeEnvelope {
    #[serde(default = "run_outcome_envelope_schema")]
    pub schema: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<RunOutcomeHandoffRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunOutcomeProjection {
    pub schema: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<RunOutcomeHandoffRef>,
}

impl RunOutcomeEnvelope {
    pub fn new(status: impl Into<String>) -> Self {
        Self {
            schema: run_outcome_envelope_schema(),
            status: status.into(),
            run_id: None,
            runner_id: None,
            exit_code: None,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            handoffs: Vec::new(),
            result: Value::Null,
        }
    }

    pub fn with_run_id(mut self, run_id: Option<String>) -> Self {
        self.run_id = run_id;
        self
    }

    pub fn with_runner_id(mut self, runner_id: Option<String>) -> Self {
        self.runner_id = runner_id;
        self
    }

    pub fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = Some(exit_code);
        self
    }

    pub fn with_result(mut self, result: Value) -> Self {
        self.result = result;
        self
    }

    pub fn from_runner_execution_record(record: &RunnerExecutionRecord) -> Self {
        let run_id = runner_execution_record_run_id(record);
        let mut envelope = Self::new(record.status.clone())
            .with_run_id(Some(run_id.clone()))
            .with_runner_id(Some(record.runner_id.clone()));
        envelope.add_runner_execution_artifact_refs(&run_id, record.artifact_refs.clone());
        envelope
    }

    pub fn add_job_artifact_refs(
        &mut self,
        run_id: &str,
        artifacts: impl IntoIterator<Item = JobArtifactMetadata>,
    ) {
        for artifact in artifacts {
            self.push_artifact_ref(job_artifact_metadata_ref(run_id, artifact));
        }
    }

    pub fn add_runner_artifact_refs(
        &mut self,
        run_id: &str,
        artifacts: impl IntoIterator<Item = RunnerArtifactRef>,
    ) {
        for artifact in artifacts {
            self.push_artifact_ref(runner_artifact_ref(run_id, artifact));
        }
    }

    pub fn add_runner_execution_artifact_refs(
        &mut self,
        run_id: &str,
        artifacts: impl IntoIterator<Item = RunnerExecutionArtifactRef>,
    ) {
        for artifact in artifacts {
            self.push_artifact_ref(runner_execution_artifact_ref(run_id, artifact));
        }
    }

    /// Record a runner handoff. Takes the already-extracted primitive fields
    /// (rather than the runner-owned `RunnerHandoff` type) so this core envelope
    /// does not depend on runner.
    pub fn add_handoff(
        &mut self,
        runner_id: String,
        transport: String,
        lifecycle_owner: String,
        job_id: Option<String>,
    ) {
        self.handoffs.push(RunOutcomeHandoffRef {
            runner_id,
            transport,
            lifecycle_owner,
            job_id,
        });
    }

    pub fn projection(&self) -> RunOutcomeProjection {
        RunOutcomeProjection {
            schema: self.schema.clone(),
            status: self.status.clone(),
            run_id: self.run_id.clone(),
            runner_id: self.runner_id.clone(),
            exit_code: self.exit_code,
            artifact_refs: self.artifact_refs.clone(),
            evidence_refs: self.evidence_refs.clone(),
            handoffs: self.handoffs.clone(),
        }
    }

    fn push_artifact_ref(&mut self, artifact: ArtifactRef) {
        if self
            .artifact_refs
            .iter()
            .any(|existing| existing.id == artifact.id && existing.run_id == artifact.run_id)
        {
            return;
        }
        self.evidence_refs.push(EvidenceRef {
            schema: crate::artifact_ref::EVIDENCE_REF_SCHEMA.to_string(),
            kind: "artifact".to_string(),
            target: artifact.canonical_uri(),
            label: artifact
                .semantic_key
                .clone()
                .or_else(|| artifact.role.clone())
                .unwrap_or_else(|| artifact.id.clone()),
            role: artifact.role.clone(),
            semantic_key: artifact.semantic_key.clone(),
            artifact: Some(artifact.clone()),
        });
        self.artifact_refs.push(artifact);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunOutcomeHandoffRef {
    pub runner_id: String,
    pub transport: String,
    pub lifecycle_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

fn job_artifact_metadata_ref(run_id: &str, artifact: JobArtifactMetadata) -> ArtifactRef {
    let metadata = artifact.metadata.as_ref();
    project_artifact_ref(
        run_id,
        ArtifactProjection {
            id: artifact.id,
            kind: metadata_string(metadata, "kind"),
            path: artifact.path,
            url: artifact.url,
            role: metadata_string(metadata, "role"),
            semantic_key: metadata_string(metadata, "semantic_key")
                .or_else(|| metadata_string(metadata, "semanticKey")),
        },
    )
}

fn runner_artifact_ref(run_id: &str, artifact: RunnerArtifactRef) -> ArtifactRef {
    project_artifact_ref(
        run_id,
        ArtifactProjection::from_named(
            artifact.artifact_id,
            artifact.name,
            artifact.path,
            artifact.url,
        ),
    )
}

fn runner_execution_artifact_ref(
    run_id: &str,
    artifact: RunnerExecutionArtifactRef,
) -> ArtifactRef {
    project_artifact_ref(
        run_id,
        ArtifactProjection::from_named(artifact.id, artifact.name, artifact.path, artifact.url),
    )
}

struct ArtifactProjection {
    id: String,
    kind: Option<String>,
    path: Option<String>,
    url: Option<String>,
    role: Option<String>,
    semantic_key: Option<String>,
}

impl ArtifactProjection {
    fn from_named(
        id: String,
        name: Option<String>,
        path: Option<String>,
        url: Option<String>,
    ) -> Self {
        Self {
            id,
            kind: name.clone(),
            path,
            url,
            role: None,
            semantic_key: name,
        }
    }
}

fn project_artifact_ref(run_id: &str, artifact: ArtifactProjection) -> ArtifactRef {
    let artifact_type = artifact_type(artifact.path.as_deref(), artifact.url.as_deref());
    let path = artifact
        .path
        .unwrap_or_else(|| artifact_uri(run_id, &artifact.id));
    ArtifactRef {
        schema: ARTIFACT_REF_SCHEMA.to_string(),
        id: artifact.id,
        run_id: run_id.to_string(),
        kind: artifact.kind.unwrap_or_else(|| "artifact".to_string()),
        artifact_type,
        path,
        url: artifact.url,
        public_url: None,
        role: artifact.role,
        semantic_key: artifact.semantic_key,
    }
}

fn metadata_string(metadata: Option<&Value>, key: &str) -> Option<String> {
    metadata?.get(key)?.as_str().map(ToString::to_string)
}

fn runner_execution_record_run_id(record: &RunnerExecutionRecord) -> String {
    record
        .remote_run_id
        .clone()
        .or_else(|| record.mirror_run_id.clone())
        .or_else(|| record.agent_task_run_id.clone())
        .or_else(|| record.local_run_id.clone())
        .or_else(|| record.job_id.clone())
        .unwrap_or_else(|| record.execution_id.clone())
}

fn artifact_type(path: Option<&str>, url: Option<&str>) -> String {
    if path.is_some() || url.is_some() {
        "file".to_string()
    } else {
        "reference".to_string()
    }
}

fn run_outcome_envelope_schema() -> String {
    RUN_OUTCOME_ENVELOPE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn run_outcome_envelope_normalizes_artifact_and_evidence_refs() {
        let mut envelope = RunOutcomeEnvelope::new("succeeded")
            .with_run_id(Some("run-1".to_string()))
            .with_runner_id(Some("runner-1".to_string()))
            .with_exit_code(0);
        envelope.add_job_artifact_refs(
            "run-1",
            vec![JobArtifactMetadata {
                id: "summary".to_string(),
                name: Some("summary.json".to_string()),
                path: Some("summary.json".to_string()),
                url: Some("https://artifacts.example.test/summary.json".to_string()),
                mime: Some("application/json".to_string()),
                size_bytes: Some(42),
                sha256: Some("abc123".to_string()),
                content_base64: None,
                metadata: Some(json!({
                    "kind": "run_summary",
                    "role": "summary",
                    "semantic_key": "run.summary"
                })),
            }],
        );

        let value = serde_json::to_value(&envelope).expect("json");

        assert_eq!(value["schema"], RUN_OUTCOME_ENVELOPE_SCHEMA);
        assert_eq!(value["artifact_refs"][0]["schema"], ARTIFACT_REF_SCHEMA);
        assert_eq!(value["artifact_refs"][0]["kind"], "run_summary");
        assert_eq!(
            value["evidence_refs"][0]["target"],
            "homeboy://run/run-1/artifact/summary"
        );
        assert_eq!(value["evidence_refs"][0]["role"], "summary");
    }

    #[test]
    fn run_outcome_projection_exposes_inspection_fields_without_result_payload() {
        let mut envelope = RunOutcomeEnvelope::new("succeeded")
            .with_run_id(Some("run-1".to_string()))
            .with_runner_id(Some("runner-1".to_string()))
            .with_exit_code(0)
            .with_result(json!({ "large": "payload" }));
        envelope.add_job_artifact_refs(
            "run-1",
            vec![JobArtifactMetadata {
                id: "summary".to_string(),
                name: Some("summary.json".to_string()),
                path: Some("summary.json".to_string()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                content_base64: None,
                metadata: None,
            }],
        );

        let value = serde_json::to_value(envelope.projection()).expect("projection json");

        assert_eq!(value["schema"], RUN_OUTCOME_ENVELOPE_SCHEMA);
        assert_eq!(value["run_id"], "run-1");
        assert_eq!(value["runner_id"], "runner-1");
        assert_eq!(value["artifact_refs"][0]["id"], "summary");
        assert!(value.get("result").is_none());
    }

    #[test]
    fn run_outcome_envelope_projects_terminal_runner_execution_record() {
        let record = RunnerExecutionRecord::terminal("job-1", "lab-a", "daemon", 0)
            .with_job_id("job-1")
            .with_mirror_run_id(Some("run-1".to_string()))
            .with_artifact_refs(vec![RunnerExecutionArtifactRef {
                id: "report".to_string(),
                name: Some("summary".to_string()),
                path: Some("artifacts/summary.json".to_string()),
                url: None,
            }]);

        let value = serde_json::to_value(RunOutcomeEnvelope::from_runner_execution_record(&record))
            .expect("outcome json");

        assert_eq!(value["schema"], RUN_OUTCOME_ENVELOPE_SCHEMA);
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["run_id"], "run-1");
        assert_eq!(value["runner_id"], "lab-a");
        assert_eq!(value["artifact_refs"][0]["id"], "report");
        assert_eq!(
            value["evidence_refs"][0]["target"],
            "homeboy://run/run-1/artifact/report"
        );
        assert!(value.get("transport").is_none());
        assert!(value.get("materialized_paths").is_none());
        assert!(value.get("next_actions").is_none());
    }

    #[test]
    fn artifact_projection_serialized_output_snapshot_preserves_source_semantics() {
        let job_snake = job_artifact_metadata_ref(
            "run-1",
            job_artifact(
                "job-snake",
                Some(json!({
                    "kind": "report",
                    "role": "summary",
                    "semantic_key": "snake-wins",
                    "semanticKey": "camel-loses"
                })),
            ),
        );
        let job_camel = job_artifact_metadata_ref(
            "run-1",
            job_artifact("job-camel", Some(json!({ "semanticKey": "camel" }))),
        );
        let runner = runner_artifact_ref(
            "run-1",
            RunnerArtifactRef {
                artifact_id: "runner".to_string(),
                name: Some("runner-name".to_string()),
                path: Some("artifacts/runner.json".to_string()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                transport: None,
            },
        );
        let execution = runner_execution_artifact_ref(
            "run-1",
            RunnerExecutionArtifactRef {
                id: "execution".to_string(),
                name: None,
                path: None,
                url: Some("https://example.test/execution.json".to_string()),
            },
        );

        assert_eq!(
            serde_json::to_string(&vec![job_snake, job_camel, runner, execution])
                .expect("artifact projection json"),
            concat!(
                r#"[{"schema":"homeboy/artifact-ref/v1","id":"job-snake","run_id":"run-1","kind":"report","type":"reference","path":"homeboy://run/run-1/artifact/job-snake","role":"summary","semantic_key":"snake-wins"},"#,
                r#"{"schema":"homeboy/artifact-ref/v1","id":"job-camel","run_id":"run-1","kind":"artifact","type":"reference","path":"homeboy://run/run-1/artifact/job-camel","semantic_key":"camel"},"#,
                r#"{"schema":"homeboy/artifact-ref/v1","id":"runner","run_id":"run-1","kind":"runner-name","type":"file","path":"artifacts/runner.json","semantic_key":"runner-name"},"#,
                r#"{"schema":"homeboy/artifact-ref/v1","id":"execution","run_id":"run-1","kind":"artifact","type":"file","path":"homeboy://run/run-1/artifact/execution","url":"https://example.test/execution.json"}]"#,
            )
        );
    }

    fn job_artifact(id: &str, metadata: Option<Value>) -> JobArtifactMetadata {
        JobArtifactMetadata {
            id: id.to_string(),
            name: None,
            path: None,
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            content_base64: None,
            metadata,
        }
    }
}
