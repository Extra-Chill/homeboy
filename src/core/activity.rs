use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_lifecycle::{self, AgentTaskRunRecord, AgentTaskRunState};
use crate::core::api_jobs::{self, ActiveRunnerJobSummary, Job, JobStatus};
use crate::core::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
use crate::core::{paths, runners, Error, Result};

pub const ACTIVITY_REPORT_SCHEMA: &str = "homeboy/activity-report/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityScope {
    ActiveRecent,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
    TimedOut,
    Stale,
    Unknown,
}

impl ActivityState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::PartialFailure | Self::Failed | Self::TimedOut | Self::Stale | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityNextAction {
    pub label: String,
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRunnerRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrossRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEvidenceRef {
    pub id: String,
    pub kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityItem {
    pub id: String,
    pub kind: String,
    pub source_store: String,
    pub state: ActivityState,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub runner: ActivityRunnerRefs,
    #[serde(default)]
    pub refs: ActivityCrossRefs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ActivityEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ActivityEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<ActivityNextAction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCounts {
    pub total: usize,
    pub active: usize,
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub partial_failure: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub timed_out: usize,
    pub stale: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub counts: ActivityCounts,
    pub items: Vec<ActivityItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}

pub fn activity_report(scope: ActivityScope, limit: usize) -> Result<ActivityReport> {
    let mut collector = ActivityCollector::default();
    observation::collect(&mut collector, limit)?;
    agent_tasks::collect(&mut collector, limit)?;
    daemon_jobs::collect(&mut collector)?;
    runner_sessions::collect(&mut collector);
    Ok(report_from_items(collector.items(scope, limit), "activity"))
}

pub fn show_activity(id: &str) -> Result<ActivityReport> {
    let report = activity_report(ActivityScope::All, 1000)?;
    let Some(item) = resolve_item(&report.items, id) else {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("activity item not found: {id}"),
            Some(id.to_string()),
            Some(vec![
                "Run `homeboy activity` to list active and recent work.".to_string(),
            ]),
        ));
    };
    Ok(report_from_items(vec![item.clone()], "activity.show"))
}

pub fn resolve_activity(id: &str) -> Result<ActivityItem> {
    let report = activity_report(ActivityScope::All, 1000)?;
    resolve_item(&report.items, id).cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "id",
            format!("activity item not found: {id}"),
            Some(id.to_string()),
            Some(vec![
                "Run `homeboy activity` to list active and recent work.".to_string(),
            ]),
        )
    })
}

fn resolve_item<'a>(items: &'a [ActivityItem], id: &str) -> Option<&'a ActivityItem> {
    items.iter().find(|item| {
        item.id == id
            || item.refs.run_id.as_deref() == Some(id)
            || item.refs.agent_task_run_id.as_deref() == Some(id)
            || item.refs.runner_job_id.as_deref() == Some(id)
    })
}

