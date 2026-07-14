//! Reviewer-facing projections of agent-task artifacts.
//!
//! Durable scheduler records retain their local artifact paths for operator
//! recovery. Every externally serialized projection goes through this module.

use crate::core::agent_task::AgentTaskArtifact;
use crate::core::agent_task::AgentTaskTypedArtifact;
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskArtifactLineage, AgentTaskArtifactRunBinding,
};

pub(crate) fn reviewer_facing_artifact(artifact: &AgentTaskArtifact) -> AgentTaskArtifact {
    let mut artifact = artifact.clone();
    artifact.path = None;
    artifact
}

pub(crate) fn reviewer_facing_artifacts(artifacts: &[AgentTaskArtifact]) -> Vec<AgentTaskArtifact> {
    artifacts.iter().map(reviewer_facing_artifact).collect()
}

pub(crate) fn reviewer_facing_aggregate(aggregate: &AgentTaskAggregate) -> AgentTaskAggregate {
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
    binding.path = None;
    binding
}

fn reviewer_facing_lineage(mut lineage: AgentTaskArtifactLineage) -> AgentTaskArtifactLineage {
    lineage.path = None;
    lineage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_artifact_projection_omits_operator_local_path() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/private/operator/patch.diff".to_string()),
            url: Some(
                "homeboy://agent-task/run/run-1/artifacts#task=task-1&artifact=patch".to_string(),
            ),
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::Value::Null,
        };
        assert_eq!(reviewer_facing_artifact(&artifact).path, None);
        assert_eq!(
            artifact.path.as_deref(),
            Some("/private/operator/patch.diff")
        );
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
    fn typed_artifact_projection_omits_embedded_portable_path() {
        let artifact = AgentTaskArtifact {
            schema: "homeboy/agent-task-artifact/v1".to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/private/operator/patch.diff".to_string()),
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
            None
        );
        assert_eq!(typed.artifact.unwrap().path, artifact.path);
    }
}
