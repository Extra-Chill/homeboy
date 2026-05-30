use std::collections::HashMap;

use crate::core::execution::{
    ChangeArtifact, ChangeArtifactProvenance, ExecutionMode, ExecutionRun, ExecutionStatus,
    ExecutionStepResult,
};
use crate::core::plan::PlanSubject;

use super::types::{ReleaseArtifact, ReleaseRun, ReleaseStepResult, ReleaseStepStatus};

impl From<&ReleaseRun> for ExecutionRun {
    fn from(run: &ReleaseRun) -> Self {
        let run_id = format!("release.{}", run.component_id);

        Self {
            id: run_id.clone(),
            request_id: None,
            mode: ExecutionMode::Execute,
            subject: PlanSubject {
                component_id: Some(run.component_id.clone()),
                ..PlanSubject::default()
            },
            status: release_status_to_execution_status(&run.result.status),
            steps: run
                .result
                .steps
                .iter()
                .map(ExecutionStepResult::from)
                .collect(),
            artifacts: release_artifacts_from_run(&run_id, &run.result.steps),
            warnings: run.result.warnings.clone(),
            metadata: HashMap::from([("enabled".to_string(), serde_json::json!(run.enabled))]),
        }
    }
}

impl From<&ReleaseStepResult> for ExecutionStepResult {
    fn from(step: &ReleaseStepResult) -> Self {
        Self {
            id: step.id.clone(),
            kind: step.step_type.clone(),
            status: release_status_to_execution_status(&step.status),
            summary: None,
            artifacts: release_artifact_ids(&step.id, step.data.as_ref()),
            warnings: step.warnings.clone(),
            data: step.data.clone(),
            error: step.error.clone(),
        }
    }
}

fn release_status_to_execution_status(status: &ReleaseStepStatus) -> ExecutionStatus {
    match status {
        ReleaseStepStatus::Success => ExecutionStatus::Success,
        ReleaseStepStatus::PartialSuccess => ExecutionStatus::PartialSuccess,
        ReleaseStepStatus::Failed => ExecutionStatus::Failed,
        ReleaseStepStatus::Skipped => ExecutionStatus::Skipped,
        ReleaseStepStatus::Missing => ExecutionStatus::Missing,
    }
}

fn release_artifacts_from_run(run_id: &str, steps: &[ReleaseStepResult]) -> Vec<ChangeArtifact> {
    steps
        .iter()
        .flat_map(|step| release_artifacts_from_step(run_id, step))
        .collect()
}

fn release_artifacts_from_step(run_id: &str, step: &ReleaseStepResult) -> Vec<ChangeArtifact> {
    let Some(data) = step.data.as_ref() else {
        return Vec::new();
    };

    let Ok(artifacts) = serde_json::from_value::<Vec<ReleaseArtifact>>(data.clone()) else {
        return Vec::new();
    };

    artifacts
        .into_iter()
        .enumerate()
        .map(|(index, artifact)| {
            let ReleaseArtifact {
                path: release_artifact_path,
                artifact_type,
                platform,
            } = artifact;
            let id = format!("{}.artifact.{}", step.id, index + 1);
            let artifact_type = artifact_type.unwrap_or_else(|| "release_artifact".to_string());
            let mut metadata = HashMap::new();
            if let Some(platform) = platform {
                metadata.insert("platform".to_string(), serde_json::Value::String(platform));
            }

            ChangeArtifact {
                id,
                artifact_type,
                provenance: ChangeArtifactProvenance {
                    source: "release".to_string(),
                    run_id: Some(run_id.to_string()),
                    step_id: Some(step.id.clone()),
                    command: None,
                    captured_at: None,
                },
                title: None,
                summary: None,
                path: Some(release_artifact_path),
                files: Vec::new(),
                diff: None,
                approval_scope: None,
                metadata,
            }
        })
        .collect()
}

fn release_artifact_ids(step_id: &str, data: Option<&serde_json::Value>) -> Vec<String> {
    let Some(data) = data else {
        return Vec::new();
    };

    let Ok(artifacts) = serde_json::from_value::<Vec<ReleaseArtifact>>(data.clone()) else {
        return Vec::new();
    };

    artifacts
        .iter()
        .enumerate()
        .map(|(index, _)| format!("{}.artifact.{}", step_id, index + 1))
        .collect()
}
