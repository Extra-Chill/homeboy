use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_jobs::{self, ActiveRunnerJobSummary, Job, JobEvent};
use crate::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
use crate::run_lifecycle_record::RunExecutionState;
use crate::run_lifecycle_status::RunLifecycleStatus;
use crate::{paths, Error, Result};

pub mod agent_task_provider;

pub const ACTIVITY_REPORT_SCHEMA: &str = "homeboy/activity-report/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityScope {
    ActiveRecent,
    All,
}

pub type ActivityState = RunLifecycleStatus;

pub fn is_active(state: ActivityState) -> bool {
    matches!(state, ActivityState::Queued | ActivityState::Running)
}

pub fn is_failure(state: ActivityState) -> bool {
    !is_active(state) && !matches!(state, ActivityState::Succeeded | ActivityState::Cancelled)
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

/// A store-specific view retained with the canonical activity item so state
/// reconciliation remains inspectable without returning duplicate work items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivitySourceProjection {
    pub source_store: String,
    pub id: String,
    pub state: ActivityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
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
    pub source_projections: Vec<ActivitySourceProjection>,
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
    /// Agent-task record-health summary, carried as JSON so core does not depend
    /// on the agent-task health type. Supplied by the agent-task activity
    /// provider (null when the agent-task subsystem is absent).
    #[serde(default)]
    pub agent_task_record_health: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}

pub fn activity_report(scope: ActivityScope, limit: usize) -> Result<ActivityReport> {
    let mut collector = ActivityCollector::default();
    observation::collect(&mut collector, limit)?;
    for item in agent_task_provider::agent_task_activity_items()? {
        collector.insert(item);
    }
    daemon_jobs::collect(&mut collector)?;
    runner_sessions::collect(&mut collector);
    let mut report = report_from_items(collector.items(scope, limit), "activity");
    report.agent_task_record_health = agent_task_provider::agent_task_record_health()?;
    Ok(report)
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
    let mut result = report_from_items(vec![item.clone()], "activity.show");
    result.agent_task_record_health = report.agent_task_record_health;
    Ok(result)
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
        agent_task_record_health: Value::Null,
        next_actions,
    }
}

