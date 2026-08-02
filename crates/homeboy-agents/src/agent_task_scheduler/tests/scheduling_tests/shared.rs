//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

// Child modules import this prelude explicitly; parent imports do not propagate.
pub(super) use super::super::fixtures::*;
pub(super) use crate::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskArtifactDeclaration,
    AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA,
};
pub(super) use crate::agent_task_scheduler::attempt_workspace::fingerprint;
pub(super) use crate::agent_task_scheduler::harvest::git_output_raw;
pub(super) use crate::agent_task_scheduler::*;
pub(super) use serde_json::{json, Value};
pub(super) use std::collections::HashMap;
pub(super) use std::fs;
pub(super) use std::process::Command;
pub(super) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::thread;
pub(super) use std::time::{Duration, Instant};

pub(super) fn concept_packet_declaration() -> AgentTaskArtifactDeclaration {
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

pub(super) struct ConceptPacketExecutor {
    pub(super) observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
    pub(super) emit_concept_packet: bool,
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

pub(super) struct GenericChildRunExecutor;

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

/// Spin until `ready` reports true, or panic naming the condition.
///
/// These suites coordinate with executor threads through atomics, and the
/// obvious spelling — `while !flag.load(..) { sleep(..) }` — has no exit. When
/// the executor never reaches the state the test is waiting for, the loop
/// spins until the 1500s CI budget kills the entire test phase, which reports
/// `exit 124` with `failed: 0` and no test name. Every gate on every pull
/// request then says the same uninformative thing, and the suite that actually
/// broke is invisible (#10687).
///
/// Bounding the wait converts that into one named failing test, which is the
/// same contract `bounded_output` gives hermetic subprocesses (#11234).
pub(super) fn wait_until(label: &str, ready: impl Fn() -> bool) {
    // Generous: these waits gate on real thread scheduling and git fixtures,
    // so the budget only has to be short enough to beat the CI phase timeout.
    const BUDGET: Duration = Duration::from_secs(60);
    const POLL: Duration = Duration::from_millis(2);

    let started = Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < BUDGET,
            "timed out after {:?} waiting for {label}; the scheduler never reached this state, \
             so the condition is either unreachable or slower than the budget",
            started.elapsed()
        );
        thread::sleep(POLL);
    }
}