fn report_from_items(items: Vec<ActivityItem>, command: &'static str) -> ActivityReport {
    let counts = counts_for_items(&items);
    let next_actions = items
        .iter()
        .flat_map(|item| {
            item.next_actions
                .iter()
                .map(|action| action.command.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ActivityReport {
        schema: ACTIVITY_REPORT_SCHEMA,
        command,
        counts,
        items,
        next_actions,
    }
}

fn counts_for_items(items: &[ActivityItem]) -> ActivityCounts {
    let mut counts = ActivityCounts {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        if item.state.is_active() {
            counts.active += 1;
        }
        match item.state {
            ActivityState::Queued => counts.queued += 1,
            ActivityState::Running => counts.running += 1,
            ActivityState::Succeeded => counts.succeeded += 1,
            ActivityState::PartialFailure => counts.partial_failure += 1,
            ActivityState::Failed => counts.failed += 1,
            ActivityState::Cancelled => counts.cancelled += 1,
            ActivityState::TimedOut => counts.timed_out += 1,
            ActivityState::Stale => counts.stale += 1,
            ActivityState::Unknown => counts.unknown += 1,
        }
    }
    counts
}

#[derive(Default)]
struct ActivityCollector {
    items: BTreeMap<String, ActivityItem>,
}

impl ActivityCollector {
    fn insert(&mut self, item: ActivityItem) {
        let key = dedupe_key(&item);
        self.items
            .entry(key)
            .and_modify(|existing| merge_item(existing, &item))
            .or_insert(item);
    }

    fn items(self, scope: ActivityScope, limit: usize) -> Vec<ActivityItem> {
        let mut items = self.items.into_values().collect::<Vec<_>>();
        items.sort_by(|left, right| item_sort_key(right).cmp(&item_sort_key(left)));
        if scope == ActivityScope::ActiveRecent {
            items.retain(|item| item.state.is_active() || item.finished_at.is_some());
        }
        items.truncate(limit.max(1));
        items
    }
}

fn dedupe_key(item: &ActivityItem) -> String {
    if let Some(run_id) = &item.refs.run_id {
        return format!("run:{run_id}");
    }
    if let Some(agent_task_run_id) = &item.refs.agent_task_run_id {
        return format!("agent-task:{agent_task_run_id}");
    }
    if let Some(job_id) = &item.refs.runner_job_id {
        return format!("runner-job:{job_id}");
    }
    item.id.clone()
}

fn merge_item(existing: &mut ActivityItem, incoming: &ActivityItem) {
    if existing.refs.run_id.is_none() {
        existing.refs.run_id = incoming.refs.run_id.clone();
    }
    if existing.refs.agent_task_run_id.is_none() {
        existing.refs.agent_task_run_id = incoming.refs.agent_task_run_id.clone();
    }
    if existing.refs.runner_job_id.is_none() {
        existing.refs.runner_job_id = incoming.refs.runner_job_id.clone();
    }
    if existing.runner.runner_id.is_none() {
        existing.runner.runner_id = incoming.runner.runner_id.clone();
    }
    if existing.runner.job_id.is_none() {
        existing.runner.job_id = incoming.runner.job_id.clone();
    }
    if existing.runner.transport.is_none() {
        existing.runner.transport = incoming.runner.transport.clone();
    }
    if incoming.state.is_active()
        || (!existing.state.is_active() && incoming.updated_at > existing.updated_at)
    {
        existing.state = incoming.state.clone();
    }
    append_unique(&mut existing.artifacts, &incoming.artifacts);
    append_unique(&mut existing.evidence, &incoming.evidence);
    append_actions(&mut existing.next_actions, &incoming.next_actions);
}

fn append_unique(target: &mut Vec<ActivityEvidenceRef>, incoming: &[ActivityEvidenceRef]) {
    for item in incoming {
        if !target.iter().any(|existing| existing.uri == item.uri) {
            target.push(item.clone());
        }
    }
}

fn append_actions(target: &mut Vec<ActivityNextAction>, incoming: &[ActivityNextAction]) {
    for action in incoming {
        if !target
            .iter()
            .any(|existing| existing.command == action.command)
        {
            target.push(action.clone());
        }
    }
}

fn item_sort_key(item: &ActivityItem) -> (bool, Option<DateTime<Utc>>, String) {
    (
        item.state.is_active(),
        item.updated_at
            .as_deref()
            .or(Some(item.created_at.as_str()))
            .and_then(parse_ts),
        item.id.clone(),
    )
}

fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
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

mod observation {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector, limit: usize) -> Result<()> {
        let store = ObservationStore::open_initialized()?;
        let records = store.list_runs(RunListFilter {
            limit: Some(limit as i64),
            ..Default::default()
        })?;
        for run in records {
            collector.insert(item_from_run(&store, run)?);
        }
        Ok(())
    }

    fn item_from_run(store: &ObservationStore, run: RunRecord) -> Result<ActivityItem> {
        let artifacts = store
            .list_artifacts(&run.id)?
            .into_iter()
            .map(|artifact| ActivityEvidenceRef {
                id: artifact.id,
                kind: artifact.kind,
                uri: artifact
                    .public_url
                    .or(artifact.url)
                    .unwrap_or(artifact.path),
            })
            .collect::<Vec<_>>();
        let runner_id = metadata_string(&run.metadata_json, &["runner_id"]);
        let job_id = metadata_string(&run.metadata_json, &["runner_job_id", "job_id"]);
        Ok(ActivityItem {
            id: run.id.clone(),
            kind: run.kind.clone(),
            source_store: "observation.sqlite".to_string(),
            state: state_from_run_status(&run.status),
            created_at: run.started_at.clone(),
            updated_at: run
                .finished_at
                .clone()
                .or_else(|| Some(run.started_at.clone())),
            finished_at: run.finished_at,
            command: run.command,
            cwd: run.cwd,
            runner: ActivityRunnerRefs {
                runner_id,
                job_id: job_id.clone(),
                transport: None,
            },
            refs: ActivityCrossRefs {
                run_id: Some(run.id.clone()),
                agent_task_run_id: None,
                runner_job_id: job_id,
            },
            artifacts,
            evidence: Vec::new(),
            next_actions: actions_for_observation_run(&run.id, state_from_run_status(&run.status)),
        })
    }

    fn state_from_run_status(status: &str) -> ActivityState {
        match RunStatus::from_label(status) {
            Some(RunStatus::Running) => ActivityState::Running,
            Some(RunStatus::Pass | RunStatus::Skipped) => ActivityState::Succeeded,
            Some(RunStatus::Fail | RunStatus::Error) => ActivityState::Failed,
            Some(RunStatus::Stale) => ActivityState::Stale,
            None => ActivityState::Unknown,
        }
    }

    fn actions_for_observation_run(run_id: &str, state: ActivityState) -> Vec<ActivityNextAction> {
        let mut actions = vec![action("show run", format!("homeboy runs show {run_id}"))];
        if state.is_active() {
            actions.push(action(
                "watch run",
                format!("homeboy activity watch {run_id}"),
            ));
        }
        actions.push(action(
            "artifacts",
            format!("homeboy runs artifacts {run_id}"),
        ));
        if matches!(state, ActivityState::Stale) {
            actions.push(action("reconcile stale runs", "homeboy runs reconcile"));
        }
        actions
    }
}

mod agent_tasks {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector, limit: usize) -> Result<()> {
        for record in agent_task_lifecycle::list_records()?
            .into_iter()
            .take(limit)
        {
            collector.insert(item_from_agent_task(record));
        }
        Ok(())
    }

    fn item_from_agent_task(record: AgentTaskRunRecord) -> ActivityItem {
        let runner_id = metadata_string(&record.metadata, &["runner_id"]);
        let job_id = metadata_string(&record.metadata, &["runner_job_id", "job_id"]);
        let remote_run_id = metadata_string(&record.metadata, &["remote_run_id"]);
        let state = state_from_agent_task(record.state);
        ActivityItem {
            id: record.run_id.clone(),
            kind: "agent-task".to_string(),
            source_store: "agent-task.lifecycle".to_string(),
            state: state.clone(),
            created_at: record.submitted_at.clone(),
            updated_at: record.updated_at.clone(),
            finished_at: if state.is_active() {
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
            next_actions: actions_for_agent_task(&record.run_id, state),
        }
    }

    fn state_from_agent_task(state: AgentTaskRunState) -> ActivityState {
        match state {
            AgentTaskRunState::Queued => ActivityState::Queued,
            AgentTaskRunState::Running => ActivityState::Running,
            AgentTaskRunState::Succeeded => ActivityState::Succeeded,
            AgentTaskRunState::PartialFailure => ActivityState::PartialFailure,
            AgentTaskRunState::Failed => ActivityState::Failed,
            AgentTaskRunState::Cancelled => ActivityState::Cancelled,
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
        if state.is_active() {
            actions.push(action("watch", format!("homeboy activity watch {run_id}")));
        } else if state.is_failure() {
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
}

mod daemon_jobs {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector) -> Result<()> {
        let path = paths::daemon_jobs_file()?;
        if !path.exists() {
            return Ok(());
        }
        let store = api_jobs::JobStore::open(path)?;
        for job in store.list() {
            collector.insert(item_from_job(job));
        }
        Ok(())
    }

    fn item_from_job(job: Job) -> ActivityItem {
        let state = state_from_job(job.status);
        let job_id = job.id.to_string();
        let durable_run_id = durable_run_id(&job);
        ActivityItem {
            id: job_id.clone(),
            kind: job.operation.clone(),
            source_store: "daemon.jobs-json".to_string(),
            state: state.clone(),
            created_at: ms_to_rfc3339(job.created_at_ms),
            updated_at: Some(ms_to_rfc3339(job.updated_at_ms)),
            finished_at: job.finished_at_ms.map(ms_to_rfc3339),
            command: Some(job.operation),
            cwd: None,
            runner: ActivityRunnerRefs {
                runner_id: job
                    .claimed_by_runner_id
                    .clone()
                    .or(job.target_runner_id.clone()),
                job_id: Some(job_id.clone()),
                transport: Some("daemon".to_string()),
            },
            refs: ActivityCrossRefs {
                run_id: durable_run_id,
                agent_task_run_id: None,
                runner_job_id: Some(job_id.clone()),
            },
            artifacts: job
                .artifacts
                .into_iter()
                .map(|artifact| ActivityEvidenceRef {
                    id: artifact.id,
                    kind: artifact.mime.unwrap_or_else(|| "artifact".to_string()),
                    uri: artifact
                        .url
                        .or(artifact.path)
                        .unwrap_or_else(|| "homeboy://artifact/unavailable".to_string()),
                })
                .collect(),
            evidence: Vec::new(),
            next_actions: actions_for_job(None, &job_id, state),
        }
    }

    fn durable_run_id(job: &Job) -> Option<String> {
        job.source_snapshot.as_ref().and_then(|_| None)
    }

    fn state_from_job(status: JobStatus) -> ActivityState {
        match status {
            JobStatus::Queued => ActivityState::Queued,
            JobStatus::Running => ActivityState::Running,
            JobStatus::Succeeded => ActivityState::Succeeded,
            JobStatus::Failed => ActivityState::Failed,
            JobStatus::Cancelled => ActivityState::Cancelled,
        }
    }

    fn actions_for_job(
        runner_id: Option<&str>,
        job_id: &str,
        state: ActivityState,
    ) -> Vec<ActivityNextAction> {
        let mut actions = Vec::new();
        if let Some(runner_id) = runner_id {
            actions.push(action(
                "logs",
                format!("homeboy runner job logs {runner_id} {job_id}"),
            ));
            if state.is_active() {
                actions.push(action(
                    "follow logs",
                    format!("homeboy runner job logs {runner_id} {job_id} --follow"),
                ));
            }
        } else {
            actions.push(action("daemon status", "homeboy daemon status"));
        }
        actions
    }

    fn ms_to_rfc3339(ms: u64) -> String {
        DateTime::<Utc>::from_timestamp_millis(ms as i64)
            .unwrap_or_else(Utc::now)
            .to_rfc3339()
    }
}

mod runner_sessions {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector) {
        for report in runners::statuses()
            .unwrap_or_default()
            .into_iter()
            .filter(|report| report.connected)
        {
            for job in report.active_jobs {
                collector.insert(item_from_active_runner_job(job));
            }
        }
    }

    fn item_from_active_runner_job(job: ActiveRunnerJobSummary) -> ActivityItem {
        let state = if job.stale_reason.is_some() {
            ActivityState::Stale
        } else {
            state_from_job(job.status)
        };
        ActivityItem {
            id: job
                .durable_run_id
                .clone()
                .unwrap_or_else(|| job.job_id.clone()),
            kind: job.kind.clone(),
            source_store: "runner.session".to_string(),
            state: state.clone(),
            created_at: ms_to_rfc3339(job.started_at_ms),
            updated_at: Some(ms_to_rfc3339(job.updated_at_ms)),
            finished_at: None,
            command: Some(job.command.clone()),
            cwd: job.cwd,
            runner: ActivityRunnerRefs {
                runner_id: Some(job.runner_id.clone()),
                job_id: Some(job.job_id.clone()),
                transport: Some(job.source.clone()),
            },
            refs: ActivityCrossRefs {
                run_id: job.durable_run_id,
                agent_task_run_id: None,
                runner_job_id: Some(job.job_id.clone()),
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            next_actions: actions_for_runner_job(&job.runner_id, &job.job_id, state),
        }
    }

    fn state_from_job(status: JobStatus) -> ActivityState {
        match status {
            JobStatus::Queued => ActivityState::Queued,
            JobStatus::Running => ActivityState::Running,
            JobStatus::Succeeded => ActivityState::Succeeded,
            JobStatus::Failed => ActivityState::Failed,
            JobStatus::Cancelled => ActivityState::Cancelled,
        }
    }

    fn actions_for_runner_job(
        runner_id: &str,
        job_id: &str,
        state: ActivityState,
    ) -> Vec<ActivityNextAction> {
        let mut actions = vec![action(
            "logs",
            format!("homeboy runner job logs {runner_id} {job_id}"),
        )];
        if state.is_active() {
            actions.push(action(
                "follow logs",
                format!("homeboy runner job logs {runner_id} {job_id} --follow"),
            ));
            actions.push(action("watch", format!("homeboy activity watch {job_id}")));
        }
        if state.is_failure() {
            actions.push(action(
                "reconcile",
                format!("homeboy runner job logs {runner_id} {job_id}"),
            ));
        }
        actions
    }

    fn ms_to_rfc3339(ms: u64) -> String {
        DateTime::<Utc>::from_timestamp_millis(ms as i64)
            .unwrap_or_else(Utc::now)
            .to_rfc3339()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, state: ActivityState) -> ActivityItem {
        ActivityItem {
            id: id.to_string(),
            kind: "bench".to_string(),
            source_store: "test".to_string(),
            state,
            created_at: "2026-07-04T00:00:00Z".to_string(),
            updated_at: None,
            finished_at: None,
            command: None,
            cwd: None,
            runner: ActivityRunnerRefs::default(),
            refs: ActivityCrossRefs {
                run_id: Some(id.to_string()),
                agent_task_run_id: None,
                runner_job_id: None,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            next_actions: vec![action("show", format!("homeboy runs show {id}"))],
        }
    }

    #[test]
    fn source_merging_dedupes_by_run_id() {
        let mut collector = ActivityCollector::default();
        collector.insert(item("run-1", ActivityState::Running));
        let mut duplicate = item("job-1", ActivityState::Queued);
        duplicate.refs.run_id = Some("run-1".to_string());
        duplicate.refs.runner_job_id = Some("job-1".to_string());
        duplicate.runner.job_id = Some("job-1".to_string());
        collector.insert(duplicate);

        let items = collector.items(ActivityScope::All, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].refs.run_id.as_deref(), Some("run-1"));
        assert_eq!(items[0].refs.runner_job_id.as_deref(), Some("job-1"));
    }

    #[test]
    fn id_resolution_checks_all_id_spaces() {
        let mut item = item("run-1", ActivityState::Succeeded);
        item.refs.agent_task_run_id = Some("agent-1".to_string());
        item.refs.runner_job_id = Some("job-1".to_string());
        let items = vec![item];

        assert!(resolve_item(&items, "run-1").is_some());
        assert!(resolve_item(&items, "agent-1").is_some());
        assert!(resolve_item(&items, "job-1").is_some());
        assert!(resolve_item(&items, "missing").is_none());
    }

    #[test]
    fn counts_normalize_states() {
        let items = vec![
            item("queued", ActivityState::Queued),
            item("running", ActivityState::Running),
            item("stale", ActivityState::Stale),
        ];
        let counts = counts_for_items(&items);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.active, 2);
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.stale, 1);
    }

    #[test]
    fn next_actions_are_lifted_as_exact_commands() {
        let report = report_from_items(vec![item("run-1", ActivityState::Running)], "activity");
        assert_eq!(report.next_actions, vec!["homeboy runs show run-1"]);
        assert_eq!(report.items[0].next_actions[0].label, "show");
    }

    #[test]
    fn empty_state_output_has_zero_counts() {
        let report = report_from_items(Vec::new(), "activity");
        assert_eq!(report.counts.total, 0);
        assert!(report.items.is_empty());
        assert!(report.next_actions.is_empty());
    }
}