fn counts_for_items(items: &[ActivityItem]) -> ActivityCounts {
    let mut counts = ActivityCounts {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        if is_active(item.state) {
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
    fn insert(&mut self, mut item: ActivityItem) {
        let projection = source_projection(&item);
        append_source_projection(&mut item.source_projections, projection);
        let key = canonical_identity(&item);
        self.items
            .entry(key)
            .and_modify(|existing| merge_item(existing, &item))
            .or_insert(item);
    }

    fn items(self, scope: ActivityScope, limit: usize) -> Vec<ActivityItem> {
        let mut items = self.items.into_values().collect::<Vec<_>>();
        items.sort_by(|left, right| item_sort_key(right).cmp(&item_sort_key(left)));
        if scope == ActivityScope::ActiveRecent {
            items.retain(|item| is_active(item.state) || item.finished_at.is_some());
        }
        items.truncate(limit.max(1));
        items
    }
}

fn canonical_identity(item: &ActivityItem) -> String {
    // Lifecycle records own agent-task state. Their durable id is also the
    // observation run id, so normalize both projections onto that one identity.
    if item.source_store == "agent-task.lifecycle" {
        return format!("run:{}", item.id);
    }
    if let Some(agent_task_run_id) = &item.refs.agent_task_run_id {
        return format!("run:{agent_task_run_id}");
    }
    if let Some(run_id) = &item.refs.run_id {
        return format!("run:{run_id}");
    }
    if let Some(job_id) = &item.refs.runner_job_id {
        return format!("runner-job:{job_id}");
    }
    format!("item:{}", item.id)
}

fn merge_item(existing: &mut ActivityItem, incoming: &ActivityItem) {
    if source_precedence(incoming) > source_precedence(existing)
        || (source_precedence(incoming) == source_precedence(existing)
            && incoming.updated_at > existing.updated_at)
    {
        let mut replacement = incoming.clone();
        merge_refs(&mut replacement, existing);
        append_unique(&mut replacement.artifacts, &existing.artifacts);
        append_unique(&mut replacement.evidence, &existing.evidence);
        append_actions(&mut replacement.next_actions, &existing.next_actions);
        append_source_projections(
            &mut replacement.source_projections,
            &existing.source_projections,
        );
        *existing = replacement;
        return;
    }

    merge_refs(existing, incoming);
    append_unique(&mut existing.artifacts, &incoming.artifacts);
    append_unique(&mut existing.evidence, &incoming.evidence);
    append_actions(&mut existing.next_actions, &incoming.next_actions);
    append_source_projections(
        &mut existing.source_projections,
        &incoming.source_projections,
    );
}

fn source_precedence(item: &ActivityItem) -> u8 {
    match item.source_store.as_str() {
        "agent-task.lifecycle" => 4,
        "runner.session" => 3,
        "daemon.jobs-json" => 2,
        "observation.sqlite" => 1,
        _ => 0,
    }
}

fn merge_refs(existing: &mut ActivityItem, incoming: &ActivityItem) {
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
}

fn source_projection(item: &ActivityItem) -> ActivitySourceProjection {
    ActivitySourceProjection {
        source_store: item.source_store.clone(),
        id: item.id.clone(),
        state: item.state,
        updated_at: item.updated_at.clone(),
        finished_at: item.finished_at.clone(),
    }
}

fn append_source_projection(
    target: &mut Vec<ActivitySourceProjection>,
    incoming: ActivitySourceProjection,
) {
    if !target.iter().any(|projection| {
        projection.source_store == incoming.source_store && projection.id == incoming.id
    }) {
        target.push(incoming);
    }
}

fn append_source_projections(
    target: &mut Vec<ActivitySourceProjection>,
    incoming: &[ActivitySourceProjection],
) {
    for projection in incoming {
        append_source_projection(target, projection.clone());
    }
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
        is_active(item.state),
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

fn ms_to_rfc3339(ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

mod observation {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector, limit: usize) -> Result<()> {
        let store = ObservationStore::open_initialized()?;
        let mut records = store.list_runs(RunListFilter {
            limit: Some(limit as i64),
            ..Default::default()
        })?;
        let listed_ids = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<BTreeSet<_>>();
        // Recent terminal records are bounded for display, but active work is
        // always included before the canonical report applies its final limit.
        records.extend(
            store
                .list_active_runs()?
                .into_iter()
                .filter(|record| !listed_ids.contains(&record.id)),
        );
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
            source_projections: Vec::new(),
            next_actions: actions_for_observation_run(&run.id, state_from_run_status(&run.status)),
        })
    }

    pub(super) fn state_from_run_status(status: &str) -> ActivityState {
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
        if is_active(state) {
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

mod daemon_jobs {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector) -> Result<()> {
        let path = paths::daemon_jobs_file()?;
        if !path.exists() {
            return Ok(());
        }
        let store = api_jobs::JobStore::open_without_reconciliation(path)?;
        for job in store.list() {
            collector.insert(item_from_job(&store, job)?);
        }
        Ok(())
    }

    pub(super) fn item_from_job(store: &api_jobs::JobStore, job: Job) -> Result<ActivityItem> {
        let state = ActivityState::from(job.status);
        let job_id = job.id.to_string();
        let (durable_run_id, agent_task_run_id) = job_run_refs(store, &job);
        Ok(ActivityItem {
            id: job_id.clone(),
            kind: job.operation.clone(),
            source_store: "daemon.jobs-json".to_string(),
            state,
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
                agent_task_run_id,
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
            source_projections: Vec::new(),
            next_actions: actions_for_job(None, &job_id, state),
        })
    }

    fn job_run_refs(store: &api_jobs::JobStore, job: &Job) -> (Option<String>, Option<String>) {
        store.events(job.id).unwrap_or_default().iter().fold(
            (None, None),
            |(durable, agent_task), event| {
                (
                    durable.or_else(|| event_metadata_string(event, &["durable_run_id", "run_id"])),
                    agent_task.or_else(|| event_metadata_string(event, &["agent_task_run_id"])),
                )
            },
        )
    }

    fn event_metadata_string(event: &JobEvent, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| event.data.as_ref()?.get(*key)?.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    }

    pub(super) fn actions_for_job(
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
            if is_active(state) {
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
}

mod runner_sessions {
    use super::*;

    pub(super) fn collect(collector: &mut ActivityCollector) {
        for report in
            crate::observation::runs_service::with_runner_evidence(|provider| provider.statuses())
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
            ActivityState::from(job.status)
        };
        ActivityItem {
            id: job
                .durable_run_id
                .clone()
                .unwrap_or_else(|| job.job_id.clone()),
            kind: job.kind.clone(),
            source_store: "runner.session".to_string(),
            state,
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
            source_projections: Vec::new(),
            next_actions: actions_for_runner_job(&job.runner_id, &job.job_id, state),
        }
    }

    fn actions_for_runner_job(
        runner_id: &str,
        job_id: &str,
        state: ActivityState,
    ) -> Vec<ActivityNextAction> {
        let mut actions = daemon_jobs::actions_for_job(Some(runner_id), job_id, state);
        if is_active(state) {
            actions.push(action("watch", format!("homeboy activity watch {job_id}")));
        }
        if is_failure(state) {
            actions.push(action(
                "reconcile",
                format!("homeboy runner job logs {runner_id} {job_id}"),
            ));
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_lifecycle::{
        reconcile_active_lab_runner_handoffs, record_lab_offload_planned, rewrite_record_for_test,
        status, AgentTaskRunState, LabOffloadProxyPlan,
    };
    use crate::api_jobs::JobStatus;
    use crate::observation::NewRunRecord;
    use crate::test_support::with_isolated_home;

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
            source_projections: Vec::new(),
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
    fn agent_task_lifecycle_is_authoritative_over_observation_and_runner_projections() {
        let mut collector = ActivityCollector::default();

        let mut observation = item("agent-task-1", ActivityState::Running);
        observation.kind = "agent-task".to_string();
        observation.source_store = "observation.sqlite".to_string();
        collector.insert(observation);

        let mut runner = item("runner-job-1", ActivityState::Running);
        runner.source_store = "runner.session".to_string();
        runner.refs.run_id = Some("agent-task-1".to_string());
        runner.refs.runner_job_id = Some("runner-job-1".to_string());
        collector.insert(runner);

        let mut lifecycle = item("agent-task-1", ActivityState::Queued);
        lifecycle.kind = "agent-task".to_string();
        lifecycle.source_store = "agent-task.lifecycle".to_string();
        lifecycle.refs.run_id = None;
        lifecycle.refs.agent_task_run_id = Some("agent-task-1".to_string());
        collector.insert(lifecycle);

        let items = collector.items(ActivityScope::All, 10);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "agent-task-1");
        assert_eq!(items[0].source_store, "agent-task.lifecycle");
        assert_eq!(items[0].state, ActivityState::Queued);
        assert_eq!(
            items[0]
                .source_projections
                .iter()
                .map(|projection| (projection.source_store.as_str(), projection.state))
                .collect::<Vec<_>>(),
            vec![
                ("agent-task.lifecycle", ActivityState::Queued),
                ("runner.session", ActivityState::Running),
                ("observation.sqlite", ActivityState::Running),
            ]
        );
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
    fn observation_status_projection_preserves_every_label_and_unknown_values() {
        for case in "running:running,pass:succeeded,skipped:succeeded,fail:failed,error:failed,stale:stale,:unknown,future_status:unknown".split(',') {
            let (status, expected) = case.split_once(':').expect("status case");
            assert_eq!(serde_json::to_value(observation::state_from_run_status(status)).unwrap(), expected, "{status:?}");
        }
    }

    #[test]
    fn agent_task_status_projection_preserves_every_state() {
        for label in "queued,running,succeeded,partial_failure,failed,cancelled".split(',') {
            let state: AgentTaskRunState = serde_json::from_value(label.into()).unwrap();
            let expected: ActivityState = serde_json::from_value(label.into()).unwrap();
            assert_eq!(
                ActivityState::from(RunExecutionState::from(state)),
                expected,
                "{state:?}"
            );
        }
    }

    #[test]
    fn activity_policy_preserves_active_and_failure_sets() {
        for case in "unknown:01,queued:10,running:10,succeeded:00,partial_failure:01,failed:01,cancelled:00,timed_out:01,stale:01".split(',') {
            let (label, flags) = case.split_once(':').expect("policy case");
            let state: ActivityState = serde_json::from_value(label.into()).unwrap();
            let (active, failure) = (flags.starts_with('1'), flags.ends_with('1'));
            assert_eq!(is_active(state), active, "{state:?} active");
            assert_eq!(is_failure(state), failure, "{state:?} failure");
        }
    }

    #[test]
    fn next_actions_are_lifted_as_exact_commands() {
        let report = report_from_items(vec![item("run-1", ActivityState::Running)], "activity");
        assert_eq!(report.next_actions, vec!["homeboy runs show run-1"]);
        assert_eq!(report.items[0].next_actions[0].label, "show");
    }

    #[test]
    fn daemon_job_projects_durable_and_agent_task_run_ids_from_metadata() {
        let store = api_jobs::JobStore::default();
        let job = store.create_with_source_snapshot_and_metadata(
            "runner.exec",
            None,
            Some(serde_json::json!({
                "durable_run_id": "agent-task-run-123",
                "agent_task_run_id": "agent-task-run-123",
            })),
        );

        let item = daemon_jobs::item_from_job(&store, job).expect("activity item");

        assert_eq!(item.refs.run_id.as_deref(), Some("agent-task-run-123"));
        assert_eq!(
            item.refs.agent_task_run_id.as_deref(),
            Some("agent-task-run-123")
        );
    }

    #[test]
    fn daemon_job_activity_collection_does_not_reconcile_running_jobs() {
        with_isolated_home(|_| {
            let path = paths::daemon_jobs_file().expect("jobs path");
            let store =
                api_jobs::JobStore::open_without_reconciliation(&path).expect("open durable store");
            let job = store.create("runner.exec");
            store.start(job.id).expect("start job");

            let mut collector = ActivityCollector::default();
            daemon_jobs::collect(&mut collector).expect("collect activity");

            let reopened = api_jobs::JobStore::open_without_reconciliation(&path)
                .expect("reopen durable store");
            assert_eq!(
                reopened.get(job.id).expect("job remains").status,
                JobStatus::Running
            );
        });
    }

    #[test]
    fn active_handoff_reconciliation_expires_a_busy_lab_runner_handoff_idempotently() {
        with_isolated_home(|_| {
            let command = vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ];
            record_lab_offload_planned(LabOffloadProxyPlan {
                run_id: "busy-runner-expired-handoff",
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/homeboy",
                remote_command: &command,
                durable_plan: None,
            })
            .expect("persist unbound handoff while runner is busy");
            rewrite_record_for_test("busy-runner-expired-handoff", |record| {
                record.metadata["handoff_acceptance"]["deadline_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("expire unbound handoff");

            assert_eq!(
                reconcile_active_lab_runner_handoffs().expect("reconcile expired handoff"),
                1
            );
            let expired = status("busy-runner-expired-handoff").expect("expired handoff status");
            assert_eq!(expired.state, AgentTaskRunState::Cancelled);
            assert_eq!(expired.metadata["handoff_acceptance"]["state"], "expired");
            assert_eq!(
                expired.metadata["runner_execution_record"]["status"],
                "failed"
            );
            assert_eq!(expired.metadata["retryable"], true);

            assert_eq!(
                reconcile_active_lab_runner_handoffs().expect("idempotent handoff reconciliation"),
                0
            );
            assert_eq!(
                status("busy-runner-expired-handoff")
                    .expect("expired handoff remains terminal")
                    .state,
                AgentTaskRunState::Cancelled
            );
        });
    }

    #[test]
    fn observation_collection_keeps_active_runs_outside_the_recent_source_limit() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let active = store
                .start_run(NewRunRecord::builder("active").build())
                .expect("active run");
            let recent_terminal = store
                .start_run(NewRunRecord::builder("terminal").build())
                .expect("terminal run");
            store
                .finish_run(&recent_terminal.id, RunStatus::Pass, None)
                .expect("finish terminal run");

            let mut collector = ActivityCollector::default();
            observation::collect(&mut collector, 1).expect("collect activity");
            let items = collector.items(ActivityScope::All, 10);

            assert!(items.iter().any(|item| item.id == active.id));
        });
    }

    #[test]
    fn empty_state_output_has_zero_counts() {
        let report = report_from_items(Vec::new(), "activity");
        assert_eq!(report.counts.total, 0);
        assert!(report.items.is_empty());
        assert!(report.next_actions.is_empty());
    }
}
