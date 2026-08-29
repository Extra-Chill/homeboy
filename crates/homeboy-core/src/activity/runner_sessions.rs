//! Runner-resident activity source.
//!
//! The other three sources (`observation`, `agent_task_provider`,
//! `daemon_jobs`) all read stores that live on **this** controller. A run
//! offloaded to a Lab runner is recorded on that runner until it reports back,
//! so none of them can see it — which is why `agent-task list` used to ship a
//! prose apology telling operators to go run a second, runner-scoped command
//! themselves. This module is that command, folded into the federation.
//!
//! ## Bounds (the part that matters more than the feature)
//!
//! `homeboy activity` is a common, interactive command; latency and hangs are
//! its failure modes. This source therefore *reuses* the existing bounded probe
//! rather than inventing one:
//!
//! * It calls `RunnerEvidenceProvider::statuses_indexed`, the latency-bounded
//!   read introduced for exactly this caller (#9522): one `/jobs` query against
//!   the already-connected session, **no** generation reconcile (which issues
//!   one blocking HTTP call per draining generation and can take minutes).
//! * That query runs under `readonly_probe_timeout()` — a 15s wall-clock
//!   default, overridable with `HOMEBOY_READONLY_PROBE_TIMEOUT_SECONDS` —
//!   enforced inside the runner layer. There is no unbounded wait here.
//! * A runner with no connected session is **never queried**: no session is
//!   opened, no SSH is started, no network is performed. That is reported as
//!   `queried: false`, mirroring the runner layer's own
//!   `RunnerActiveJobState::NotQueried`.
//! * A connected runner that fails or times out marks the report `partial` and
//!   names the runner. It does **not** fail the command; every other source
//!   still returns.
//!
//! When no runner layer is registered in this process (no Lab in play, and every
//! test that does not opt in) the federation short-circuits before touching
//! anything at all.

use super::{
    action, daemon_jobs, is_active, is_failure, ms_to_rfc3339, ActivityCollector, ActivityContext,
    ActivityCrossRefs, ActivityFilter, ActivityItem, ActivityNextAction, ActivityRunnerFederation,
    ActivityRunnerRefs, ActivityRunnerSource, ActivityState,
};
use crate::api_jobs::ActiveRunnerJobSummary;
use crate::observation::runs_service::{
    has_runner_evidence_provider, with_runner_evidence, RunnerConnectionInfo,
};

/// Federate runner-resident work into `collector`, returning the per-runner
/// accounting. Never errors: an unreachable runner degrades the answer, it does
/// not fail it.
pub(super) fn collect(
    collector: &mut ActivityCollector,
    enabled: bool,
    filter: &ActivityFilter,
) -> ActivityRunnerFederation {
    if !enabled || !has_runner_evidence_provider() {
        return ActivityRunnerFederation::default();
    }
    let statuses = with_runner_evidence(|provider| provider.statuses_indexed());
    federate(collector, statuses, filter)
}

/// Split from [`collect`] so the bounded/best-effort contract is testable
/// without a registered provider or a live runner.
fn federate(
    collector: &mut ActivityCollector,
    statuses: Vec<RunnerConnectionInfo>,
    filter: &ActivityFilter,
) -> ActivityRunnerFederation {
    let mut federation = ActivityRunnerFederation {
        enabled: true,
        partial: false,
        runners: Vec::with_capacity(statuses.len()),
    };
    for report in statuses {
        let RunnerConnectionInfo {
            runner_id,
            connected,
            active_jobs,
            active_jobs_available,
            active_jobs_error,
            stale_runner_jobs: _,
        } = report;

        let queried = connected && active_jobs_available;
        // A connected runner that did not answer is the whole reason this
        // accounting exists: its empty `active_jobs` means "unknown", not "idle".
        let unanswered = connected && !active_jobs_available;
        federation.partial |= unanswered;

        let error = unanswered.then(|| {
            active_jobs_error.unwrap_or_else(|| {
                "runner is connected but its bounded active-job probe did not answer; \
                 runner-resident work may be missing from this report"
                    .to_string()
            })
        });

        let mut items = 0;
        if queried {
            for job in active_jobs {
                let item = item_from_active_runner_job(job);
                if filter.matches(&item) {
                    collector.insert(item);
                    items += 1;
                }
            }
        }

        federation.runners.push(ActivityRunnerSource {
            runner_id,
            connected,
            queried,
            items,
            error,
        });
    }
    federation
}

