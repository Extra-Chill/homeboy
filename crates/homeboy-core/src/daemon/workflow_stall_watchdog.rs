//! Check `item running => run unsettled or still progressing` on daemon ticks.
//!
//! `HOMEBOY_STALL_WATCHDOG_SECONDS` overrides the default 30-minute silence
//! bound; zero disables observation. Invalid values fall back to the default.
//! Time alone never authorizes settlement: Pending/Unknown are reported only.
//! This observer never recovers runs or changes worktrees/operator state.
use super::*;
use crate::notification_payload::{NotifyAction, NotifyEventKind, NotifyPayload, NotifySubject};
use crate::notify::NotifyEvent;
use crate::notify_outbox::{self, NotifyOnceMarker};

fn bound(configured: Option<&str>) -> Option<Duration> {
    let seconds = configured
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(30 * 60);
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

pub(crate) fn stall_watchdog(store: &JobStore) -> Result<()> {
    let Some(bound) = bound(
        std::env::var("HOMEBOY_STALL_WATCHDOG_SECONDS")
            .ok()
            .as_deref(),
    ) else {
        return Ok(());
    };
    observe_stalls(store, now_ms(), bound)
}

pub(super) fn observe_stalls(store: &JobStore, now: u64, bound: Duration) -> Result<()> {
    for (job, controller) in store.controller_jobs_of_type(WORKFLOW_JOB_TYPE, &[]) {
        if job.status.is_terminal() {
            continue;
        }
        let Some(state) = controller
            .checkpoint
            .and_then(|value| serde_json::from_value::<InstanceState>(value).ok())
        else {
            continue;
        };
        if state.status.is_terminal() {
            continue;
        }
        let request = WorkflowJobRequest::parse(&controller.request)?.running();
        for (step_id, step) in &state.steps {
            for (index, item) in step.items.iter().enumerate() {
                if item.phase != StepPhase::Running {
                    continue;
                }
                let (Some(run), Some(key)) = (item.run_id.as_deref(), item.key.as_deref()) else {
                    continue;
                };
                let child = store.controller_job_for_key(key);
                let mut latest = item
                    .spawned_at_ms
                    .or(item.started_at_ms)
                    .unwrap_or(job.created_at_ms);
                if let Some(child) = &child {
                    latest = latest.max(child.updated_at_ms);
                }
                // Cheapest signals first: a healthy item never reads its
                // event log or resolves its run.
                let quiet =
                    |latest: u64| u128::from(now.saturating_sub(latest)) > bound.as_millis();
                if !quiet(latest) {
                    continue;
                }
                if let Some(child) = &child {
                    for event in store.events(child.id)? {
                        latest = latest.max(event.timestamp_ms);
                    }
                }
                let progress = run_progress(Some(run));
                if let Some(at) = progress["updated_at_ms"].as_u64().or_else(|| {
                    progress["updated_at"]
                        .as_str()
                        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                        .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
                }) {
                    latest = latest.max(at);
                }
                if !quiet(latest) {
                    continue;
                }
                let silent = now.saturating_sub(latest);
                let resolved = run_outcome_resolver()
                    .read()
                    .expect("workflow run outcome resolver lock")
                    .as_ref()
                    .map(|resolve| resolve(run))
                    .unwrap_or(WorkflowRunOutcome::Unknown);
                let action = match (&resolved, &child) {
                    (WorkflowRunOutcome::Settled { outcome, reason }, Some(child)) => {
                        // Normal completion retains terminal-consumer custody. The
                        // workflow's existing step_result/advance loop owns item
                        // settlement, including retries, usage and downstream work.
                        if !child.status.is_terminal() {
                            if outcome == "succeeded" {
                                store.complete_controller_success(
                                    child.id,
                                    json!({"stall_watchdog": true}),
                                )?;
                            } else {
                                store.fail_controller_error(
                                    child.id,
                                    cook_terminal_error(run, outcome, reason.as_deref()),
                                    json!({"stall_watchdog": true}),
                                )?;
                            }
                        }
                        store.deliver_pending_step_terminals()?;
                        format!("settled as {outcome}")
                    }
                    _ => "reported only".to_string(),
                };
                let incident = format!("{}/{step_id}/{index}/{run}", request.instance);
                // NotifyOnceMarker is provenance, not a claim mechanism. Keep
                // the producer's durable receipt in the existing job ledger;
                // no additional watchdog state files are needed.
                if store.events(job.id)?.iter().any(|event| {
                    event
                        .data
                        .as_ref()
                        .is_some_and(|data| data["stall_watchdog_reported"] == incident)
                }) {
                    continue;
                }
                let kind = NotifyEventKind::NeedsAttention;
                let payload =
                    NotifyPayload::new(kind, NotifySubject::new("workflow", &request.instance))
                        .with_fact("Workflow", &request.instance)
                        .with_fact("Step", step_id)
                        .with_fact("Item", index.to_string())
                        .with_fact("Run", run)
                        .with_fact("Silent for", format!("{} minutes", silent / 60_000))
                        .with_fact("Action taken", action)
                        .with_action(NotifyAction::new(
                            "Status",
                            format!("homeboy agent-task status {run}"),
                        ));
                let event = NotifyEvent::lifecycle(kind, &request.instance, "stalled")
                    .with_title(format!("workflow {} stalled", request.instance))
                    .with_payload(payload)
                    .with_route(request.notification_route.as_ref());
                let dispatched = notify_outbox::dispatch_with_outbox(
                    &event,
                    Some(NotifyOnceMarker::new(
                        "workflow_stall",
                        &incident,
                        "stall-watchdog",
                    )),
                );
                if !matches!(
                    dispatched.disposition,
                    notify_outbox::NotifyOutboxDisposition::Dropped
                ) {
                    store.append_event(
                        job.id,
                        JobEventKind::Progress,
                        None,
                        Some(json!({"stall_watchdog_reported": incident})),
                    )?;
                }
            }
        }
    }
    Ok(())
}
