use super::{
    action, is_active, ms_to_rfc3339, ActivityCollector, ActivityCrossRefs, ActivityEvidenceRef,
    ActivityItem, ActivityNextAction, ActivityRunnerRefs, ActivityState,
};
use crate::api_jobs::{self, Job, JobEvent};
use crate::{paths, Result};

/// Resolve a single daemon-job activity item by id without listing every
/// job. The daemon job id is a UUID; when `id` parses as one, look it up
/// directly. Non-UUID ids (run labels, etc.) resolve through other providers
/// or the full-scan fallback (#9762).
pub(super) fn probe_by_id(id: &str) -> Result<Option<ActivityItem>> {
    let Ok(job_id) = uuid::Uuid::parse_str(id) else {
        return Ok(None);
    };
    let path = paths::daemon_jobs_file()?;
    if !path.exists() {
        return Ok(None);
    }
    let store = api_jobs::JobStore::open_without_reconciliation(path)?;
    match store.get(job_id) {
        Ok(job) => Ok(Some(item_from_job(&store, job)?)),
        Err(_) => Ok(None),
    }
}

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
        state_conflicts: Vec::new(),
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
