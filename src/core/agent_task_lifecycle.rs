use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::agent_task::{AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskOutcome};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskPlan, AgentTaskProgressEvent, AgentTaskState,
};
use crate::core::{paths, Result};

#[path = "lifecycle_store.rs"]
mod lifecycle_store;

use lifecycle_store as store;

pub const AGENT_TASK_RUN_SCHEMA: &str = "homeboy/agent-task-run/v1";
pub const AGENT_TASK_RUN_LOG_SCHEMA: &str = "homeboy/agent-task-run-log/v1";
pub const AGENT_TASK_RUN_ARTIFACTS_SCHEMA: &str = "homeboy/agent-task-run-artifacts/v1";

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskRunTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
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
        schema: AGENT_TASK_RUN_SCHEMA.to_string(),
        run_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: plan_path.display().to_string(),
        aggregate_path: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
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
    record.state = AgentTaskRunState::Running;
    record.updated_at = Some(now_timestamp());
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
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

fn record_aggregate(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = store::write_aggregate(&record.run_id, aggregate)?;
    record.state = run_state_for_aggregate(aggregate);
    record.updated_at = Some(now_timestamp());
    record.aggregate_path = Some(aggregate_path.display().to_string());
    record.tasks = tasks_for_aggregate(plan, aggregate);
    record.artifact_refs = artifact_refs_for_outcomes(&aggregate.outcomes);
    store::write_record(&record)?;
    Ok(record.clone())
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    store::read_record(&sanitize_run_id(run_id))
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    let run_id = sanitize_run_id(run_id);
    let record = store::read_record(&run_id)?;
    let events = store::read_aggregate(&run_id)
        .map(|aggregate| aggregate.events)
        .unwrap_or_else(|_| queued_events(&record.tasks));
    Ok(AgentTaskRunLog {
        schema: AGENT_TASK_RUN_LOG_SCHEMA.to_string(),
        run_id,
        events,
    })
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    let run_id = sanitize_run_id(run_id);
    store::read_record(&run_id)?;
    let aggregate = store::read_aggregate(&run_id).ok();
    Ok(AgentTaskRunArtifacts {
        schema: AGENT_TASK_RUN_ARTIFACTS_SCHEMA.to_string(),
        run_id,
        artifacts: aggregate_artifacts(aggregate.as_ref()),
        evidence_refs: aggregate_evidence_refs(aggregate.as_ref()),
    })
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
                .flat_map(|outcome| outcome.evidence_refs.clone())
                .collect()
        })
        .unwrap_or_default()
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
            }
            task
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
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
        AGENT_TASK_AGGREGATE_SCHEMA,
    };
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn submit_plan_persists_queued_status() {
        with_temp_home(|| {
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
    fn record_completed_run_exposes_logs_and_artifacts() {
        with_temp_home(|| {
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
        with_temp_home(|| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-execute")).expect("submitted");

            let loaded_plan = load_plan("run-execute").expect("plan loaded");
            let running = mark_running("run-execute").expect("marked running");
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: loaded_plan.plan_id.clone(),
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
                queue: Default::default(),
            };

            let completed =
                record_run_aggregate("run-execute", &loaded_plan, &aggregate).expect("completed");

            assert_eq!(loaded_plan.plan_id, "plan-a");
            assert_eq!(running.state, AgentTaskRunState::Running);
            assert_eq!(running.tasks[0].state, AgentTaskState::Running);
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
            assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
            assert!(completed.aggregate_path.is_some());
        });
    }

    fn with_temp_home(run: impl FnOnce()) {
        let lock = test_home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("temp home");
        std::env::set_var("HOME", home.path());
        run();
        drop(lock);
    }

    fn test_home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
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
}
