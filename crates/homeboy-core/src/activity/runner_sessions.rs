use super::{
    action, daemon_jobs, is_active, is_failure, ms_to_rfc3339, ActivityCollector,
    ActivityCrossRefs, ActivityItem, ActivityNextAction, ActivityRunnerRefs, ActivityState,
};
use crate::api_jobs::ActiveRunnerJobSummary;

pub(super) fn collect(collector: &mut ActivityCollector) {
    // Use the latency-bounded indexed view: activity only needs the
    // current/recent active-job list, never the full generation reconcile
    // that `statuses()` performs (one blocking poll per draining
    // generation). This keeps `homeboy activity` bounded as generation
    // history grows (#9522).
    for report in crate::observation::runs_service::with_runner_evidence(|provider| {
        provider.statuses_indexed()
    })
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
        state_conflicts: Vec::new(),
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
