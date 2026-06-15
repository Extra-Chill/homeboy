use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutionHandle, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkflowEvidence,
    AgentTaskWorkspace, AgentTaskWorkspaceMode, AGENT_TASK_OUTCOME_SCHEMA,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_provider::{role_aliases_for_provider, AgentTaskProviderRoleAliases};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
    AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
};
use crate::core::{paths, Error, ErrorCode, Result};

#[path = "lifecycle_store.rs"]
mod lifecycle_store;

use lifecycle_store as store;

mod schemas {
    pub(super) const RUN: &str = "homeboy/agent-task-run/v1";
    pub(super) const RUN_LOG: &str = "homeboy/agent-task-run-log/v1";
    pub(super) const RUN_ARTIFACTS: &str = "homeboy/agent-task-run-artifacts/v1";
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunRecord {
    pub schema: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub plan_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<crate::core::agent_task_scheduler::AgentTaskAggregateTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskRunTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_handles: Vec<AgentTaskRunProviderHandle>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentTaskRunRecord {
    fn record_runner_metadata(&mut self, reclaimed_stale: bool) {
        let metadata = self.ensure_metadata_object();
        metadata.insert("runner_pid".to_string(), json!(std::process::id()));
        metadata.insert("runner_started_at".to_string(), json!(now_timestamp()));
        if reclaimed_stale {
            metadata.insert("reclaimed_stale_running".to_string(), json!(true));
        } else {
            metadata.remove("reclaimed_stale_running");
        }
        metadata.remove("stale_running");
        metadata.remove("stale_running_reason");
    }

    fn annotate_stale_running(&mut self) {
        if self.state != AgentTaskRunState::Running || self.owner_process_is_running() {
            return;
        }

        let reason = if self.owner_pid().is_some() {
            "owner_process_not_running"
        } else {
            "missing_runner_pid"
        };
        let metadata = self.ensure_metadata_object();
        metadata.insert("stale_running".to_string(), json!(true));
        metadata.insert("stale_running_reason".to_string(), json!(reason));
    }

    fn owner_process_is_running(&self) -> bool {
        self.owner_pid()
            .is_some_and(crate::core::process::pid_is_running)
    }

    fn owner_pid(&self) -> Option<u32> {
        self.metadata
            .get("runner_pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    }

    fn ensure_metadata_object(&mut self) -> &mut serde_json::Map<String, Value> {
        if !self.metadata.is_object() {
            self.metadata = json!({});
        }
        self.metadata.as_object_mut().expect("metadata object")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRunState {
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRunTask {
    pub task_id: String,
    pub state: AgentTaskState,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactRef {
    pub task_id: String,
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunProviderHandle {
    pub task_id: String,
    pub backend: String,
    pub provider_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentTaskState>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunLog {
    pub schema: String,
    pub run_id: String,
    pub events: Vec<AgentTaskProgressEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunArtifacts {
    pub schema: String,
    pub run_id: String,
    pub artifacts: Vec<AgentTaskArtifact>,
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
}

pub fn submit_plan(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let run_id = requested_run_id
        .map(sanitize_run_id)
        .unwrap_or_else(default_run_id);
    let plan_path = store::write_plan(&run_id, plan)?;

    let record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: plan_path.display().to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        metadata: json!({
            "task_count": plan.tasks.len(),
            "max_concurrency": plan.options.max_concurrency,
            "provider_run_ids": [],
            "note": "submitted tasks are durable; provider run ids are recorded after an executor returns them as generic artifacts or evidence refs"
        }),
    };
    store::write_record(&record)?;
    Ok(record)
}

pub fn record_completed_run(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let mut record = submit_plan(plan, requested_run_id)?;
    record_aggregate(&mut record, plan, aggregate)
}

pub fn load_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
    store::read_plan_path(&record.plan_path)
}

pub fn mark_running(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if record.state == AgentTaskRunState::Running && record.owner_process_is_running() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already running under pid {}",
                record.run_id,
                record.owner_pid().unwrap_or_default()
            ),
            Some(record.run_id),
            None,
        ));
    }
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let reclaimed_stale = record.state == AgentTaskRunState::Running;
    record.state = AgentTaskRunState::Running;
    record.updated_at = Some(now_timestamp());
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    record.record_runner_metadata(reclaimed_stale);
    store::write_record(&record)?;
    Ok(record)
}

pub fn claim_next_queued_run() -> Result<Option<AgentTaskRunRecord>> {
    let mut queued: Vec<AgentTaskRunRecord> = store::read_records()?
        .into_iter()
        .filter(|record| record.state == AgentTaskRunState::Queued)
        .collect();
    queued.sort_by(|left, right| {
        left.submitted_at
            .cmp(&right.submitted_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    for record in queued {
        match mark_running(&record.run_id) {
            Ok(claimed) => return Ok(Some(claimed)),
            Err(error) if error.code == ErrorCode::ValidationInvalidArgument => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(None)
}

pub fn cancel_run(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if record.state == AgentTaskRunState::Cancelled {
        return Ok(record);
    }

    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    if record.state == AgentTaskRunState::Running && record.owner_process_is_running() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is running under pid {}; provider live cancellation is not available through this durable record yet",
                record.run_id,
                record.owner_pid().unwrap_or_default()
            ),
            Some(record.run_id),
            None,
        ));
    }

    let cancelled_at = now_timestamp();
    let was_stale_running = record.state == AgentTaskRunState::Running;
    record.state = AgentTaskRunState::Cancelled;
    record.updated_at = Some(cancelled_at.clone());
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Cancelled;
        }
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("cancelled_at".to_string(), json!(cancelled_at));
    metadata.insert("cancelled_by_pid".to_string(), json!(std::process::id()));
    metadata.insert(
        "cancel_reason".to_string(),
        json!(reason.unwrap_or("cancel requested")),
    );
    if was_stale_running {
        metadata.insert("cancelled_stale_running".to_string(), json!(true));
    }
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");

    store::write_record(&record)?;
    Ok(record)
}

pub fn record_run_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    record_aggregate(&mut record, plan, aggregate)
}

#[derive(Debug, Clone)]
pub struct AgentTaskPreDispatchFailure<'a> {
    pub run_id: &'a str,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub runner_id: &'a str,
    pub remote_workspace: &'a str,
    pub failure_message: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_pre_dispatch_failure(
    failure: AgentTaskPreDispatchFailure<'_>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(failure.run_id);
    if let Ok(record) = status(&run_id) {
        return Ok(record);
    }

    let task_id = "agent-task-predispatch".to_string();
    let metadata = json!({
        "kind": "lab_offload_pre_dispatch_failure",
        "runner_id": failure.runner_id,
        "remote_workspace": failure.remote_workspace,
        "local_command": failure.local_command,
        "remote_command": failure.remote_command,
        "exit_code": failure.exit_code,
        "failure_message": failure.failure_message,
    });
    let plan = AgentTaskPlan::new(
        format!("{run_id}.predispatch"),
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.clone(),
            group_key: Some("lab-offload".to_string()),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "homeboy-lab".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Persist Lab offload pre-dispatch validation failure evidence."
                .to_string(),
            inputs: json!({
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "runner_id": failure.runner_id,
                "remote_workspace": failure.remote_workspace,
                "failure": {
                    "message": failure.failure_message,
                    "exit_code": failure.exit_code,
                    "stdout": failure.stdout,
                    "stderr": failure.stderr,
                }
            }),
            source_refs: vec![AgentTaskSourceRef {
                kind: "lab-offload-run".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/lab-offload"),
                revision: None,
            }],
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Existing,
                root: Some(failure.remote_workspace.to_string()),
                slug: None,
                kind: Some("lab-offload".to_string()),
                component_id: None,
                branch: None,
                base_ref: None,
                task_url: None,
                cleanup: Some("preserve".to_string()),
                materialization: metadata.clone(),
            },
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: metadata.clone(),
        }],
    );
    submit_plan(&plan, Some(&run_id))?;
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            failed: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(failure.failure_message.to_string()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "lab-offload-pre-dispatch-failure".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/logs"),
                label: Some("Lab offload pre-dispatch failure".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: json!({
                "schema": "homeboy/agent-task-predispatch-failure/v1",
                "runner_id": failure.runner_id,
                "remote_workspace": failure.remote_workspace,
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "exit_code": failure.exit_code,
                "stdout": failure.stdout,
                "stderr": failure.stderr,
            }),
            workflow: None,
            follow_up: None,
            metadata,
        }],
        events: vec![
            AgentTaskProgressEvent {
                task_id: task_id.clone(),
                state: AgentTaskState::Queued,
                attempt: 1,
                message: Some("Lab offload selected and remote command prepared".to_string()),
            },
            AgentTaskProgressEvent {
                task_id,
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some(failure.failure_message.to_string()),
            },
        ],
        artifact_lineage: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: 1,
            completed: 1,
            ..AgentTaskQueueStatus::default()
        },
    };
    record_run_aggregate(&run_id, &plan, &aggregate)
}

