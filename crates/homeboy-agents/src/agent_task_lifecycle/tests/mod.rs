//! Tests for agent_task_lifecycle (extracted from mod.rs to keep mod.rs under structural thresholds).
#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactDeclaration, AgentTaskExecutionHandle, AgentTaskExecutor,
    AgentTaskLimits, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef,
    AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence, AgentTaskWorkflowStepStatus,
    AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA, AGENT_TASK_WORKFLOW_SCHEMA,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use homeboy_core::api_jobs::{Job, JobEvent, JobEventKind, JobStore, RemoteRunnerJobRequest};
use homeboy_core::test_support::with_isolated_home;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
pub(super) fn fake_controller_artifact(
    path: &std::path::Path,
    identity: &str,
    marker: &str,
) -> String {
    use std::os::unix::fs::PermissionsExt;

    let identity = serde_json::to_string(identity).expect("serialize fake controller identity");
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n# {marker}\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = self ] && [ \"$2\" = status ]; then\n  printf '%s\\n' '{{\"data\":{{\"active_build_identity\":{{\"display\":{identity}}}}}}}'\n  exit 0\nfi\nexit 1\n"
        ),
    )
    .expect("write fake controller artifact");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("make fake controller artifact executable");
    format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).expect("read fake controller artifact"))
    )
}

#[derive(Clone)]
pub(super) struct IntentReplayProvider {
    store: JobStore,
    submitted: Arc<Mutex<Vec<uuid::Uuid>>>,
    fail_after_accept_once: Arc<Mutex<bool>>,
}

impl RunnerContinuationProvider for IntentReplayProvider {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
        Err(Error::internal_unexpected(
            "not used by submission reconciliation",
        ))
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        true
    }
    fn runner_exists(&self, _runner_id: &str) -> bool {
        true
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        Err(Error::internal_unexpected(
            "not used by submission reconciliation",
        ))
    }

    fn submit_reverse_broker_job(
        &self,
        _runner_id: &str,
        request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        let job = self.store.submit_remote_runner_job(request)?;
        self.submitted.lock().expect("submission log").push(job.id);
        let mut fail = self.fail_after_accept_once.lock().expect("fault flag");
        if std::mem::take(&mut *fail) {
            return Err(Error::internal_unexpected(
                "injected post-accept pre-ack crash",
            ));
        }
        Ok(job)
    }
}

pub(super) fn outcome_with_refs(
    task_id: &str,
    artifacts: Vec<AgentTaskArtifact>,
    evidence_refs: Vec<AgentTaskEvidenceRef>,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
        summary: Some("ok".to_string()),
        failure_classification: None,
        artifacts,
        typed_artifacts: Vec::new(),
        evidence_refs,
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

pub(super) fn artifact_ref_artifact(
    id: &str,
    kind: &str,
    url: Option<&str>,
    path: Option<&str>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: Some(format!("{kind} artifact")),
        label: None,
        role: None,
        semantic_key: None,
        path: path.map(str::to_string),
        url: url.map(str::to_string),
        mime: None,
        size_bytes: None,
        sha256: None,
        metadata: Value::Null,
    }
}

pub(super) fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }],
    )
}

pub(super) fn terminal_child_snapshot(
    aggregate: &AgentTaskAggregate,
) -> homeboy_core::api_jobs::RunnerJobLogSnapshot {
    let job_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").expect("job id");
    homeboy_core::api_jobs::RunnerJobLogSnapshot {
        job: homeboy_core::api_jobs::Job {
            id: job_id,
            operation: "agent-task".to_string(),
            status: homeboy_core::api_jobs::JobStatus::Succeeded,
            created_at_ms: 1,
            updated_at_ms: 2,
            started_at_ms: Some(1),
            finished_at_ms: Some(2),
            event_count: 1,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        },
        events: vec![homeboy_core::api_jobs::JobEvent {
            sequence: 1,
            job_id,
            kind: homeboy_core::api_jobs::JobEventKind::Progress,
            timestamp_ms: 2,
            message: Some("agent-task lifecycle event".to_string()),
            data: Some(json!({
                "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                "identity": {
                    "runner_id": "homeboy-lab",
                    "runner_job_id": job_id.to_string(),
                    "persisted_run_id": "agent-task-disconnected-child",
                    "run_id": "agent-task-disconnected-child",
                },
                "aggregate": aggregate,
            })),
        }],
    }
}

pub(super) fn persisted_terminal_result_snapshot(
    aggregate: &AgentTaskAggregate,
) -> homeboy_core::api_jobs::RunnerJobLogSnapshot {
    let mut snapshot = terminal_child_snapshot(aggregate);
    snapshot.events[0].kind = JobEventKind::Result;
    snapshot.events[0].data = Some(json!({
        "exit_code": 0,
        "stdout": format!("HOMEBOY_RUNNER_PROGRESS {{\"phase\":\"finished\"}}\n{}", json!({
            "schema": "homeboy/command-result/v3",
            "command": "agent-task",
            "success": true,
            "exit_code": 0,
            "data": {
                "schema": "homeboy/agent-task-dispatch/v1",
                "aggregate": aggregate,
            },
        }))
    }));
    snapshot
}

pub(super) fn succeeded_aggregate(plan: &AgentTaskPlan) -> AgentTaskAggregate {
    AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Succeeded,
        totals: AgentTaskAggregateTotals {
            queued: 1,
            succeeded: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }],
        events: vec![AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Succeeded,
            attempt: 1,
            message: Some("ok".to_string()),
        }],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    }
}

mod handoff_and_proxy;
mod status_and_recovery;
mod submit_and_persist;
mod terminal_and_reconcile;
