use std::path::Path;

use serde::{Deserialize, Serialize};

use super::pipeline::PipelineOutcome;
use super::spec::RigSpec;
use crate::core::observation::{ArtifactRecord, ObservationStore, RunEvidenceCommands, RunRecord};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RigRunArtifactIndex {
    pub run_id: String,
    pub rig_id: String,
    pub status: String,
    pub artifact_root: String,
    pub artifact_index_path: String,
    pub artifact_index_command: String,
    #[serde(flatten)]
    pub evidence_commands: RunEvidenceCommands,
    pub export_command: String,
    pub retrieval_commands: Vec<String>,
    pub key_report_refs: Vec<RigRunArtifactRef>,
    pub failed_step_refs: Vec<RigRunFailedStepRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RigRunArtifactRef {
    pub id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RigRunFailedStepRef {
    pub pipeline: String,
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn for_completed_rig_run(
    store: &ObservationStore,
    rig: &RigSpec,
    run_id: &str,
    status: &str,
    pipeline: Option<&PipelineOutcome>,
) -> Option<RigRunArtifactIndex> {
    let artifact_root = crate::core::artifact_root().ok()?;
    let run_artifact_root = artifact_root.join(run_id);
    let artifact_index_path = run_artifact_root.join("rig-artifact-index.json");
    let artifacts = store.list_artifacts(run_id).unwrap_or_default();
    let mut index = build(
        &rig.id,
        run_id,
        status,
        &artifact_root,
        &artifact_index_path,
        &artifacts,
        pipeline.map(failed_step_refs).unwrap_or_default(),
    );

    write_index_file(&artifact_index_path, &index);
    if let Ok(artifact) = store.record_artifact(run_id, "rig_artifact_index", &artifact_index_path)
    {
        index.key_report_refs.insert(0, artifact_ref(&artifact));
    }
    Some(index)
}

pub fn for_run(store: &ObservationStore, run: &RunRecord) -> Option<RigRunArtifactIndex> {
    let rig_id = run.rig_id.as_ref()?;
    if run.kind != "rig" {
        return None;
    }
    let artifact_root = crate::core::artifact_root().ok()?;
    let artifact_index_path = artifact_root.join(&run.id).join("rig-artifact-index.json");
    let artifacts = store.list_artifacts(&run.id).unwrap_or_default();
    Some(build(
        rig_id,
        &run.id,
        &run.status,
        &artifact_root,
        &artifact_index_path,
        &artifacts,
        failed_step_refs_from_metadata(&run.metadata_json),
    ))
}

fn build(
    rig_id: &str,
    run_id: &str,
    status: &str,
    artifact_root: &Path,
    artifact_index_path: &Path,
    artifacts: &[ArtifactRecord],
    failed_step_refs: Vec<RigRunFailedStepRef>,
) -> RigRunArtifactIndex {
    let artifacts_command = format!("homeboy runs artifacts {run_id}");
    let evidence_command = format!("homeboy runs evidence {run_id}");
    let export_command = format!(
        "homeboy runs export --run {run_id} --output ~/.local/share/homeboy/exports/{run_id}"
    );
    let key_report_refs = artifacts
        .iter()
        .filter(|artifact| artifact_is_key_report_ref(artifact))
        .map(artifact_ref)
        .collect::<Vec<_>>();
    RigRunArtifactIndex {
        run_id: run_id.to_string(),
        rig_id: rig_id.to_string(),
        status: status.to_string(),
        artifact_root: artifact_root.display().to_string(),
        artifact_index_path: artifact_index_path.display().to_string(),
        artifact_index_command: artifacts_command.clone(),
        evidence_commands: RunEvidenceCommands {
            evidence_command: evidence_command.clone(),
            artifacts_command: artifacts_command.clone(),
        },
        export_command: export_command.clone(),
        retrieval_commands: vec![artifacts_command, evidence_command, export_command],
        key_report_refs,
        failed_step_refs,
    }
}

fn write_index_file(path: &Path, index: &RigRunArtifactIndex) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(index) {
        let _ = std::fs::write(path, bytes);
    }
}

fn artifact_is_key_report_ref(artifact: &ArtifactRecord) -> bool {
    [artifact.kind.as_str(), artifact.path.as_str()]
        .iter()
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("report")
                || value.contains("summary")
                || value.contains("result")
                || value.contains("evidence")
                || value.contains("index")
        })
}

fn artifact_ref(artifact: &ArtifactRecord) -> RigRunArtifactRef {
    RigRunArtifactRef {
        id: artifact.id.clone(),
        kind: artifact.kind.clone(),
        artifact_type: artifact.artifact_type.clone(),
        path: artifact.path.clone(),
        url: artifact
            .url
            .clone()
            .or_else(|| (artifact.artifact_type == "url").then(|| artifact.path.clone())),
        get_command: (artifact.artifact_type == "file").then(|| {
            format!(
                "homeboy runs artifact get {} {}",
                artifact.run_id, artifact.id
            )
        }),
    }
}

fn failed_step_refs(pipeline: &PipelineOutcome) -> Vec<RigRunFailedStepRef> {
    pipeline
        .steps
        .iter()
        .filter(|step| step.status == "fail")
        .map(|step| RigRunFailedStepRef {
            pipeline: pipeline.name.clone(),
            kind: step.kind.clone(),
            label: step.label.clone(),
            error: step.error.clone(),
        })
        .collect()
}

fn failed_step_refs_from_metadata(metadata: &serde_json::Value) -> Vec<RigRunFailedStepRef> {
    let Some(pipeline) = metadata.get("pipeline") else {
        return Vec::new();
    };
    let pipeline_name = pipeline
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("pipeline")
        .to_string();
    pipeline
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| step.get("status").and_then(serde_json::Value::as_str) == Some("fail"))
        .map(|step| RigRunFailedStepRef {
            pipeline: pipeline_name.clone(),
            kind: step
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("step")
                .to_string(),
            label: step
                .get("label")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unnamed step")
                .to_string(),
            error: step
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
        .collect()
}
