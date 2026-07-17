//! Agent-task implementation of the activity hook.
//!
//! Projects durable agent-task lifecycle records into core `ActivityItem`s and
//! supplies the record-health summary, provided to core's activity report
//! through the `ActivityAgentTaskProvider` hook so the activity report does not
//! depend on the agent-task subsystem directly.

use serde_json::Value;

use crate::activity::agent_task_provider::{
    register_activity_agent_task_provider, ActivityAgentTaskProvider,
};
use crate::activity::{
    is_active, is_failure, ActivityCrossRefs, ActivityEvidenceRef, ActivityItem,
    ActivityNextAction, ActivityRunnerRefs, ActivityState,
};
use crate::agent_task_lifecycle::{self, AgentTaskRunRecord};
use crate::run_lifecycle_record::RunExecutionState;
use crate::Result;

struct AgentTaskActivityProvider;

impl ActivityAgentTaskProvider for AgentTaskActivityProvider {
    fn agent_task_activity_items(&self) -> Result<Vec<ActivityItem>> {
        Ok(agent_task_lifecycle::list_records()?
            .into_iter()
            .map(item_from_agent_task)
            .collect())
    }

    fn agent_task_record_health(&self) -> Result<Value> {
        let summary = agent_task_lifecycle::record_health_summary()?;
        serde_json::to_value(summary).map_err(|error| {
            crate::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task record health".to_string()),
            )
        })
    }
}

fn metadata_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn action(label: impl Into<String>, command: impl Into<String>) -> ActivityNextAction {
    ActivityNextAction {
        label: label.into(),
        command: command.into(),
    }
}

fn item_from_agent_task(record: AgentTaskRunRecord) -> ActivityItem {
    let runner_id = metadata_string(&record.metadata, &["runner_id"]);
    let job_id = metadata_string(&record.metadata, &["runner_job_id", "job_id"]);
    let remote_run_id = metadata_string(&record.metadata, &["remote_run_id"]);
    let state = ActivityState::from(RunExecutionState::from(record.state));
    ActivityItem {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        source_store: "agent-task.lifecycle".to_string(),
        state,
        created_at: record.submitted_at.clone(),
        updated_at: record.updated_at.clone(),
        finished_at: if is_active(state) {
            None
        } else {
            record.updated_at.clone()
        },
        command: None,
        cwd: None,
        runner: ActivityRunnerRefs {
            runner_id,
            job_id: job_id.clone(),
            transport: remote_run_id,
        },
        refs: ActivityCrossRefs {
            run_id: None,
            agent_task_run_id: Some(record.run_id.clone()),
            runner_job_id: job_id,
        },
        artifacts: record
            .artifact_refs
            .into_iter()
            .map(|artifact| ActivityEvidenceRef {
                id: artifact.task_id,
                kind: artifact.kind,
                uri: artifact.uri,
            })
            .collect(),
        evidence: record
            .latest_executor_evidence
            .into_iter()
            .flat_map(|evidence| evidence.refs())
            .enumerate()
            .map(|(index, evidence)| ActivityEvidenceRef {
                id: evidence
                    .label
                    .unwrap_or_else(|| format!("evidence-{}", index + 1)),
                kind: evidence.kind,
                uri: evidence.uri,
            })
            .collect(),
        source_projections: Vec::new(),
        next_actions: actions_for_agent_task(&record.run_id, state),
    }
}

fn actions_for_agent_task(run_id: &str, state: ActivityState) -> Vec<ActivityNextAction> {
    let mut actions = vec![
        action("status", format!("homeboy agent-task status {run_id}")),
        action("logs", format!("homeboy agent-task logs {run_id}")),
        action(
            "artifacts",
            format!("homeboy agent-task artifacts {run_id}"),
        ),
    ];
    if is_active(state) {
        actions.push(action("watch", format!("homeboy activity watch {run_id}")));
    } else if is_failure(state) {
        actions.push(action(
            "retry",
            format!("homeboy agent-task retry --run {run_id}"),
        ));
    }
    if matches!(state, ActivityState::Stale) {
        actions.push(action("reconcile", "homeboy agent-task active --reconcile"));
    }
    actions
}

/// Register the agent-task activity provider. Called once at startup so core's
/// activity report includes agent-task records without depending on the
/// agent-task subsystem.
pub fn register() {
    register_activity_agent_task_provider(Box::new(AgentTaskActivityProvider));
}