#[derive(Debug, Clone)]
pub struct AgentTaskRemoteDispatchFailure<'a> {
    pub run_id: &'a str,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub runner_id: &'a str,
    pub remote_workspace: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_remote_dispatch_failure(
    failure: AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
) -> Result<Option<AgentTaskRunRecord>> {
    if envelope.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-dispatch/v1") {
        return Ok(None);
    }

    let Some(aggregate_value) = envelope.get("aggregate") else {
        return Ok(None);
    };

    let run_id = sanitize_run_id(failure.run_id);
    let mut aggregate: AgentTaskAggregate = serde_json::from_value(aggregate_value.clone())
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task dispatch aggregate".to_string()),
            )
        })?;
    enrich_remote_dispatch_aggregate(envelope, &mut aggregate);
    if aggregate.events.is_empty() {
        aggregate.events = events_for_outcomes(&aggregate.outcomes);
    }

    let (mut record, remote_run_id, remote_plan_path, remote_aggregate_path) =
        if let Some(record_value) = envelope.get("record") {
            let mut record: AgentTaskRunRecord = serde_json::from_value(record_value.clone())
                .map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("parse offloaded agent-task dispatch record".to_string()),
                    )
                })?;
            let remote_run_id = record.run_id.clone();
            let remote_plan_path = record.plan_path.clone();
            let remote_aggregate_path = record.aggregate_path.clone();
            let plan = store::read_plan_path(&record.plan_path).unwrap_or_else(|_| {
                synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate)
            });
            record.run_id = run_id.clone();
            apply_aggregate_to_record(
                &mut record,
                &plan,
                &aggregate,
                store::write_aggregate(&run_id, &aggregate)?
                    .display()
                    .to_string(),
            );
            (
                record,
                remote_run_id,
                remote_plan_path,
                remote_aggregate_path,
            )
        } else {
            let remote_run_id = envelope
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or(failure.run_id)
                .to_string();
            let remote_plan_path = envelope
                .get("plan_path")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    envelope
                        .get("plan_id")
                        .and_then(Value::as_str)
                        .unwrap_or(&aggregate.plan_id)
                        .to_string()
                });
            let remote_aggregate_path = envelope
                .get("aggregate_path")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let plan = synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate);
            let mut record = submit_plan(&plan, Some(&run_id))?;
            record_aggregate(&mut record, &plan, &aggregate)?;
            (
                record,
                remote_run_id,
                remote_plan_path,
                remote_aggregate_path,
            )
        };

    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "kind".to_string(),
        json!("lab_offload_remote_dispatch_failure"),
    );
    metadata.insert("runner_id".to_string(), json!(failure.runner_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(failure.remote_workspace),
    );
    metadata.insert("local_command".to_string(), json!(failure.local_command));
    metadata.insert("remote_command".to_string(), json!(failure.remote_command));
    metadata.insert("exit_code".to_string(), json!(failure.exit_code));
    metadata.insert("stdout".to_string(), json!(failure.stdout));
    metadata.insert("stderr".to_string(), json!(failure.stderr));
    metadata.insert("remote_run_id".to_string(), json!(remote_run_id));
    metadata.insert("remote_plan_path".to_string(), json!(remote_plan_path));
    metadata.insert(
        "remote_aggregate_path".to_string(),
        json!(remote_aggregate_path),
    );
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));

    store::write_record(&record)?;
    Ok(Some(record))
}

fn enrich_remote_dispatch_aggregate(envelope: &Value, aggregate: &mut AgentTaskAggregate) {
    let remote_run_id = envelope.get("run_id").and_then(Value::as_str);
    for outcome in &mut aggregate.outcomes {
        normalize_provider_run_result(outcome);

        if outcome.evidence_refs.is_empty() {
            if let Some(remote_run_id) = remote_run_id {
                outcome.evidence_refs.extend([
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-logs".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/logs"),
                        label: Some("Remote agent-task logs".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-review".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/review"),
                        label: Some("Remote agent-task review".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-artifacts".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/artifacts"),
                        label: Some("Remote agent-task artifacts".to_string()),
                    },
                ]);
            }
        }
    }
}

