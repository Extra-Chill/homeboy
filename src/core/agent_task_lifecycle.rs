use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::agent_task::{AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskOutcome};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskPlan, AgentTaskProgressEvent, AgentTaskState,
};
use crate::core::{paths, Error, Result};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<crate::core::agent_task_scheduler::AgentTaskAggregateTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskRunTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
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
        totals: None,
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
    fn status_recovers_terminal_state_from_durable_aggregate() {
        with_temp_home(|| {
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
        with_temp_home(|| {
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
    fn mark_running_reclaims_stale_running_record() {
        with_temp_home(|| {
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
        with_temp_home(|| {
            let plan = test_plan();
            submit_plan(&plan, Some("run-live-owner")).expect("submitted");
            mark_running("run-live-owner").expect("marked running");

            let error = mark_running("run-live-owner").expect_err("live run rejected");

            assert!(error.message.contains("already running"));
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
        }
    }
}
