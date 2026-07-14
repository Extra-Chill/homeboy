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

mod timeout_tests {
    use super::*;

    #[test]
    fn normalizes_slow_task_to_timeout() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn timeout_with_completed_runtime_artifacts_is_discoverable_and_promotable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("fix.patch");
        fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        let agent_result_path = artifact_root.join("agent-result.json");
        fs::write(
            &agent_result_path,
            serde_json::to_string(&AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("patch ready".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "fix".to_string(),
                    kind: "patch".to_string(),
                    name: Some("fix.patch".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: json!({ "role": "patch" }),
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "runtime_bundle".to_string(),
                    uri: artifact_root.display().to_string(),
                    label: Some("runtime bundle".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: json!({}),
            })
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::CandidateRecoverable
        );
        assert_eq!(aggregate.totals.candidate_recoverable, 1);
        assert_eq!(aggregate.totals.timed_out, 0);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1"
                && event.state == AgentTaskState::CandidateRecoverable));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "transcript"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "agent_result"
                && evidence.uri == agent_result_path.display().to_string()));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "scheduler_timeout"
                && diagnostic
                    .data
                    .get("candidate_recoverable")
                    .and_then(Value::as_bool)
                    == Some(true)
        }));
    }

    #[test]
    fn timeout_with_empty_patch_artifacts_and_actionable_false_stays_timed_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("patch.diff");
        let mounted_patch_path = artifact_root.join("mount-5.patch");
        fs::write(&patch_path, "").expect("patch diff");
        fs::write(&mounted_patch_path, "").expect("mounted patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        fs::write(
            artifact_root.join("agent-result.json"),
            serde_json::to_string(&json!({
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": "task-1",
                "status": "succeeded",
                "summary": "runtime produced no actionable patch",
                "actionable": false,
                "artifacts": [
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "patch",
                        "kind": "patch",
                        "name": "patch.diff",
                        "path": patch_path.display().to_string(),
                        "mime": "text/x-diff",
                        "metadata": { "role": "patch" }
                    },
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "mount-5",
                        "kind": "patch",
                        "name": "mount-5.patch",
                        "path": mounted_patch_path.display().to_string(),
                        "mime": "text/x-patch",
                        "metadata": { "role": "patch" }
                    }
                ],
                "evidence_refs": [],
                "diagnostics": [],
                "metadata": {}
            }))
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed
        );
        assert_eq!(aggregate.totals.succeeded, 0);
        assert_eq!(aggregate.totals.timed_out, 1);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::TimedOut));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "completed_runtime_late_provider_race"
                && diagnostic
                    .data
                    .get("actionable_patch")
                    .and_then(Value::as_bool)
                    == Some(false)
        }));
    }
}