fn synthetic_remote_dispatch_plan(
    run_id: &str,
    failure: &AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
    aggregate: &AgentTaskAggregate,
) -> AgentTaskPlan {
    let tasks = aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            let provider = outcome
                .metadata
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("homeboy-lab");
            AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: outcome.task_id.clone(),
                group_key: Some("lab-offload".to_string()),
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: provider.to_string(),
                    selector: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: outcome.summary.clone().unwrap_or_else(|| {
                    "Persist remote Lab agent-task dispatch outcome.".to_string()
                }),
                inputs: json!({
                    "remote_dispatch_envelope": envelope,
                    "remote_command": failure.remote_command,
                }),
                source_refs: vec![AgentTaskSourceRef {
                    kind: "lab-offload-remote-dispatch".to_string(),
                    uri: envelope
                        .get("run_id")
                        .and_then(Value::as_str)
                        .map(|remote_run_id| format!("homeboy://agent-task/run/{remote_run_id}"))
                        .unwrap_or_else(|| {
                            format!("homeboy://agent-task/run/{run_id}/lab-offload")
                        }),
                    revision: envelope
                        .get("plan_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                }],
                workspace: AgentTaskWorkspace {
                    mode: AgentTaskWorkspaceMode::Existing,
                    root: Some(failure.remote_workspace.to_string()),
                    slug: None,
                    kind: Some("lab-offload".to_string()),
                    component_id: None,
                    branch: None,
                    base_ref: None,
                    task_url: None,
                    cleanup: Some("preserve".to_string()),
                    materialization: json!({
                        "runner_id": failure.runner_id,
                        "remote_workspace": failure.remote_workspace,
                    }),
                },
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: outcome.metadata.clone(),
            }
        })
        .collect();

    let mut plan = AgentTaskPlan::new(
        envelope
            .get("plan_id")
            .and_then(Value::as_str)
            .unwrap_or(&aggregate.plan_id),
        tasks,
    );
    plan.group_key = Some("lab-offload".to_string());
    plan.metadata = json!({
        "kind": "lab_offload_remote_dispatch_failure",
        "runner_id": failure.runner_id,
        "remote_workspace": failure.remote_workspace,
        "remote_run_id": envelope.get("run_id").and_then(Value::as_str),
    });
    plan
}

fn record_aggregate(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = store::write_aggregate(&record.run_id, aggregate)?;
    apply_aggregate_to_record(
        record,
        plan,
        aggregate,
        aggregate_path.display().to_string(),
    );
    store::write_record(record)?;
    Ok(record.clone())
}

fn apply_aggregate_to_record(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    aggregate_path: String,
) {
    record.state = run_state_for_aggregate(aggregate);
    record.updated_at = Some(now_timestamp());
    record.aggregate_path = Some(aggregate_path);
    record.totals = Some(aggregate.totals.clone());
    record.tasks = tasks_for_aggregate(plan, aggregate);
    record.artifact_refs = artifact_refs_for_outcomes(&aggregate.outcomes);
    record.provider_handles = provider_handles_for_outcomes(&aggregate.outcomes);
    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let metadata = record.ensure_metadata_object();
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if let (Ok(aggregate), Ok(plan)) = (
        store::read_aggregate(&record.run_id),
        store::read_plan_path(&record.plan_path),
    ) {
        let aggregate_path = store::aggregate_path(&record.run_id)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "aggregate.json".to_string());
        let mut reconciled = record.clone();
        apply_aggregate_to_record(&mut reconciled, &plan, &aggregate, aggregate_path);

        if reconciled != record {
            if let Err(error) = store::write_record(&reconciled) {
                reconciled
                    .ensure_metadata_object()
                    .insert("finalization_error".to_string(), json!(error.message));
            }

            record = reconciled;
        }
    }
    record.annotate_stale_running();
    Ok(record)
}

pub fn list_records() -> Result<Vec<AgentTaskRunRecord>> {
    let mut records = Vec::new();
    for record in store::read_records()? {
        records.push(status(&record.run_id)?);
    }
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok(records)
}

pub fn run_record_exists(run_id: &str) -> Result<bool> {
    store::record_exists(run_id)
}

pub fn cancel(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    record.state = AgentTaskRunState::Cancelled;
    record.updated_at = Some(now_timestamp());
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Cancelled;
        }
    }
    for handle in &mut record.provider_handles {
        if !matches!(
            handle.state,
            Some(AgentTaskState::Succeeded | AgentTaskState::Failed | AgentTaskState::Cancelled)
        ) {
            handle.state = Some(AgentTaskState::Cancelled);
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("cancel_requested_at".to_string(), json!(now_timestamp()));
    metadata.insert(
        "cancel_note".to_string(),
        json!("provider-specific cancellation is delegated through opaque provider handles"),
    );
    store::write_record(&record)?;
    Ok(record)
}

pub fn mark_resuming(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("resume_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&record)?;
    mark_running(run_id)
}

pub fn retry(run_id: &str, requested_run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let source = store::read_record(&sanitize_run_id(run_id))?;
    let plan = store::read_plan_path(&source.plan_path)?;
    let mut retry = submit_plan(&plan, requested_run_id)?;
    let metadata = retry.ensure_metadata_object();
    metadata.insert("retry_of".to_string(), json!(source.run_id));
    metadata.insert("retry_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&retry)?;
    Ok(retry)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    let run_id = sanitize_run_id(run_id);
    let record = store::read_record(&run_id)?;
    let events = store::read_aggregate(&run_id)
        .map(|aggregate| aggregate.events)
        .unwrap_or_else(|_| queued_events(&record.tasks));
    Ok(AgentTaskRunLog {
        schema: schemas::RUN_LOG.to_string(),
        run_id,
        events,
    })
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    let run_id = sanitize_run_id(run_id);
    store::read_record(&run_id)?;
    let aggregate = store::read_aggregate(&run_id).ok();
    Ok(AgentTaskRunArtifacts {
        schema: schemas::RUN_ARTIFACTS.to_string(),
        run_id,
        artifacts: aggregate_artifacts(aggregate.as_ref()),
        evidence_refs: aggregate_evidence_refs(aggregate.as_ref()),
    })
}

pub fn aggregate_source(run_id: &str) -> Result<(String, PathBuf)> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
    let path = record.aggregate_path.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' has no aggregate artifact yet",
                record.run_id
            ),
            Some(record.run_id),
            None,
        )
    })?;
    let path = PathBuf::from(path);
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok((raw, path))
}