fn item_from_active_runner_job(job: ActiveRunnerJobSummary) -> ActivityItem {
    let state = if job.stale_reason.is_some() {
        ActivityState::Stale
    } else {
        ActivityState::from(job.status)
    };
    // An offloaded agent-task run is a runner job whose durable record is the
    // agent-task run. Publishing it in the agent-task id space is what lets the
    // collector merge it with the controller-local projection once the run
    // reports back, instead of listing the same work twice.
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
        cwd: job.cwd.clone(),
        runner: ActivityRunnerRefs {
            runner_id: Some(job.runner_id.clone()),
            job_id: Some(job.job_id.clone()),
            transport: Some(job.source.clone()),
        },
        refs: ActivityCrossRefs {
            run_id: job.durable_run_id,
            runner_job_id: Some(job.job_id.clone()),
        },
        context: ActivityContext {
            worktree: job.cwd.clone(),
            ..Default::default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::ActivityScope;

    fn runner(runner_id: &str, connected: bool, available: bool) -> RunnerConnectionInfo {
        RunnerConnectionInfo {
            runner_id: runner_id.to_string(),
            connected,
            active_jobs_available: available,
            ..Default::default()
        }
    }

    /// The federation is off unless it is both requested and possible. With no
    /// runner layer registered in this process it must not touch anything.
    #[test]
    fn federation_is_inert_when_disabled() {
        let mut collector = ActivityCollector::default();
        let federation = collect(&mut collector, false, &ActivityFilter::default());

        assert!(!federation.enabled);
        assert!(!federation.partial);
        assert!(federation.runners.is_empty());
        assert!(collector.items(ActivityScope::All, 10).is_empty());
    }

    /// A disconnected runner is `NotQueried`, not a failure: nothing was asked,
    /// so the report is complete, not partial.
    #[test]
    fn a_disconnected_runner_is_not_queried_and_is_not_a_degradation() {
        let mut collector = ActivityCollector::default();
        let federation = federate(
            &mut collector,
            vec![runner("lab-offline", false, false)],
            &ActivityFilter::default(),
        );

        assert!(federation.enabled);
        assert!(!federation.partial);
        assert_eq!(federation.runners.len(), 1);
        assert!(!federation.runners[0].queried);
        assert!(!federation.runners[0].connected);
        assert!(federation.runners[0].error.is_none());
    }

    /// The load-bearing case: one runner is wedged, another is healthy. The
    /// healthy runner's work must still return, and the wedged one must be
    /// *named* rather than silently contributing an empty list.
    #[test]
    fn an_unanswering_runner_yields_partial_with_the_rest_intact() {
        let mut healthy = runner("lab-healthy", true, true);
        healthy.active_jobs = vec![job("job-healthy", Some("run-healthy"))];
        let mut wedged = runner("lab-wedged", true, false);
        wedged.active_jobs_error = Some("probe timed out after 15s".to_string());

        let mut collector = ActivityCollector::default();
        let federation = federate(
            &mut collector,
            vec![healthy, wedged],
            &ActivityFilter::default(),
        );

        assert!(federation.partial, "a connected runner failed to answer");
        let items = collector.items(ActivityScope::All, 10);
        assert_eq!(items.len(), 1, "the healthy runner's work still returns");
        assert_eq!(items[0].id, "run-healthy");

        let wedged = federation
            .runners
            .iter()
            .find(|source| source.runner_id == "lab-wedged")
            .expect("the runner that did not answer is named");
        assert!(wedged.connected);
        assert!(!wedged.queried);
        assert_eq!(wedged.items, 0);
        assert_eq!(
            wedged.error.as_deref(),
            Some("probe timed out after 15s"),
            "the reason travels with the answer"
        );

        let healthy = federation
            .runners
            .iter()
            .find(|source| source.runner_id == "lab-healthy")
            .expect("the healthy runner is accounted for");
        assert!(healthy.queried);
        assert_eq!(healthy.items, 1);
        assert!(healthy.error.is_none());
    }

    /// A connected runner that answers with nothing is a *complete* answer.
    /// Distinguishing this from the wedged case above is the entire point of
    /// carrying `active_jobs_available`.
    #[test]
    fn a_connected_idle_runner_is_complete_not_partial() {
        let mut collector = ActivityCollector::default();
        let federation = federate(
            &mut collector,
            vec![runner("lab-idle", true, true)],
            &ActivityFilter::default(),
        );

        assert!(!federation.partial);
        assert!(federation.runners[0].queried);
        assert_eq!(federation.runners[0].items, 0);
    }

    /// Runner-resident records appear in the report, in the agent-task id space
    /// when the runner job is an offloaded agent-task run — so the controller's
    /// own projection merges with it rather than duplicating it.
    #[test]
    fn runner_resident_agent_task_records_appear_in_the_agent_task_id_space() {
        let mut connected = runner("lab-a", true, true);
        connected.active_jobs = vec![job("job-1", Some("agent-task-run-1"))];

        let mut collector = ActivityCollector::default();
        let federation = federate(&mut collector, vec![connected], &ActivityFilter::default());

        assert!(!federation.partial);
        let items = collector.items(ActivityScope::All, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "agent-task-run-1");
        assert_eq!(items[0].source_store, "runner.session");
        assert_eq!(items[0].refs.run_id.as_deref(), Some("agent-task-run-1"));
        assert_eq!(items[0].refs.runner_job_id.as_deref(), Some("job-1"));
        assert_eq!(items[0].runner.runner_id.as_deref(), Some("lab-a"));
    }

    fn job(job_id: &str, durable_run_id: Option<&str>) -> ActiveRunnerJobSummary {
        ActiveRunnerJobSummary {
            runner_id: "lab-a".to_string(),
            job_id: job_id.to_string(),
            operation: "agent-task".to_string(),
            source: "daemon".to_string(),
            kind: "agent-task".to_string(),
            status: crate::api_jobs::JobStatus::Running,
            command: "homeboy agent-task run-plan".to_string(),
            cwd: None,
            started_at_ms: 0,
            updated_at_ms: 0,
            elapsed_ms: 0,
            heartbeat_age_ms: 0,
            claim: Default::default(),
            claim_expires_in_ms: None,
            lifecycle: None,
            durable_run_id: durable_run_id.map(str::to_string),
            stale_reason: None,
            lifecycle_state: None,
            retryable: None,
            active_child_count: None,
            active_cell_count: None,
        }
    }
}
