//! Reviewer-facing projections of agent-task artifacts.
//!
//! Durable scheduler records retain their local artifact paths for operator
//! recovery. Every externally serialized projection goes through this module.

use crate::agent_task::AgentTaskArtifact;
use crate::agent_task::AgentTaskTypedArtifact;
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskArtifactLineage, AgentTaskArtifactRunBinding,
};
use crate::artifact_manifest::normalize_relative_artifact_path;

pub(crate) fn reviewer_facing_artifact(artifact: &AgentTaskArtifact) -> AgentTaskArtifact {
    let mut artifact = artifact.clone();
    artifact.path = reviewer_facing_path(artifact.path.as_deref());
    artifact
}

pub(crate) fn reviewer_facing_artifacts(artifacts: &[AgentTaskArtifact]) -> Vec<AgentTaskArtifact> {
    artifacts.iter().map(reviewer_facing_artifact).collect()
}

pub fn reviewer_facing_aggregate(aggregate: &AgentTaskAggregate) -> AgentTaskAggregate {
    let mut aggregate = aggregate.clone();
    for outcome in &mut aggregate.outcomes {
        outcome.artifacts = reviewer_facing_artifacts(&outcome.artifacts);
        for typed_artifact in &mut outcome.typed_artifacts {
            *typed_artifact = reviewer_facing_typed_artifact(typed_artifact);
        }
    }
    aggregate.artifact_lineage = aggregate
        .artifact_lineage
        .into_iter()
        .map(reviewer_facing_lineage)
        .collect();
    aggregate.artifact_bindings = aggregate
        .artifact_bindings
        .into_iter()
        .map(reviewer_facing_binding)
        .collect();
    aggregate
}

pub(crate) fn reviewer_facing_typed_artifact(
    typed_artifact: &AgentTaskTypedArtifact,
) -> AgentTaskTypedArtifact {
    let mut typed_artifact = typed_artifact.clone();
    typed_artifact.artifact = typed_artifact
        .artifact
        .as_ref()
        .map(reviewer_facing_artifact);
    typed_artifact
}

pub(crate) fn reviewer_facing_binding(
    mut binding: AgentTaskArtifactRunBinding,
) -> AgentTaskArtifactRunBinding {
    binding.path = reviewer_facing_path(binding.path.as_deref());
    binding
}

fn reviewer_facing_lineage(mut lineage: AgentTaskArtifactLineage) -> AgentTaskArtifactLineage {
    lineage.path = reviewer_facing_path(lineage.path.as_deref());
    lineage
}

fn reviewer_facing_path(path: Option<&str>) -> Option<String> {
    path.and_then(|path| normalize_relative_artifact_path(path).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_projection_preserves_safe_relative_path() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("artifacts/patch.diff".to_string()),
            url: Some(
                "homeboy://agent-task/run/run-1/artifacts#task=task-1&artifact=patch".to_string(),
            ),
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        };
        assert_eq!(
            reviewer_facing_artifact(&artifact).path.as_deref(),
            Some("artifacts/patch.diff")
        );
        assert_eq!(artifact.path.as_deref(), Some("artifacts/patch.diff"));
    }

    #[test]
    fn local_only_artifact_projection_omits_operator_local_path() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "local".to_string(),
            kind: "report".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/private/operator/report.json".to_string()),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        };
        assert_eq!(reviewer_facing_artifact(&artifact).path, None);
        assert_eq!(
            artifact.path.as_deref(),
            Some("/private/operator/report.json")
        );
    }

    #[test]
    fn typed_artifact_projection_preserves_safe_embedded_path() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("artifacts\\patch.diff".to_string()),
            url: Some(
                "homeboy://agent-task/run/run-1/artifacts#task=task-1&artifact=patch".to_string(),
            ),
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        };
        let typed = AgentTaskTypedArtifact {
            name: "patch".to_string(),
            artifact_type: None,
            artifact_schema: None,
            payload: serde_json::Value::Null,
            artifact: Some(artifact.clone()),
            metadata: serde_json::Value::Null,
        };
        assert_eq!(
            reviewer_facing_typed_artifact(&typed)
                .artifact
                .unwrap()
                .path,
            Some("artifacts/patch.diff".to_string())
        );
        assert_eq!(typed.artifact.unwrap().path, artifact.path);
    }

    #[test]
    fn projection_clears_unsafe_paths_in_artifacts_bindings_and_lineage() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "report".to_string(),
            kind: "report".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("C:\\controller\\report.json".to_string()),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        };
        let binding = AgentTaskArtifactRunBinding {
            task_id: "task".to_string(),
            run_id: "run".to_string(),
            artifact_id: "report".to_string(),
            kind: "report".to_string(),
            name: None,
            path: Some("artifacts/../report.json".to_string()),
            url: None,
            sha256: None,
        };
        let lineage = AgentTaskArtifactLineage {
            task_id: "task".to_string(),
            name: "report".to_string(),
            kind: "report".to_string(),
            schema: None,
            artifact_id: Some("report".to_string()),
            path: Some("artifacts/.git/config".to_string()),
            url: None,
            sha256: None,
            payload: serde_json::Value::Null,
        };

        assert_eq!(reviewer_facing_artifact(&artifact).path, None);
        assert_eq!(reviewer_facing_binding(binding).path, None);
        assert_eq!(reviewer_facing_lineage(lineage).path, None);
    }
}