fn aggregate_artifacts(aggregate: Option<&AgentTaskAggregate>) -> Vec<AgentTaskArtifact> {
    aggregate
        .map(|aggregate| {
            aggregate
                .outcomes
                .iter()
                .flat_map(|outcome| outcome.artifacts.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn aggregate_evidence_refs(aggregate: Option<&AgentTaskAggregate>) -> Vec<AgentTaskEvidenceRef> {
    aggregate
        .map(|aggregate| {
            aggregate
                .outcomes
                .iter()
                .flat_map(evidence_refs_for_outcome)
                .collect()
        })
        .unwrap_or_default()
}

fn evidence_refs_for_outcome(outcome: &AgentTaskOutcome) -> Vec<AgentTaskEvidenceRef> {
    outcome
        .evidence_refs
        .iter()
        .cloned()
        .chain(workflow_evidence_refs(outcome.workflow.as_ref()))
        .collect()
}

fn workflow_evidence_refs(
    workflow: Option<&AgentTaskWorkflowEvidence>,
) -> impl Iterator<Item = AgentTaskEvidenceRef> + '_ {
    workflow.into_iter().flat_map(|workflow| {
        workflow
            .steps
            .iter()
            .flat_map(|step| step.artifact_refs.iter().cloned())
    })
}

fn queued_task(request: &crate::core::agent_task::AgentTaskRequest) -> AgentTaskRunTask {
    AgentTaskRunTask {
        task_id: request.task_id.clone(),
        state: AgentTaskState::Queued,
        backend: request.executor.backend.clone(),
        selector: request.executor.selector.clone(),
        model: request.executor.model.clone(),
        provider_ref: request
            .executor
            .selector
            .as_ref()
            .map(|selector| format!("{}:{selector}", request.executor.backend)),
    }
}

fn tasks_for_aggregate(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Vec<AgentTaskRunTask> {
    plan.tasks
        .iter()
        .map(|request| {
            let mut task = queued_task(request);
            if let Some(event) = aggregate
                .events
                .iter()
                .rev()
                .find(|event| event.task_id == request.task_id)
            {
                task.state = event.state;
            } else if let Some(outcome) = aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == request.task_id)
            {
                task.state = task_state_for_outcome_status(outcome.status);
            }
            task
        })
        .collect()
}

fn task_state_for_outcome_status(status: AgentTaskOutcomeStatus) -> AgentTaskState {
    match status {
        AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
            AgentTaskState::Succeeded
        }
        AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
        AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
        _ => AgentTaskState::Failed,
    }
}

fn events_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskProgressEvent> {
    outcomes
        .iter()
        .map(|outcome| AgentTaskProgressEvent {
            task_id: outcome.task_id.clone(),
            state: task_state_for_outcome_status(outcome.status),
            attempt: 1,
            message: outcome.summary.clone(),
        })
        .collect()
}

fn queued_events(tasks: &[AgentTaskRunTask]) -> Vec<AgentTaskProgressEvent> {
    tasks
        .iter()
        .map(|task| AgentTaskProgressEvent {
            task_id: task.task_id.clone(),
            state: task.state,
            attempt: 1,
            message: Some("task submitted".to_string()),
        })
        .collect()
}

fn artifact_refs_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskArtifactRef> {
    outcomes
        .iter()
        .flat_map(|outcome| {
            let artifact_refs = outcome.artifacts.iter().filter_map(|artifact| {
                artifact
                    .url
                    .clone()
                    .or_else(|| artifact.path.clone())
                    .map(|uri| AgentTaskArtifactRef {
                        task_id: outcome.task_id.clone(),
                        kind: artifact.kind.clone(),
                        uri,
                        label: artifact.name.clone(),
                    })
            });
            let evidence_refs = outcome
                .evidence_refs
                .iter()
                .cloned()
                .chain(workflow_evidence_refs(outcome.workflow.as_ref()))
                .map(|evidence| AgentTaskArtifactRef {
                    task_id: outcome.task_id.clone(),
                    kind: evidence.kind.clone(),
                    uri: evidence.uri.clone(),
                    label: evidence.label.clone(),
                });
            artifact_refs.chain(evidence_refs).collect::<Vec<_>>()
        })
        .collect()
}

fn provider_handles_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskRunProviderHandle> {
    outcomes
        .iter()
        .flat_map(|outcome| provider_handles_for_outcome(outcome))
        .collect()
}

fn provider_handles_for_outcome(outcome: &AgentTaskOutcome) -> Vec<AgentTaskRunProviderHandle> {
    let mut handles = Vec::new();
    if let Some(handle) = outcome
        .metadata
        .get("provider_handle")
        .and_then(provider_handle_from_value)
    {
        handles.push(run_provider_handle(outcome, handle));
    }
    if let Some(values) = outcome
        .metadata
        .get("provider_handles")
        .and_then(Value::as_array)
    {
        handles.extend(
            values
                .iter()
                .filter_map(provider_handle_from_value)
                .map(|handle| run_provider_handle(outcome, handle)),
        );
    }
    if handles.is_empty() {
        if let Some(handle) = provider_handle_from_outcome_metadata(outcome) {
            handles.push(handle);
        }
    }
    handles
}

fn provider_handle_from_outcome_metadata(
    outcome: &AgentTaskOutcome,
) -> Option<AgentTaskRunProviderHandle> {
    let provider = outcome.metadata.get("provider").and_then(Value::as_str)?;
    let role_aliases = role_aliases_for_provider(provider);
    let provider_run_id = outcome
        .metadata
        .get("remote_run_id")
        .or_else(|| outcome.metadata.get("provider_run_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            provider_run_result(outcome, &role_aliases)
                .and_then(|result| result.get("run_id").or_else(|| result.get("id")))
                .and_then(Value::as_str)
        })?;

    Some(AgentTaskRunProviderHandle {
        task_id: outcome.task_id.clone(),
        backend: provider.to_string(),
        provider_run_id: provider_run_id.to_string(),
        stream_uri: outcome
            .metadata
            .get("stream_uri")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        state: Some(task_state_for_outcome_status(outcome.status)),
        metadata: outcome.metadata.clone(),
    })
}

fn normalize_provider_run_result(outcome: &mut AgentTaskOutcome) {
    if outcome.outputs.get("provider_run_result").is_some() {
        return;
    }
    let role_aliases = outcome
        .metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(role_aliases_for_provider)
        .unwrap_or_default();
    if let Some(result) = provider_run_result(outcome, &role_aliases).cloned() {
        let mut outputs = outcome.outputs.as_object().cloned().unwrap_or_default();
        outputs.insert("provider_run_result".to_string(), result);
        outcome.outputs = Value::Object(outputs);
    }
}

fn provider_run_result<'a>(
    outcome: &'a AgentTaskOutcome,
    role_aliases: &AgentTaskProviderRoleAliases,
) -> Option<&'a Value> {
    outcome
        .outputs
        .get("provider_run_result")
        .or_else(|| {
            role_aliases
                .output_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| outcome.outputs.get(alias))
        })
        .or_else(|| {
            role_aliases
                .metadata_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| outcome.metadata.get(alias))
        })
}

