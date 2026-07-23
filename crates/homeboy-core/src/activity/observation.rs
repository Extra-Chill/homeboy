use std::collections::BTreeSet;

use super::{
    action, is_active, metadata_string, ActivityCollector, ActivityCrossRefs, ActivityEvidenceRef,
    ActivityItem, ActivityNextAction, ActivityRunnerRefs, ActivityState,
};
use crate::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
use crate::Result;

/// Resolve a single observation-run activity item by id without scanning the
/// full corpus. Matches the persisted run id directly (indexed `get_run`),
/// so `activity show <run-id>` returns immediately for a known run (#9762).
pub(super) fn probe_by_id(id: &str) -> Result<Option<ActivityItem>> {
    let store = ObservationStore::open_initialized()?;
    match store.get_run(id)? {
        Some(run) => Ok(Some(item_from_run(&store, run)?)),
        None => Ok(None),
    }
}

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
        state_conflicts: Vec::new(),
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
