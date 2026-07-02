use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::api_jobs::JobArtifactMetadata;
use crate::core::artifact_ref::{artifact_uri, ArtifactRef, EvidenceRef, ARTIFACT_REF_SCHEMA};
use crate::core::runner::{RunnerArtifactRef, RunnerHandoff};

pub const RUN_OUTCOME_ENVELOPE_SCHEMA: &str = "homeboy/run-outcome-envelope/v1";

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

    pub fn add_handoff(&mut self, handoff: &RunnerHandoff) {
        self.handoffs.push(RunOutcomeHandoffRef {
            runner_id: handoff.runner_id.clone(),
            transport: handoff.transport.clone(),
            lifecycle_owner: serde_json::to_value(&handoff.lifecycle_owner)
                .ok()
                .and_then(|value| value.as_str().map(ToString::to_string))
                .unwrap_or_else(|| "unknown".to_string()),
            job_id: handoff.job.as_ref().map(|job| job.job_id.clone()),
        });
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
            schema: crate::core::artifact_ref::EVIDENCE_REF_SCHEMA.to_string(),
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
    let id = artifact.id.clone();
    let role = artifact
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("role"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let semantic_key = artifact
        .metadata
        .as_ref()
        .and_then(|metadata| {
            metadata
                .get("semantic_key")
                .or_else(|| metadata.get("semanticKey"))
        })
        .and_then(Value::as_str)
        .map(ToString::to_string);
    ArtifactRef {
        schema: ARTIFACT_REF_SCHEMA.to_string(),
        id: id.clone(),
        run_id: run_id.to_string(),
        kind: artifact
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("artifact")
            .to_string(),
        artifact_type: artifact_type(artifact.path.as_deref(), artifact.url.as_deref()),
        path: artifact.path.unwrap_or_else(|| artifact_uri(run_id, &id)),
        url: artifact.url,
        public_url: None,
        role,
        semantic_key,
    }
}

fn runner_artifact_ref(run_id: &str, artifact: RunnerArtifactRef) -> ArtifactRef {
    let id = artifact.artifact_id.clone();
    ArtifactRef {
        schema: ARTIFACT_REF_SCHEMA.to_string(),
        id: id.clone(),
        run_id: run_id.to_string(),
        kind: artifact
            .name
            .clone()
            .unwrap_or_else(|| "artifact".to_string()),
        artifact_type: artifact_type(artifact.path.as_deref(), artifact.url.as_deref()),
        path: artifact.path.unwrap_or_else(|| artifact_uri(run_id, &id)),
        url: artifact.url,
        public_url: None,
        role: None,
        semantic_key: artifact.name,
    }
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
}