fn provider_handle_from_value(value: &Value) -> Option<AgentTaskExecutionHandle> {
    serde_json::from_value(value.clone()).ok()
}

fn run_provider_handle(
    outcome: &AgentTaskOutcome,
    handle: AgentTaskExecutionHandle,
) -> AgentTaskRunProviderHandle {
    AgentTaskRunProviderHandle {
        task_id: handle.task_id,
        backend: handle.backend,
        provider_run_id: handle.run_id,
        stream_uri: handle.stream_uri,
        state: Some(match outcome.status {
            crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded
            | crate::core::agent_task::AgentTaskOutcomeStatus::NoOp => AgentTaskState::Succeeded,
            crate::core::agent_task::AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
            crate::core::agent_task::AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
            _ => AgentTaskState::Failed,
        }),
        metadata: handle.metadata,
    }
}

fn run_state_for_aggregate(aggregate: &AgentTaskAggregate) -> AgentTaskRunState {
    match aggregate.status {
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded => {
            AgentTaskRunState::Succeeded
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure => {
            AgentTaskRunState::PartialFailure
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed => {
            AgentTaskRunState::Failed
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Cancelled => {
            AgentTaskRunState::Cancelled
        }
    }
}

fn default_run_id() -> String {
    format!("agent-task-{}", Uuid::new_v4())
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn sanitize_run_id(run_id: &str) -> String {
    let sanitized = paths::sanitize_path_segment(run_id);
    if sanitized.is_empty() {
        default_run_id()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutionHandle, AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy,
        AgentTaskRequest, AgentTaskWorkflowEvidence, AgentTaskWorkflowStepEvidence,
        AgentTaskWorkflowStepStatus, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
        AGENT_TASK_WORKFLOW_SCHEMA,
    };
    use crate::core::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
        AGENT_TASK_AGGREGATE_SCHEMA,
    };
    use crate::test_support::with_isolated_home;

    #[test]
    fn provider_run_result_reads_declared_output_alias() {
        let role_aliases: AgentTaskProviderRoleAliases = serde_json::from_value(json!({
            "outputs": {
                "provider_run_result": ["custom_run_result"]
            }
        }))
        .expect("role aliases");
        let outcome = AgentTaskOutcome {
            schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
            summary: None,
            failure_classification: None,
            artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({
                "custom_run_result": {
                    "run_id": "custom-run-1"
                }
            }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        assert_eq!(
            provider_run_result(&outcome, &role_aliases)
                .and_then(|result| result.get("run_id"))
                .and_then(Value::as_str),
            Some("custom-run-1")
        );
    }

    #[test]
    fn submit_plan_persists_queued_status() {
        with_isolated_home(|_| {
            let plan = test_plan();

            let record = submit_plan(&plan, Some("run/a")).expect("submitted");
            let loaded = status(&record.run_id).expect("status loaded");

            assert_eq!(record.run_id, "run_a");
            assert_eq!(loaded.state, AgentTaskRunState::Queued);
            assert_eq!(loaded.tasks[0].task_id, "task-a");
            assert_eq!(
                loaded.tasks[0].provider_ref.as_deref(),
                Some("test:fixture")
            );
        });
    }

    #[test]
    fn pre_dispatch_failure_persists_failed_run_without_provider_handle() {
        with_isolated_home(|_| {
            let record = record_pre_dispatch_failure(AgentTaskPreDispatchFailure {
                run_id: "cook-lab-predispatch",
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-lab-predispatch".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--cwd".to_string(),
                    "/runner/workspace/repo".to_string(),
                ],
                runner_id: "lab-a",
                remote_workspace: "/runner/workspace/repo",
                failure_message: "Invalid argument 'cwd': agent-task Codebox dispatch requires --cwd to be a git checkout",
                stdout: "",
                stderr: "Invalid argument 'cwd': agent-task Codebox dispatch requires --cwd to be a git checkout\n",
                exit_code: 1,
            })
            .expect("pre-dispatch failure recorded");

            let loaded = status("cook-lab-predispatch").expect("status loaded");
            let log = logs("cook-lab-predispatch").expect("logs loaded");

            assert_eq!(record.state, AgentTaskRunState::Failed);
            assert_eq!(loaded.state, AgentTaskRunState::Failed);
            assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
            assert!(loaded.provider_handles.is_empty());
            assert_eq!(log.events[1].state, AgentTaskState::Failed);
            assert_eq!(loaded.metadata["provider_run_ids"], serde_json::json!([]));
            assert_eq!(
                loaded.artifact_refs[0].kind,
                "lab-offload-pre-dispatch-failure"
            );
        });
    }

    #[test]
    fn remote_dispatch_failure_preserves_structured_outcome_details() {
        with_isolated_home(|_| {
            let plan = test_plan();
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Failed,
                totals: AgentTaskAggregateTotals {
                    failed: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                    summary: Some("Remote provider agent task failed.".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::Provider),
                    artifacts: Vec::new(),
                    evidence_refs: vec![AgentTaskEvidenceRef {
                        kind: "logs".to_string(),
                        uri: "homeboy://agent-task/run/remote-run/logs".to_string(),
                        label: Some("remote provider logs".to_string()),
                    }],
                    diagnostics: Vec::new(),
                    outputs: serde_json::json!({
                        "provider_run_result": {
                            "status": "failed",
                            "failure_classification": "runtime",
                            "artifacts": [],
                            "refs": { "logs": [], "transcripts": [], "runtimes": [] }
                        }
                    }),
                    workflow: None,
                    follow_up: None,
                    metadata: serde_json::json!({
                        "provider": "fixture.agent-task-executor",
                        "remote_run_id": "provider-run-1",
                        "remote_workspace": "/runner/workspace/repo"
                    }),
                }],
                events: vec![AgentTaskProgressEvent {
                    task_id: "task-a".to_string(),
                    state: AgentTaskState::Failed,
                    attempt: 1,
                    message: Some("Remote provider agent task failed.".to_string()),
                }],
                artifact_lineage: Vec::new(),
                queue: AgentTaskQueueStatus {
                    max_concurrency: 1,
                    completed: 1,
                    ..AgentTaskQueueStatus::default()
                },
            };
            let remote_record =
                record_completed_run(&plan, &aggregate, Some("remote-run")).expect("remote record");
            let envelope = serde_json::json!({
                "schema": "homeboy/agent-task-dispatch/v1",
                "run_id": "remote-run",
                "plan_id": plan.plan_id,
                "state": "failed",
                "record": remote_record,
                "aggregate": aggregate,
            });

            let record = record_remote_dispatch_failure(
                AgentTaskRemoteDispatchFailure {
                    run_id: "local-run",
                    local_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    remote_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    runner_id: "lab-a",
                    remote_workspace: "/runner/workspace/repo",
                    stdout: &envelope.to_string(),
                    stderr: "",
                    exit_code: 1,
                },
                &envelope,
            )
            .expect("remote dispatch failure recorded")
            .expect("dispatch envelope recognized");

            let loaded = status("local-run").expect("status loaded");
            let log = logs("local-run").expect("logs loaded");
            let artifacts = artifacts("local-run").expect("artifacts loaded");
            let (raw_aggregate, _) = aggregate_source("local-run").expect("aggregate source");

            assert_eq!(record.run_id, "local-run");
            assert_eq!(loaded.state, AgentTaskRunState::Failed);
            assert_eq!(loaded.tasks[0].task_id, "task-a");
            assert_ne!(loaded.tasks[0].task_id, "agent-task-predispatch");
            assert_eq!(
                loaded.metadata["kind"],
                "lab_offload_remote_dispatch_failure"
            );
            assert_eq!(loaded.metadata["runner_id"], "lab-a");
            assert_eq!(
                loaded.metadata["remote_workspace"],
                "/runner/workspace/repo"
            );
            assert_eq!(
                log.events[0].message.as_deref(),
                Some("Remote provider agent task failed.")
            );
            assert_eq!(artifacts.evidence_refs[0].kind, "logs");
            assert!(raw_aggregate.contains("fixture.agent-task-executor"));
            assert!(raw_aggregate.contains("failure_classification"));
        });
    }

    #[test]
    fn aggregate_only_remote_dispatch_failure_preserves_lab_outcome_details() {
        with_isolated_home(|_| {
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: "remote-plan".to_string(),
                status: AgentTaskAggregateStatus::Failed,
                totals: AgentTaskAggregateTotals {
                    failed: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "cook-conductor".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                    summary: Some("Remote provider agent task failed.".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::Provider),
                    artifacts: Vec::new(),
                    evidence_refs: vec![AgentTaskEvidenceRef {
                        kind: "provider-run".to_string(),
                        uri: "homeboy://provider/runs/provider-run-1".to_string(),
                        label: Some("Provider run".to_string()),
                    }],
                    diagnostics: Vec::new(),
                    outputs: serde_json::json!({
                        "provider_run_result": {
                            "schema": "custom-provider/agent-task-run-result/v1",
                            "run_id": "provider-run-1",
                            "status": "failed",
                            "failure_classification": "runtime",
                            "metadata": {
                                "remote_plan_ref": "remote-plan",
                                "remote_run_ref": "remote-run"
                            }
                        }
                    }),
                    workflow: None,
                    follow_up: None,
                    metadata: serde_json::json!({
                        "provider": "fixture.agent-task-executor",
                        "remote_run_id": "provider-run-1",
                    }),
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                queue: AgentTaskQueueStatus {
                    max_concurrency: 1,
                    completed: 1,
                    ..AgentTaskQueueStatus::default()
                },
            };
            let envelope = serde_json::json!({
                "schema": "homeboy/agent-task-dispatch/v1",
                "run_id": "remote-run",
                "plan_id": "remote-plan",
                "state": "failed",
                "aggregate": aggregate,
            });

            let record = record_remote_dispatch_failure(
                AgentTaskRemoteDispatchFailure {
                    run_id: "conductor-full-loop-proof-retry2-20260611",
                    local_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    remote_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    runner_id: "lab-a",
                    remote_workspace: "/runner/workspace/conductor",
                    stdout: &envelope.to_string(),
                    stderr: "",
                    exit_code: 1,
                },
                &envelope,
            )
            .expect("aggregate-only dispatch failure recorded")
            .expect("dispatch envelope recognized");

            let loaded =
                status("conductor-full-loop-proof-retry2-20260611").expect("status loaded");
            let log = logs("conductor-full-loop-proof-retry2-20260611").expect("logs loaded");
            let artifacts =
                artifacts("conductor-full-loop-proof-retry2-20260611").expect("artifacts loaded");
            let (raw_aggregate, _) = aggregate_source("conductor-full-loop-proof-retry2-20260611")
                .expect("aggregate source");

            assert_eq!(record.run_id, "conductor-full-loop-proof-retry2-20260611");
            assert_eq!(loaded.state, AgentTaskRunState::Failed);
            assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
            assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
            assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
            assert_eq!(loaded.provider_handles.len(), 1);
            assert_eq!(loaded.provider_handles[0].provider_run_id, "provider-run-1");
            assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
            assert_eq!(loaded.metadata["remote_plan_path"], "remote-plan");
            assert_eq!(
                log.events[0].message.as_deref(),
                Some("Remote provider agent task failed.")
            );
            assert_eq!(artifacts.evidence_refs[0].kind, "provider-run");
            assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
            assert!(raw_aggregate.contains("failure_classification"));
            assert!(raw_aggregate.contains("remote_plan_ref"));
        });
    }

    #[test]
    fn sparse_aggregate_only_remote_dispatch_failure_adds_remote_evidence_refs() {
        with_isolated_home(|_| {
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: "remote-plan".to_string(),
                status: AgentTaskAggregateStatus::Failed,
                totals: AgentTaskAggregateTotals {
                    failed: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "cook-conductor".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                    summary: Some("Remote provider agent task failed.".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::Provider),
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: serde_json::json!({}),
                    workflow: None,
                    follow_up: None,
                    metadata: serde_json::json!({
                        "provider": "fixture.agent-task-executor",
                        "provider_run_result": {
                            "schema": "custom-provider/agent-task-run-result/v1",
                            "status": "failed",
                            "failure_classification": "runtime"
                        }
                    }),
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                queue: AgentTaskQueueStatus {
                    max_concurrency: 1,
                    completed: 1,
                    ..AgentTaskQueueStatus::default()
                },
            };
            let envelope = serde_json::json!({
                "schema": "homeboy/agent-task-dispatch/v1",
                "run_id": "remote-run",
                "plan_id": "remote-plan",
                "state": "failed",
                "aggregate": aggregate,
            });

            record_remote_dispatch_failure(
                AgentTaskRemoteDispatchFailure {
                    run_id: "local-sparse-run",
                    local_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    remote_command: vec![
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook".to_string(),
                    ],
                    runner_id: "lab-a",
                    remote_workspace: "/runner/workspace/conductor",
                    stdout: "",
                    stderr: &envelope.to_string(),
                    exit_code: 1,
                },
                &envelope,
            )
            .expect("sparse dispatch failure recorded")
            .expect("dispatch envelope recognized");

            let loaded = status("local-sparse-run").expect("status loaded");
            let artifacts = artifacts("local-sparse-run").expect("artifacts loaded");
            let (raw_aggregate, _) =
                aggregate_source("local-sparse-run").expect("aggregate source");

            assert_eq!(loaded.tasks[0].task_id, "cook-conductor");
            assert_eq!(loaded.tasks[0].backend, "fixture.agent-task-executor");
            assert_eq!(loaded.metadata["remote_run_id"], "remote-run");
            assert!(artifacts
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == "remote-agent-task-logs"));
            assert!(artifacts
                .evidence_refs
                .iter()
                .any(|evidence| evidence.kind == "remote-agent-task-review"));
            assert!(raw_aggregate.contains("custom-provider/agent-task-run-result/v1"));
            assert!(raw_aggregate.contains("failure_classification"));
        });
    }

    #[test]
    fn record_completed_run_exposes_logs_and_artifacts() {
        with_isolated_home(|_| {
            let plan = test_plan();
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Succeeded,
                totals: AgentTaskAggregateTotals {
                    queued: 1,
                    succeeded: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("ok".to_string()),
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                        id: "patch".to_string(),
                        kind: "patch".to_string(),
                        name: Some("patch.diff".to_string()),
                        path: Some("/tmp/patch.diff".to_string()),
                        url: None,
                        mime: None,
                        size_bytes: None,
                        sha256: None,
                        metadata: Value::Null,
                    }],
                    evidence_refs: vec![AgentTaskEvidenceRef {
                        kind: "transcript".to_string(),
                        uri: "file:///tmp/transcript.json".to_string(),
                        label: Some("provider transcript".to_string()),
                    }],
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
                queue: Default::default(),
            };

            let record =
                record_completed_run(&plan, &aggregate, Some("run-complete")).expect("recorded");
            let log = logs(&record.run_id).expect("logs");
            let artifacts = artifacts(&record.run_id).expect("artifacts");

            assert_eq!(record.state, AgentTaskRunState::Succeeded);
            assert_eq!(log.events[0].state, AgentTaskState::Succeeded);
            assert_eq!(artifacts.artifacts[0].id, "patch");
            assert_eq!(artifacts.evidence_refs[0].kind, "transcript");
        });
    }

    #[test]
    fn submitted_run_can_be_loaded_marked_running_and_completed() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-execute")).expect("submitted");

            let loaded_plan = load_plan("run-execute").expect("plan loaded");
            let running = mark_running("run-execute").expect("marked running");
            let aggregate = succeeded_aggregate(&loaded_plan);

            let completed =
                record_run_aggregate("run-execute", &loaded_plan, &aggregate).expect("completed");
            let durable_status = status("run-execute").expect("status");

            assert_eq!(loaded_plan.plan_id, "plan-a");
            assert_eq!(running.state, AgentTaskRunState::Running);
            assert_eq!(running.tasks[0].state, AgentTaskState::Running);
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
            assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
            assert_eq!(completed.totals, Some(aggregate.totals.clone()));
            assert_eq!(durable_status.state, AgentTaskRunState::Succeeded);
            assert_eq!(durable_status.tasks[0].state, AgentTaskState::Succeeded);
            assert_eq!(durable_status.totals, Some(aggregate.totals.clone()));
            assert!(completed.aggregate_path.is_some());
        });
    }

    #[test]
    fn completed_run_persists_opaque_provider_handles_from_outcome_metadata() {
        with_isolated_home(|_| {
            let plan = test_plan();
            let mut aggregate = succeeded_aggregate(&plan);
            aggregate.outcomes[0].metadata = json!({
                "provider_handle": AgentTaskExecutionHandle {
                    task_id: "task-a".to_string(),
                    backend: "codebox".to_string(),
                    run_id: "provider-run-123".to_string(),
                    stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                    metadata: json!({ "opaque": { "provider_owned": true } }),
                }
            });

            let record = record_completed_run(&plan, &aggregate, Some("run-provider-handle"))
                .expect("recorded");

            assert_eq!(record.provider_handles.len(), 1);
            assert_eq!(record.provider_handles[0].task_id, "task-a");
            assert_eq!(record.provider_handles[0].backend, "codebox");
            assert_eq!(
                record.provider_handles[0].provider_run_id,
                "provider-run-123"
            );
            assert_eq!(
                record.provider_handles[0].stream_uri.as_deref(),
                Some("provider://runs/provider-run-123/events")
            );
            assert_eq!(
                record.provider_handles[0].state,
                Some(AgentTaskState::Succeeded)
            );
            assert_eq!(
                record.provider_handles[0].metadata["opaque"]["provider_owned"],
                json!(true)
            );
            assert_eq!(
                record.metadata["provider_run_ids"],
                json!(["provider-run-123"])
            );
        });
    }

    #[test]
    fn failed_provider_run_exposes_workflow_evidence_refs() {
        with_isolated_home(|_| {
            let plan = test_plan();
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Failed,
                totals: AgentTaskAggregateTotals {
                    queued: 1,
                    failed: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Failed,
                    summary: Some("provider task failed".to_string()),
                    failure_classification: Some(
                        crate::core::agent_task::AgentTaskFailureClassification::ExecutionFailed,
                    ),
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: Value::Null,
                    workflow: Some(AgentTaskWorkflowEvidence {
                        schema: AGENT_TASK_WORKFLOW_SCHEMA.to_string(),
                        id: "provider-run-123".to_string(),
                        label: Some("provider workflow".to_string()),
                        steps: vec![AgentTaskWorkflowStepEvidence {
                            id: "runtime".to_string(),
                            label: Some("runtime evidence".to_string()),
                            status: AgentTaskWorkflowStepStatus::Failed,
                            depends_on: Vec::new(),
                            started_at: None,
                            finished_at: None,
                            duration_ms: None,
                            metrics: Value::Null,
                            artifact_refs: vec![AgentTaskEvidenceRef {
                                kind: "provider-transcript".to_string(),
                                uri: "provider://runs/provider-run-123/transcript".to_string(),
                                label: Some("Provider transcript".to_string()),
                            }],
                            diagnostics: Vec::new(),
                            suggestions: Vec::new(),
                            metadata: Value::Null,
                        }],
                        metadata: Value::Null,
                    }),
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: vec![AgentTaskProgressEvent {
                    task_id: "task-a".to_string(),
                    state: AgentTaskState::Failed,
                    attempt: 1,
                    message: Some("provider task failed".to_string()),
                }],
                artifact_lineage: Vec::new(),
                queue: Default::default(),
            };

            let record = record_completed_run(&plan, &aggregate, Some("run-provider-failed"))
                .expect("recorded");
            let durable_status = status(&record.run_id).expect("status");
            let durable_artifacts = artifacts(&record.run_id).expect("artifacts");

            assert_eq!(durable_status.state, AgentTaskRunState::Failed);
            assert_eq!(durable_status.artifact_refs.len(), 1);
            assert_eq!(durable_status.artifact_refs[0].kind, "provider-transcript");
            assert_eq!(durable_artifacts.evidence_refs.len(), 1);
            assert_eq!(
                durable_artifacts.evidence_refs[0].uri,
                "provider://runs/provider-run-123/transcript"
            );
        });
    }

    #[test]
    fn cancel_marks_queued_run_and_tasks_cancelled() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-cancel")).expect("submitted");

            let record = cancel("run-cancel").expect("cancelled");

            assert_eq!(record.state, AgentTaskRunState::Cancelled);
            assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
            assert!(record.metadata["cancel_requested_at"].is_string());
        });
    }

    #[test]
    fn retry_submits_new_run_from_existing_plan() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-original")).expect("submitted");

            let record = retry("run-original", Some("run-retry")).expect("retry submitted");
            let loaded_plan = load_plan("run-retry").expect("retry plan loaded");

            assert_eq!(record.run_id, "run-retry");
            assert_eq!(record.state, AgentTaskRunState::Queued);
            assert_eq!(record.metadata["retry_of"], json!("run-original"));
            assert_eq!(loaded_plan.plan_id, "plan-a");
        });
    }

    #[test]
    fn status_recovers_terminal_state_from_durable_aggregate() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-stale-status")).expect("submitted");
            mark_running("run-stale-status").expect("marked running");
            let aggregate = succeeded_aggregate(&plan);
            store::write_aggregate("run-stale-status", &aggregate).expect("aggregate written");

            let recovered = status("run-stale-status").expect("status recovered");
            let persisted = store::read_record("run-stale-status").expect("record persisted");

            assert_eq!(recovered.state, AgentTaskRunState::Succeeded);
            assert_eq!(recovered.tasks[0].state, AgentTaskState::Succeeded);
            assert_eq!(recovered.totals, Some(aggregate.totals.clone()));
            assert_eq!(persisted.state, AgentTaskRunState::Succeeded);
            assert_eq!(persisted.tasks[0].state, AgentTaskState::Succeeded);
            assert_eq!(persisted.totals, Some(aggregate.totals.clone()));
        });
    }

    #[test]
    fn status_marks_running_run_without_owner_as_stale() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-stale-missing-owner")).expect("submitted");
            let mut record = store::read_record("run-stale-missing-owner").expect("record");
            record.state = AgentTaskRunState::Running;
            store::write_record(&record).expect("stored running record");

            let loaded = status("run-stale-missing-owner").expect("status loaded");

            assert_eq!(loaded.state, AgentTaskRunState::Running);
            assert_eq!(loaded.metadata["stale_running"], json!(true));
            assert_eq!(
                loaded.metadata["stale_running_reason"],
                "missing_runner_pid"
            );
        });
    }

    #[test]
    fn aggregate_source_loads_completed_run_without_path_spelunking() {
        with_isolated_home(|_| {
            let plan = test_plan();
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Succeeded,
                totals: AgentTaskAggregateTotals {
                    queued: 1,
                    succeeded: 1,
                    ..AgentTaskAggregateTotals::default()
                },
                outcomes: vec![AgentTaskOutcome {
                    schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("ok".to_string()),
                    failure_classification: None,
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                queue: Default::default(),
            };
            record_completed_run(&plan, &aggregate, Some("run-source")).expect("recorded");

            let (raw, path) = aggregate_source("run-source").expect("aggregate source");

            assert!(path.ends_with("aggregate.json"));
            assert!(raw.contains("task-a"));
        });
    }

    #[test]
    fn mark_running_reclaims_stale_running_record() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-stale-dead-owner")).expect("submitted");
            let mut record = store::read_record("run-stale-dead-owner").expect("record");
            record.state = AgentTaskRunState::Running;
            record.metadata = json!({ "runner_pid": u32::MAX });
            store::write_record(&record).expect("stored stale record");

            let running = mark_running("run-stale-dead-owner").expect("reclaimed");

            assert_eq!(running.state, AgentTaskRunState::Running);
            assert_eq!(running.metadata["reclaimed_stale_running"], json!(true));
            assert_eq!(running.metadata["runner_pid"], json!(std::process::id()));
        });
    }

    #[test]
    fn mark_running_rejects_live_running_record() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-live-owner")).expect("submitted");
            mark_running("run-live-owner").expect("marked running");

            let error = mark_running("run-live-owner").expect_err("live run rejected");

            assert!(error.message.contains("already running"));
        });
    }

    #[test]
    fn cancel_run_marks_queued_record_cancelled() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-cancel-queued")).expect("submitted");

            let cancelled =
                cancel_run("run-cancel-queued", Some("loser cell")).expect("queued run cancelled");
            let loaded = status("run-cancel-queued").expect("status loaded");

            assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
            assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
            assert_eq!(cancelled.metadata["cancel_reason"], json!("loser cell"));
            assert_eq!(loaded.state, AgentTaskRunState::Cancelled);
        });
    }

    #[test]
    fn cancel_run_reclaims_stale_running_record() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-cancel-stale")).expect("submitted");
            let mut record = store::read_record("run-cancel-stale").expect("record");
            record.state = AgentTaskRunState::Running;
            record.tasks[0].state = AgentTaskState::Running;
            record.metadata = json!({ "runner_pid": u32::MAX });
            store::write_record(&record).expect("stored stale record");

            let cancelled = cancel_run("run-cancel-stale", None).expect("stale run cancelled");

            assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
            assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
            assert_eq!(cancelled.metadata["cancelled_stale_running"], json!(true));
            assert!(cancelled.metadata.get("stale_running").is_none());
        });
    }

    #[test]
    fn cancel_run_rejects_live_running_record() {
        with_isolated_home(|_| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-cancel-live")).expect("submitted");
            mark_running("run-cancel-live").expect("marked running");

            let error = cancel_run("run-cancel-live", None).expect_err("live run rejected");

            assert!(error.message.contains("running under pid"));
            assert!(error.message.contains("provider live cancellation"));
        });
    }

    fn test_plan() -> AgentTaskPlan {
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
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    fn succeeded_aggregate(plan: &AgentTaskPlan) -> AgentTaskAggregate {
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
                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
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
            queue: Default::default(),
        }
    }
}
