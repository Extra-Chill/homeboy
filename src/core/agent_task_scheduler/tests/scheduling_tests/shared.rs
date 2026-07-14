//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::super::fixtures::*;
use super::super::*;
use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskArtifactDeclaration,
    AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

fn concept_packet_declaration() -> AgentTaskArtifactDeclaration {
    AgentTaskArtifactDeclaration {
        name: "concept_packet".to_string(),
        artifact_type: Some("concept_packet".to_string()),
        artifact_schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
        path: None,
        required: true,
        description: None,
        metadata: Value::Null,
    }
}

struct ConceptPacketExecutor {
    observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
    emit_concept_packet: bool,
}

impl AgentTaskExecutorAdapter for ConceptPacketExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        self.observed
            .lock()
            .expect("observed requests")
            .push(request.clone());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: if self.emit_concept_packet {
                vec![AgentTaskTypedArtifact {
                    name: "concept_packet".to_string(),
                    artifact_type: Some("concept_packet".to_string()),
                    artifact_schema: Some("wp-site-generator/ConceptPacket/v1".to_string()),
                    payload: json!({ "title": "Typed concept" }),
                    artifact: None,
                    metadata: json!({ "source": "sample-runtime/artifact-result-envelope/v1" }),
                }]
            } else {
                Vec::new()
            },
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

struct GenericChildRunExecutor;

impl AgentTaskExecutorAdapter for GenericChildRunExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("generic fuzz case completed".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: format!("artifact-{}", request.task_id),
                kind: "fuzz-report".to_string(),
                name: Some("report.json".to_string()),
                label: Some("Fuzz report".to_string()),
                role: Some("fuzz_report".to_string()),
                semantic_key: Some("fuzz.report".to_string()),
                path: Some(format!("artifacts/{}/report.json", request.task_id)),
                url: None,
                mime: Some("application/json".to_string()),
                size_bytes: Some(512),
                sha256: Some(format!("sha256:{}", request.task_id)),
                metadata: json!({ "case_id": request.task_id }),
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({ "case_id": request.task_id }),
            workflow: None,
            follow_up: None,
            metadata: json!({
                "provider": "generic-fuzz",
                "child_run_id": format!("child-{}", request.task_id)
            }),
        }
    }
}
