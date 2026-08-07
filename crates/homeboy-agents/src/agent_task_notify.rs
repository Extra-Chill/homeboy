//! Cook lifecycle notifications.
//!
//! Agent tasks used to emit nothing. A cook could run for twenty minutes and
//! finish in total silence, so "fan out N tasks and report to my chat thread"
//! produced N silences followed by N terminations the operator had to go
//! looking for.
//!
//! Emission is deliberately sparse. Cook already reports eight internal phases
//! (`durable_identity`, `provider_ready`, `provider_start`, `retry`,
//! `heartbeat`, `promotion`, `finalization`, terminal) plus a heartbeat every
//! fifteen seconds; forwarding that would make the destination useless. Only
//! three boundaries change what a human would *do*:
//!
//! - `durable_identity` — the first point at which the cook is addressable, so
//!   the operator can answer "what did I just start, and how do I watch it?"
//! - `retry` — the attempt budget is being consumed. Someone who intends to
//!   intervene has to know before it runs out, not after.
//! - terminal — the outcome, with the pull request link on success and the
//!   durable recovery commands on failure.
//!
//! `provider_ready`, `promotion`, `finalization`, and `heartbeat` are internal
//! progress with no decision attached, and are intentionally not delivered.

use homeboy_core::notification_payload::{
    NotifyAction, NotifyEventKind, NotifyLink, NotifyPayload, NotifySubject,
};
use homeboy_core::notification_route::{self, NotificationRoute};
use homeboy_core::notify::{NotifyDelivery, NotifyEvent, NotifyOutcome};
use homeboy_core::notify_outbox::{self, NotifyOnceMarker, NotifyOutboxDisposition};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

use crate::agent_task_loop_controller::AgentTaskLoopControllerState;
use crate::agent_task_service::{AgentTaskCookBatchReport, AgentTaskCookReport};

/// Generic subject class for every cook lifecycle notification.
const COOK_SUBJECT_KIND: &str = "agent_task_cook";

/// Generic subject class for a wave of cooks (a fanout or a cook batch).
const BATCH_SUBJECT_KIND: &str = "agent_task_cook_batch";

/// Generic subject class for a durable loop controller.
const CONTROLLER_SUBJECT_KIND: &str = "agent_task_controller";

/// Attribution recorded on the cook-level exactly-once marker.
const COOK_TERMINAL_DELIVERED_BY: &str = "cook-controller";

/// Attribution recorded on the batch-level exactly-once marker.
const BATCH_TERMINAL_DELIVERED_BY: &str = "cook-batch";

/// Whether a cook lifecycle event is delivered.
///
/// Non-terminal events require an explicitly bound route. A configured
/// operations default transport is an ambient policy for "tell me when work
/// finishes"; promoting it to per-attempt progress would be a noise regression
/// for every operator who already set one. An operator who passed
/// `--notification-route` named a destination for *this* work and gets the
/// full arc.
fn should_deliver(kind: NotifyEventKind, has_explicit_route: bool) -> bool {
    kind.is_terminal() || has_explicit_route
}

/// The destination this cook's events belong to.
///
/// `notification_route::current()` is a **thread-local**, so it only answers in
/// the process and on the thread that accepted `--notification-route`. A cook
/// resumed elsewhere — `cook --continue`, controller adoption, a claimed
/// continuation — starts with an empty one, which silently downgraded the whole
/// arc: `Started`/`Progress` were dropped by `should_deliver`, and the terminal
/// event went to the ambient default transport instead of the thread that
/// launched the work. The route is already persisted on the durable record, and
/// both the daemon completion backstop and `runs watch` already read it back
/// from there; this closes the same gap for cook lifecycle events (#11115).
///
/// In-process thread hops are still covered by
/// `notification_route::capture`/`bind`; the thread-local deliberately wins so
/// an explicitly bound route is never overridden by durable metadata.
///
/// # Precedence
///
/// Resolution is ordered most-explicit first, and this function implements only
/// the last two steps because the first two are already collapsed into the
/// thread-local before any command runs:
///
/// 1. **Explicit argv** — `--notification-transport` / `--notification-route`.
/// 2. **Propagated environment** — `HOMEBOY_NOTIFICATION_TRANSPORT` /
///    `HOMEBOY_NOTIFICATION_ROUTE`, written onto child processes by
///    `notification_route::child_env` and read back by
///    `notification_route::from_cli_or_env`. Steps 1 and 2 are resolved once at
///    process entry, where argv wins, and the winner is bound as the
///    thread-local this function reads.
/// 3. **Durable record** — the route persisted on the run, below.
///
/// The durable fallback is not made redundant by environment propagation. It
/// covers resumption paths that inherit neither argv nor environment from the
/// launching process — `cook --continue`, controller adoption, a claimed
/// continuation — so both remain load-bearing.
fn effective_route(run_id: &str) -> Option<NotificationRoute> {
    notification_route::current()
        .or_else(|| crate::agent_task_lifecycle::durable_notification_route(run_id))
}

fn deliver(event: NotifyEvent, run_id: &str) {
    let route = effective_route(run_id);
    if !should_deliver(event.kind, route.is_some()) {
        return;
    }
    // A notification is observability, never a cook failure mode. The outbox
    // preserves that exactly: enqueue is infallible from here, every storage
    // error degrades to "not queued", and nothing propagates to the cook.
    let _ = notify_outbox::dispatch_with_outbox(&event.with_route(route.as_ref()), None);
}

fn terminal_outcome(
    cook_id: &str,
    explicit_route: bool,
    outcome: &NotifyOutcome,
    disposition: &NotifyOutboxDisposition,
) -> Value {
    let (transport, error_class) = match &outcome.delivery {
        NotifyDelivery::NotConfigured => (None, Some("not_configured")),
        NotifyDelivery::Transport {
            transport_id,
            exit_code,
            ..
        } => (
            Some(transport_id.clone()),
            (!outcome.delivered).then_some(if exit_code.is_some() {
                "transport_rejected"
            } else {
                "transport_spawn_failed"
            }),
        ),
    };
    // `status` keeps its historical vocabulary. A failed attempt that the
    // outbox durably queued is still `queued` rather than `failed`: the
    // operator's question is "will this arrive?", and the answer changed.
    let status = if outcome.delivered {
        "delivered"
    } else if matches!(disposition, NotifyOutboxDisposition::Queued { .. }) {
        "queued"
    } else if matches!(outcome.delivery, NotifyDelivery::NotConfigured) {
        "not_configured"
    } else {
        "failed"
    };
    let outbox_entry_id = match disposition {
        NotifyOutboxDisposition::Queued { entry_id } => Some(entry_id.clone()),
        _ => None,
    };
    json!({
        "schema": "homeboy/cook-notification-delivery/v1",
        "cook_id": cook_id,
        "event_id": "terminal",
        "event_kind": outcome.event_kind,
        "transport": transport,
        "route_classification": if explicit_route { "explicit" } else { "default" },
        "status": status,
        "error_class": error_class,
        "outbox_entry_id": outbox_entry_id,
        "transport_result": safe_transport_result(outcome.result.as_ref()),
    })
}

/// Transport output belongs to the transport, and may contain credentials or a
/// raw destination. Retain only generic operational fields needed for retry.
fn safe_transport_result(result: Option<&Value>) -> Option<Value> {
    const KEYS: &[&str] = &[
        "schema",
        "status",
        "attempts",
        "delivery_mode",
        "route_kind",
        "retryable",
        "truncated",
    ];
    let object = result?.as_object()?;
    let mut safe = Map::new();
    for key in KEYS {
        let Some(value) = object.get(*key) else {
            continue;
        };
        match value {
            Value::Bool(_) | Value::Number(_) => {
                safe.insert((*key).to_string(), value.clone());
            }
            Value::String(value) if value.len() <= 128 => {
                safe.insert((*key).to_string(), Value::String(value.clone()));
            }
            _ => {}
        }
    }
    (!safe.is_empty()).then_some(Value::Object(safe))
}

fn cook_subject(cook_id: &str, run_id: &str, component: Option<&str>) -> NotifySubject {
    let mut subject = NotifySubject::new(COOK_SUBJECT_KIND, cook_id);
    if !run_id.is_empty() {
        subject.parent_id = Some(run_id.to_string());
    }
    subject.component = component.map(str::to_string);
    subject
}

/// Watch/diagnose commands legal at every point in a cook's life.
fn cook_actions(payload: NotifyPayload, cook_id: &str) -> NotifyPayload {
    payload
        .with_action(
            NotifyAction::new("status", format!("homeboy agent-task status {cook_id}"))
                .with_kind("show"),
        )
        .with_action(
            NotifyAction::new("watch", format!("homeboy activity watch {cook_id}"))
                .with_kind("watch"),
        )
}

/// Build the started payload. Separated from delivery so its content is
/// assertable without spawning a transport.
fn started_payload(
    cook_id: &str,
    run_id: &str,
    title: &str,
    component: Option<&str>,
    base: &str,
    max_attempts: u32,
    provider: &str,
) -> NotifyPayload {
    cook_actions(
        NotifyPayload::new(
            NotifyEventKind::Started,
            cook_subject(cook_id, run_id, component).with_phase("durable_identity"),
        )
        .with_fact("Task", title)
        .with_fact("Base", base)
        .with_fact("Provider", provider)
        .with_fact("Attempt budget", max_attempts.to_string()),
        cook_id,
    )
}

/// The cook now has a durable identity and is addressable.
pub(crate) fn cook_started(
    cook_id: &str,
    run_id: &str,
    title: &str,
    component: Option<&str>,
    base: &str,
    max_attempts: u32,
    provider: &str,
) {
    let payload = started_payload(
        cook_id,
        run_id,
        title,
        component,
        base,
        max_attempts,
        provider,
    );
    deliver(
        NotifyEvent::lifecycle(NotifyEventKind::Started, cook_id, "started")
            .with_title(format!("cook started — {title}"))
            .with_payload(payload),
        run_id,
    );
}

fn retry_payload(
    cook_id: &str,
    run_id: &str,
    component: Option<&str>,
    attempt: u32,
    max_attempts: u32,
) -> NotifyPayload {
    cook_actions(
        NotifyPayload::new(
            NotifyEventKind::Progress,
            cook_subject(cook_id, run_id, component)
                .with_phase("retry")
                .with_attempt(attempt),
        )
        .with_fact("Attempt", format!("{attempt} of {max_attempts}"))
        .with_fact("Reason", "the previous attempt did not pass its gates"),
        cook_id,
    )
}

/// A gate-feedback retry is consuming the attempt budget.
pub(crate) fn cook_retrying(
    cook_id: &str,
    run_id: &str,
    component: Option<&str>,
    attempt: u32,
    max_attempts: u32,
) {
    let payload = retry_payload(cook_id, run_id, component, attempt, max_attempts);
    deliver(
        NotifyEvent::lifecycle(NotifyEventKind::Progress, cook_id, "retrying")
            .with_title(format!("cook attempt {attempt} of {max_attempts}"))
            .with_payload(payload),
        run_id,
    );
}

/// Build the terminal payload from the report the cook already produced.
///
/// `exit_code` decides success, not the status string: the status vocabulary
/// (`succeeded`, `durable_failure`, `attempts_exhausted`, `moving_base`, ...)
/// is open, while the exit code is the contract every caller already branches
/// on.
fn terminal_payload(
    report: &AgentTaskCookReport,
    component: Option<&str>,
    exit_code: i32,
) -> NotifyPayload {
    let kind = if exit_code == 0 {
        NotifyEventKind::Completed
    } else {
        NotifyEventKind::NeedsAttention
    };
    let run_id = report.latest_run_id.clone().unwrap_or_default();
    let attempts = u32::try_from(report.attempts.len()).unwrap_or(u32::MAX);

    let mut subject = cook_subject(&report.cook_id, &run_id, component);
    subject.phase = report
        .terminal_phase
        .clone()
        .filter(|phase| !phase.is_empty());
    if attempts > 0 {
        subject.attempt = Some(attempts);
    }

    let mut payload = NotifyPayload::new(kind, subject)
        .with_fact("Status", report.status.clone())
        .with_fact("Attempts", attempts.to_string())
        .with_optional_fact("Stop reason", report.stop_reason.clone())
        .with_optional_fact(
            "Failure classification",
            report.terminal_failure_classification.clone(),
        );

    // The pull request link is the single most useful thing a completed cook
    // can hand a reviewer, and it was previously discarded along with the rest
    // of the finalization report.
    if let Some(finalization) = &report.finalization {
        if let Some(url) = finalization.get("pr_url").and_then(|url| url.as_str()) {
            payload = payload.with_link(NotifyLink::new("pull request", url));
        }
        payload = payload.with_optional_fact(
            "Pull request",
            finalization
                .get("pr_action")
                .and_then(|action| action.as_str()),
        );
    }

    // A failed cook already computed its own legal recovery commands. Forward
    // them verbatim instead of restating the failure in English.
    if let Some(context) = &report.failure_context {
        payload = payload
            .with_fact("Phase", context.phase.clone())
            .with_fact("Reason code", context.reason_code.clone())
            .with_fact("Recovery", context.recovery_reason.clone());
        let mut forwarded = HashSet::new();
        for action in context.next_actions.iter().chain(&context.legal_actions) {
            if !forwarded.insert((action.action.as_str(), action.command.as_str())) {
                continue;
            }
            payload = payload.with_action(
                NotifyAction::new(action.action.clone(), action.command.clone())
                    .with_kind("repair"),
            );
        }
    }

    cook_actions(payload, &report.cook_id)
}

/// The cook reached a terminal outcome.
///
/// Delivered at most once per *cook*. The pre-existing exactly-once marker is
/// keyed on a `runs` row, which dedupes one attempt against the runner-direct
/// and daemon-backstop paths but cannot dedupe an event whose subject is the
/// cook. That was tolerable while only the launching thread could deliver;
/// durable route rehydration (#11115) means a second process reaching the same
/// terminal boundary — a `--continue` over an already-terminal cook, an adopted
/// controller — would now re-announce an outcome the operator already has.
pub(crate) fn cook_terminal(report: &AgentTaskCookReport, component: Option<&str>, exit_code: i32) {
    // Claim before building the payload, and treat a failed claim the same as a
    // lost one: the runner-direct and daemon-backstop paths also decline to
    // dispatch when their marker cannot be established, because a duplicated
    // terminal notification is worse than a missing one.
    let claimed = crate::agent_task_lifecycle::claim_cook_terminal_notification(
        &report.cook_id,
        COOK_TERMINAL_DELIVERED_BY,
    )
    .unwrap_or(false);
    if !claimed {
        return;
    }
    let succeeded = exit_code == 0;
    // The cook id resolves through the same alias index when a report carries
    // no latest attempt, so a terminal event is never left unroutable.
    let route_run_id = report
        .latest_run_id
        .clone()
        .filter(|run_id| !run_id.trim().is_empty())
        .unwrap_or_else(|| report.cook_id.clone());
    let payload = terminal_payload(report, component, exit_code);
    let event = NotifyEvent::lifecycle(payload.kind, &report.cook_id, &report.status)
        .with_title(format!(
            "cook {} — {}",
            if succeeded {
                "succeeded"
            } else {
                "needs attention"
            },
            report.cook_id
        ))
        .with_payload(payload);
    let route = effective_route(&route_run_id);
    let dispatch = notify_outbox::dispatch_with_outbox(
        &event.with_route(route.as_ref()),
        Some(NotifyOnceMarker::new(
            COOK_SUBJECT_KIND,
            &report.cook_id,
            COOK_TERMINAL_DELIVERED_BY,
        )),
    );
    let persisted = terminal_outcome(
        &report.cook_id,
        route.is_some(),
        &dispatch.outcome,
        &dispatch.disposition,
    );
    let _ = crate::agent_task_lifecycle::record_cook_terminal_notification_outcome(
        &report.cook_id,
        persisted,
    );
    match dispatch.disposition {
        // Delivered, or durably queued: either way this cook's outcome will
        // reach its destination without another observer producing it. The
        // claim's exactly-once eligibility is consumed, which is precisely
        // what `confirm` records. Confirming on `Queued` is the fix for the
        // three-hour cook whose terminal event used to die with a sixty-second
        // transport outage: the claim lease is five minutes and the retry
        // budget is twenty, so leaving the claim merely *held* would let it
        // expire mid-retry and produce a duplicate.
        NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
            let _ = crate::agent_task_lifecycle::confirm_cook_terminal_notification(
                &report.cook_id,
                COOK_TERMINAL_DELIVERED_BY,
            );
        }
        // Nothing durable exists — no transport is configured, or the queue
        // could not be written. Release, exactly as before, so a later
        // terminal observer is eligible to try again.
        NotifyOutboxDisposition::Dropped => {
            let _ = crate::agent_task_lifecycle::release_cook_terminal_notification_claim(
                &report.cook_id,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Batch / wave terminal (W3-5)
// ---------------------------------------------------------------------------

/// Whole-portfolio counts a fanout supervisor computes but a plain cook batch
/// report does not carry.
///
/// `blocked` is the number the operator actually asks about — a child held on
/// a dependency is neither green nor broken, and reporting it as a failure is
/// the reason "10 unrelated cook messages" was never a usable wave summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchPortfolioCounts {
    pub ready: usize,
    pub blocked: usize,
    pub merged: usize,
}

/// Resolve the destination for a wave.
///
/// A batch has no durable run of its own, so resolution walks outwards: the
/// live thread-local (a fanout runs in the operator's own process), then the
/// wave's own durable metadata if it has any, then the first child that
/// recorded a route. Without the last step a wave resumed in a new process —
/// `fanout resume`, a supervisor advance — would silently fall back to the
/// ambient default transport instead of the thread that asked for the work.
fn batch_route(subject_id: &str, report: &AgentTaskCookBatchReport) -> Option<NotificationRoute> {
    notification_route::current()
        .or_else(|| crate::agent_task_lifecycle::durable_notification_route(subject_id))
        .or_else(|| {
            report.cooks.iter().find_map(|cell| {
                crate::agent_task_lifecycle::durable_notification_route(&cell.initial_run_id)
            })
        })
}

/// Build the wave-terminal payload. Separated from delivery so its content is
/// assertable without spawning a transport.
fn batch_terminal_payload(
    report: &AgentTaskCookBatchReport,
    subject_id: &str,
    component: Option<&str>,
    portfolio: Option<BatchPortfolioCounts>,
    exit_code: i32,
) -> NotifyPayload {
    // Attention is not the exit code alone. A wave can exit zero with children
    // that failed, and "wave done" with nothing else said is exactly the
    // silence this emitter exists to remove.
    let needs_attention =
        exit_code != 0 || report.failed > 0 || report.cancelled > 0 || report.timed_out > 0;
    let kind = if needs_attention {
        NotifyEventKind::NeedsAttention
    } else {
        NotifyEventKind::Completed
    };

    let mut subject = NotifySubject::new(BATCH_SUBJECT_KIND, subject_id);
    subject.component = component.map(str::to_string);
    if subject_id != report.batch_id {
        // The wave is addressed by its fanout id; the batch it ran is its
        // parent record, not a separate subject.
        subject.parent_id = Some(report.batch_id.clone());
    }

    let mut payload = NotifyPayload::new(kind, subject)
        .with_fact("Status", report.status.clone())
        .with_fact("Children", report.total.to_string())
        .with_fact("Succeeded", report.succeeded.to_string())
        .with_fact("Failed", report.failed.to_string());
    if report.cancelled > 0 {
        payload = payload.with_fact("Cancelled", report.cancelled.to_string());
    }
    if report.timed_out > 0 {
        payload = payload.with_fact("Timed out", report.timed_out.to_string());
    }
    if report.queued > 0 || report.running > 0 {
        payload = payload.with_fact(
            "Still in flight",
            format!("{} queued, {} running", report.queued, report.running),
        );
    }
    if let Some(portfolio) = portfolio {
        payload = payload
            .with_fact("Ready", portfolio.ready.to_string())
            .with_fact("Blocked", portfolio.blocked.to_string())
            .with_fact("Merged", portfolio.merged.to_string());
    }

    // Name the children that need a decision. A wave summary whose failures
    // are anonymous still costs the operator a second command.
    let attention: Vec<&str> = report
        .cooks
        .iter()
        .filter(|cell| cell.exit_code != 0)
        .map(|cell| cell.cook_id.as_str())
        .take(10)
        .collect();
    if !attention.is_empty() {
        payload = payload.with_fact("Needs attention", attention.join(", "));
    }

    payload
        .with_action(
            NotifyAction::new(
                "status",
                format!("homeboy agent-task fanout status {subject_id}"),
            )
            .with_kind("show"),
        )
        .with_action(
            NotifyAction::new(
                "resume",
                format!("homeboy agent-task fanout resume {subject_id}"),
            )
            .with_kind("repair"),
        )
}

/// A wave of cooks reached a terminal outcome.
///
/// This is the notification a ten-child fanout never sent: previously the
/// destination received ten unrelated cook messages and was never told the
/// wave was done, how many were green, how many needed attention, or how many
/// were blocked — even though the batch report already computed every one of
/// those totals.
///
/// `subject_id` is the operator-facing wave identity: a fanout id when the
/// batch ran under one, otherwise `report.batch_id`. `portfolio` carries the
/// whole-portfolio `ready`/`blocked`/`merged` counts a fanout supervisor
/// computes; pass `None` for a plain cook batch, which has no portfolio.
///
/// Delivered at most once per wave, through the same cook-notification claim
/// the per-cook terminal event uses — that claim is keyed on an arbitrary id,
/// so a wave gets its own without a second mechanism.
pub fn batch_terminal(
    report: &AgentTaskCookBatchReport,
    subject_id: &str,
    component: Option<&str>,
    portfolio: Option<BatchPortfolioCounts>,
    exit_code: i32,
) {
    // Claim before building the payload, exactly as `cook_terminal` does: a
    // duplicated wave summary is worse than a missing one, and a failed claim
    // is treated the same as a lost one.
    let claimed = crate::agent_task_lifecycle::claim_cook_terminal_notification(
        subject_id,
        BATCH_TERMINAL_DELIVERED_BY,
    )
    .unwrap_or(false);
    if !claimed {
        return;
    }
    let payload = batch_terminal_payload(report, subject_id, component, portfolio, exit_code);
    let needs_attention = payload.kind == NotifyEventKind::NeedsAttention;
    let event = NotifyEvent::lifecycle(payload.kind, subject_id, &report.status)
        .with_title(format!(
            "wave {} — {} of {} green{}",
            if needs_attention {
                "needs attention"
            } else {
                "done"
            },
            report.succeeded,
            report.total,
            portfolio
                .filter(|portfolio| portfolio.blocked > 0)
                .map(|portfolio| format!(", {} blocked", portfolio.blocked))
                .unwrap_or_default(),
        ))
        .with_payload(payload);
    let route = batch_route(subject_id, report);
    let dispatch = notify_outbox::dispatch_with_outbox(
        &event.with_route(route.as_ref()),
        Some(NotifyOnceMarker::new(
            BATCH_SUBJECT_KIND,
            subject_id,
            BATCH_TERMINAL_DELIVERED_BY,
        )),
    );
    let persisted = terminal_outcome(
        subject_id,
        route.is_some(),
        &dispatch.outcome,
        &dispatch.disposition,
    );
    let _ = crate::agent_task_lifecycle::record_cook_terminal_notification_outcome(
        subject_id, persisted,
    );
    match dispatch.disposition {
        NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
            let _ = crate::agent_task_lifecycle::confirm_cook_terminal_notification(
                subject_id,
                BATCH_TERMINAL_DELIVERED_BY,
            );
        }
        NotifyOutboxDisposition::Dropped => {
            let _ =
                crate::agent_task_lifecycle::release_cook_terminal_notification_claim(subject_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Controller lifecycle (W3-5)
// ---------------------------------------------------------------------------

/// One open wait, flattened for the notification payload.
#[derive(Debug, Clone)]
pub struct ControllerWaitSummary {
    pub wait_key: String,
    pub event_type: String,
    pub external_ref: Option<String>,
}

fn controller_subject(loop_id: &str, phase: Option<&str>) -> NotifySubject {
    let mut subject = NotifySubject::new(CONTROLLER_SUBJECT_KIND, loop_id);
    subject.phase = phase.map(str::to_string).filter(|phase| !phase.is_empty());
    subject
}

fn controller_actions(payload: NotifyPayload, loop_id: &str) -> NotifyPayload {
    payload
        .with_action(
            NotifyAction::new(
                "status",
                format!("homeboy agent-task controller status {loop_id}"),
            )
            .with_kind("show"),
        )
        .with_action(
            NotifyAction::new(
                "resume",
                format!("homeboy agent-task controller resume {loop_id}"),
            )
            .with_kind("repair"),
        )
}

/// Deliver a controller event, resolving its destination from durable state.
///
/// A controller is not a run, so it has no thread-local route of its own once
/// the daemon is the producer. `notification_route::current()` still wins when
/// an operator drove the transition from their own process.
fn deliver_controller(event: NotifyEvent, loop_id: &str) {
    let route = notification_route::current()
        .or_else(|| crate::agent_task_lifecycle::durable_notification_route(loop_id));
    if !should_deliver(event.kind, route.is_some()) {
        return;
    }
    let _ = notify_outbox::dispatch_with_outbox(&event.with_route(route.as_ref()), None);
}

fn controller_state_payload(
    loop_id: &str,
    phase: Option<&str>,
    from: AgentTaskLoopControllerState,
    to: AgentTaskLoopControllerState,
    reason: Option<&str>,
    open_waits: usize,
) -> NotifyPayload {
    // A controller that reached a state it cannot leave on its own needs a
    // human. Everything else is progress.
    let kind = match to {
        AgentTaskLoopControllerState::HumanReady
        | AgentTaskLoopControllerState::Escalated
        | AgentTaskLoopControllerState::Failed
        | AgentTaskLoopControllerState::Abandoned => NotifyEventKind::NeedsAttention,
        AgentTaskLoopControllerState::Completed => NotifyEventKind::Completed,
        AgentTaskLoopControllerState::Running | AgentTaskLoopControllerState::Waiting => {
            NotifyEventKind::Progress
        }
    };
    controller_actions(
        NotifyPayload::new(kind, controller_subject(loop_id, phase))
            .with_fact("State", controller_state_label(to))
            .with_fact("Previous state", controller_state_label(from))
            .with_optional_fact("Reason", reason.map(str::to_string))
            .with_fact("Open waits", open_waits.to_string()),
        loop_id,
    )
}

/// The controller changed durable state.
pub fn controller_state_changed(
    loop_id: &str,
    phase: Option<&str>,
    from: AgentTaskLoopControllerState,
    to: AgentTaskLoopControllerState,
    reason: Option<&str>,
    open_waits: usize,
) {
    if from == to {
        return;
    }
    let payload = controller_state_payload(loop_id, phase, from, to, reason, open_waits);
    deliver_controller(
        NotifyEvent::lifecycle(payload.kind, loop_id, controller_state_label(to))
            .with_title(format!(
                "controller {loop_id} — {}",
                controller_state_label(to)
            ))
            .with_payload(payload),
        loop_id,
    );
}

fn controller_waiting_payload(
    loop_id: &str,
    phase: Option<&str>,
    waits: &[ControllerWaitSummary],
) -> NotifyPayload {
    let mut payload = NotifyPayload::new(
        NotifyEventKind::Progress,
        controller_subject(loop_id, phase),
    )
    .with_fact("State", "waiting")
    .with_fact("Open waits", waits.len().to_string());
    for wait in waits.iter().take(6) {
        payload = payload.with_fact(
            format!("Waiting on {}", wait.wait_key),
            match &wait.external_ref {
                Some(reference) => format!("{} ({reference})", wait.event_type),
                None => wait.event_type.clone(),
            },
        );
    }
    controller_actions(payload, loop_id)
}

/// The controller parked in `Waiting` with open waits.
///
/// `Waiting` used to emit nothing at all, so an orchestrator had no signal
/// that its controller had stalled: `resume` returns `idle` and exits, and
/// nothing polls. This is that signal. It is `Progress`, so an ambient default
/// transport does not start receiving it — only an explicitly routed
/// controller gets the full arc.
pub fn controller_waiting(loop_id: &str, phase: Option<&str>, waits: &[ControllerWaitSummary]) {
    if waits.is_empty() {
        return;
    }
    let payload = controller_waiting_payload(loop_id, phase, waits);
    deliver_controller(
        NotifyEvent::lifecycle(NotifyEventKind::Progress, loop_id, "waiting")
            .with_title(format!(
                "controller {loop_id} — waiting on {} event(s)",
                waits.len()
            ))
            .with_payload(payload),
        loop_id,
    );
}

fn controller_action_failed_payload(
    loop_id: &str,
    phase: Option<&str>,
    action_id: &str,
    action_kind: &str,
    reason: &str,
) -> NotifyPayload {
    controller_actions(
        NotifyPayload::new(
            NotifyEventKind::NeedsAttention,
            controller_subject(loop_id, phase),
        )
        .with_fact("Action", action_id)
        .with_fact("Action kind", action_kind)
        .with_fact("Reason", reason),
        loop_id,
    )
    .with_action(
        NotifyAction::new(
            "diagnose",
            format!("homeboy agent-task controller diagnose {loop_id}"),
        )
        .with_kind("repair"),
    )
}

/// A controller action failed and the loop stopped.
pub fn controller_action_failed(
    loop_id: &str,
    phase: Option<&str>,
    action_id: &str,
    action_kind: &str,
    reason: &str,
) {
    let payload = controller_action_failed_payload(loop_id, phase, action_id, action_kind, reason);
    deliver_controller(
        NotifyEvent::lifecycle(NotifyEventKind::NeedsAttention, loop_id, "action_failed")
            .with_title(format!("controller {loop_id} — action {action_id} failed"))
            .with_payload(payload),
        loop_id,
    );
}

/// Stable snake_case labels for the controller state vocabulary.
///
/// The enum's `Debug` rendering is not a contract; these labels are what a
/// destination reads, so they are written once here rather than derived from
/// formatting at each emission site.
fn controller_state_label(state: AgentTaskLoopControllerState) -> &'static str {
    match state {
        AgentTaskLoopControllerState::Running => "running",
        AgentTaskLoopControllerState::Waiting => "waiting",
        AgentTaskLoopControllerState::HumanReady => "human_ready",
        AgentTaskLoopControllerState::Completed => "completed",
        AgentTaskLoopControllerState::Abandoned => "abandoned",
        AgentTaskLoopControllerState::Escalated => "escalated",
        AgentTaskLoopControllerState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_service::{AgentTaskCookFailureContext, AgentTaskCookRecoveryAction};

    fn report(status: &str, finalization: Option<serde_json::Value>) -> AgentTaskCookReport {
        AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id: "cook-abc".to_string(),
            latest_run_id: Some("run-1".to_string()),
            history_run_ids: Vec::new(),
            invocation_run_ids: Vec::new(),
            status: status.to_string(),
            // These tests all build terminal payloads; the notification only
            // fires for a terminal disposition in the first place.
            disposition: homeboy_core::cook_status::CookDisposition::Terminal,
            attempts: Vec::new(),
            finalization,
            intentional_no_change: None,
            selected_candidate: None,
            stop_reason: None,
            terminal_phase: None,
            terminal_failure_classification: None,
            moving_base_recovery: None,
            failure_context: None,
        }
    }

    fn failure_context() -> AgentTaskCookFailureContext {
        AgentTaskCookFailureContext {
            cook_id: "cook-abc".to_string(),
            latest_run_id: "run-1".to_string(),
            selected_run_id: None,
            selected_task_id: None,
            selected_artifact_id: None,
            promotion_provenance: None,
            durable_recipe_ref: "recipe".to_string(),
            lifecycle_state: "failed".to_string(),
            phase: "controller".to_string(),
            reason_code: "validation_invalid_argument".to_string(),
            diagnostic: None,
            continuation_admission: None,
            blocking_claim: None,
            provider_budget_consumed: true,
            provider_executions_consumed: 1,
            recovery_legal: true,
            recovery_reason: "the durable recipe is intact".to_string(),
            legal_actions: vec![AgentTaskCookRecoveryAction {
                action: "continue".to_string(),
                command: "homeboy agent-task cook --continue cook-abc".to_string(),
            }],
            next_actions: vec![AgentTaskCookRecoveryAction {
                action: "diagnose".to_string(),
                command: "homeboy agent-task diagnose cook-abc".to_string(),
            }],
        }
    }

    #[test]
    fn successful_cook_carries_the_pull_request_link() {
        let report = report(
            "succeeded",
            Some(serde_json::json!({
                "pr_url": "https://example.test/pull/42",
                "pr_action": "created",
            })),
        );
        let payload = terminal_payload(&report, Some("homeboy"), 0);
        assert_eq!(payload.kind, NotifyEventKind::Completed);
        assert_eq!(payload.links.len(), 1);
        assert_eq!(payload.links[0].url, "https://example.test/pull/42");
        // The prose half must carry it too, for transports that only render text.
        let body = payload.render_body();
        assert!(body.contains("https://example.test/pull/42"), "{body}");
        assert!(body.contains("Pull request: created"), "{body}");
        assert!(body.contains("Component: homeboy"), "{body}");
    }

    #[test]
    fn failed_cook_forwards_its_own_legal_recovery_commands() {
        let mut failed = report("durable_failure", None);
        failed.failure_context = Some(failure_context());
        let payload = terminal_payload(&failed, None, 1);
        assert_eq!(payload.kind, NotifyEventKind::NeedsAttention);
        let commands: Vec<_> = payload
            .actions
            .iter()
            .map(|action| action.command.as_str())
            .collect();
        assert!(
            commands.contains(&"homeboy agent-task diagnose cook-abc"),
            "{commands:?}"
        );
        assert!(
            commands.contains(&"homeboy agent-task cook --continue cook-abc"),
            "{commands:?}"
        );
        assert!(payload
            .render_body()
            .contains("Reason code: validation_invalid_argument"));
    }

    #[test]
    fn failed_cook_forwards_a_recovery_action_once_when_report_sections_overlap() {
        let mut failed = report("pre_execution_failure", None);
        let recovery = AgentTaskCookRecoveryAction {
            action: "refresh_lab_runtime".to_string(),
            command: "homeboy runner refresh-homeboy homeboy-lab --ref required --reconnect"
                .to_string(),
        };
        let mut context = failure_context();
        context.legal_actions = vec![recovery.clone()];
        context.next_actions = vec![recovery.clone()];
        failed.failure_context = Some(context);

        let payload = terminal_payload(&failed, None, 1);
        assert_eq!(
            payload
                .actions
                .iter()
                .filter(
                    |action| action.label == recovery.action && action.command == recovery.command
                )
                .count(),
            1
        );
    }

    #[test]
    fn terminal_kind_follows_the_exit_code_not_the_open_status_vocabulary() {
        // An unrecognized status must not be mistaken for success.
        let unknown = report("moving_base", None);
        assert_eq!(
            terminal_payload(&unknown, None, 1).kind,
            NotifyEventKind::NeedsAttention
        );
        assert_eq!(
            terminal_payload(&unknown, None, 0).kind,
            NotifyEventKind::Completed
        );
    }

    #[test]
    fn started_payload_answers_what_did_i_just_start() {
        let payload = started_payload(
            "cook-abc",
            "run-1",
            "fix the notifier",
            Some("homeboy"),
            "main",
            3,
            "claude",
        );
        assert_eq!(payload.kind, NotifyEventKind::Started);
        let subject = payload.subject.clone().expect("subject");
        assert_eq!(subject.id, "cook-abc");
        assert_eq!(subject.parent_id.as_deref(), Some("run-1"));
        assert_eq!(subject.phase.as_deref(), Some("durable_identity"));
        let body = payload.render_body();
        assert!(body.contains("Task: fix the notifier"), "{body}");
        assert!(body.contains("Attempt budget: 3"), "{body}");
        assert!(body.contains("homeboy activity watch cook-abc"), "{body}");
    }

    #[test]
    fn retry_payload_names_the_attempt_and_the_budget() {
        let payload = retry_payload("cook-abc", "run-1", None, 2, 3);
        assert_eq!(payload.kind, NotifyEventKind::Progress);
        assert_eq!(payload.subject.clone().unwrap().attempt, Some(2));
        assert!(payload.render_body().contains("Attempt: 2 of 3"));
    }

    #[test]
    fn only_terminal_events_reach_an_ambient_default_transport() {
        // The noise contract. A configured default transport keeps receiving
        // outcomes and must not start receiving per-attempt progress.
        assert!(should_deliver(NotifyEventKind::Completed, false));
        assert!(should_deliver(NotifyEventKind::NeedsAttention, false));
        assert!(!should_deliver(NotifyEventKind::Started, false));
        assert!(!should_deliver(NotifyEventKind::Progress, false));
        // An explicitly routed cook gets the full arc.
        assert!(should_deliver(NotifyEventKind::Started, true));
        assert!(should_deliver(NotifyEventKind::Progress, true));
    }

    #[test]
    fn cook_lifecycle_emission_is_inert_without_a_configured_transport() {
        // End-to-end guard that emission never panics or fails a cook when no
        // transport is installed at all.
        homeboy_core::test_support::with_isolated_home(|_| {
            cook_started("cook-abc", "run-1", "task", None, "main", 3, "claude");
            cook_retrying("cook-abc", "run-1", None, 2, 3);
            cook_terminal(&report("succeeded", None), None, 0);
            // A distinct cook: the terminal event is now claimed once per cook,
            // so reusing `cook-abc` would exercise the dedupe rather than the
            // failure path.
            let mut failed = report("durable_failure", None);
            failed.cook_id = "cook-def".to_string();
            cook_terminal(&failed, None, 1);
        });
    }

    fn route(destination: &str) -> NotificationRoute {
        NotificationRoute::new("extension", destination).expect("route")
    }

    fn seed_run_with_route(run_id: &str, destination: &str) {
        let plan = crate::agent_task_scheduler::AgentTaskPlan::new("notify-plan", Vec::new());
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("durable run");
        crate::agent_task_lifecycle::persist_notification_route(run_id, &route(destination))
            .expect("persist route");
    }

    fn install_transport(id: &str, command: Vec<&str>) {
        let mut manifest: homeboy_extension_contract::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Test transport",
                "version": "1.0.0",
                "notification_transports": [{
                    "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                    "id": id,
                    "command": command,
                }]
            }))
            .expect("manifest");
        manifest.id = "cook-notify-test".to_string();
        homeboy_core::extension_store::save_manifest(&manifest).expect("install transport");
    }

    fn set_default_transport(id: &str) {
        homeboy_core::defaults::save_config(&homeboy_core::defaults::HomeboyConfig {
            notifications: homeboy_core::defaults::NotificationConfig {
                default_transport: Some(id.to_string()),
            },
            ..Default::default()
        })
        .expect("configure transport");
    }

    fn latest_delivery(cook_id: &str) -> Value {
        crate::agent_task_lifecycle::cook_terminal_notification_outcome(cook_id)
            .expect("read outcome")
            .expect("outcome")
    }

    #[test]
    fn terminal_delivery_records_success_and_confirms_the_once_marker() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("test.cook", vec!["true"]);
            set_default_transport("test.cook");

            cook_terminal(&report("succeeded", None), None, 0);
            let delivery = latest_delivery("cook-abc");
            assert_eq!(delivery["status"], "delivered");
            assert_eq!(delivery["transport"], "test.cook");
            assert_eq!(delivery["route_classification"], "default");
            assert!(
                !crate::agent_task_lifecycle::claim_cook_terminal_notification("cook-abc", "test")
                    .unwrap()
            );
        });
    }

    #[test]
    fn non_delivery_is_recorded_and_leaves_terminal_delivery_eligible() {
        homeboy_core::test_support::with_isolated_home(|_| {
            cook_terminal(&report("durable_failure", None), None, 1);
            let delivery = latest_delivery("cook-abc");
            assert_eq!(delivery["status"], "not_configured");
            assert_eq!(delivery["error_class"], "not_configured");
            assert!(
                crate::agent_task_lifecycle::claim_cook_terminal_notification("cook-abc", "test")
                    .unwrap()
            );
            crate::agent_task_lifecycle::release_cook_terminal_notification_claim("cook-abc")
                .unwrap();
        });
    }

    #[test]
    fn transport_rejection_and_spawn_failure_are_classified_without_transport_output() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("test.reject", vec!["false"]);
            set_default_transport("test.reject");
            cook_terminal(&report("durable_failure", None), None, 1);
            let rejected = latest_delivery("cook-abc");
            // A rejected transport is an outage, so the event is now durably
            // queued rather than lost. The classification of *why* the attempt
            // failed is unchanged.
            assert_eq!(rejected["status"], "queued");
            assert_eq!(rejected["error_class"], "transport_rejected");
            assert!(rejected["outbox_entry_id"].is_string());

            install_transport(
                "test.spawn",
                vec!["/definitely/not/a/notification-transport"],
            );
            set_default_transport("test.spawn");
            let mut spawn = report("durable_failure", None);
            spawn.cook_id = "cook-spawn".to_string();
            cook_terminal(&spawn, None, 1);
            let spawned = latest_delivery("cook-spawn");
            assert_eq!(spawned["error_class"], "transport_spawn_failed");
            assert_eq!(spawned["status"], "queued");
        });
    }

    #[test]
    fn a_terminal_outcome_survives_a_transport_outage() {
        // The whole point of W3-6. A cook that ran for hours must not lose its
        // outcome because the chat transport was down for a minute.
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("test.outage", vec!["false"]);
            set_default_transport("test.outage");
            cook_terminal(&report("succeeded", None), None, 0);

            let queued = homeboy_core::notify_outbox::pending_entries();
            assert_eq!(queued.len(), 1, "{queued:?}");
            assert_eq!(queued[0].event.run_id, "cook-abc");
            assert_eq!(
                queued[0]
                    .once_marker
                    .as_ref()
                    .map(|marker| marker.subject_id.as_str()),
                Some("cook-abc"),
            );

            // The transport comes back. The daemon tick's drain delivers it,
            // and no producer had to be alive for that to happen.
            install_transport("test.outage", vec!["true"]);
            let drained = homeboy_core::notify_outbox::drain(
                chrono::Utc::now() + chrono::Duration::seconds(60),
            );
            assert_eq!(drained.delivered, 1, "{drained:?}");
            assert!(homeboy_core::notify_outbox::pending_entries().is_empty());
        });
    }

    #[test]
    fn a_queued_terminal_event_consumes_its_once_claim() {
        // Once the event is durable the outbox owns delivery. Leaving the claim
        // merely *held* would let its five-minute lease expire inside the
        // twenty-minute retry budget, and a second observer would announce the
        // same outcome a second time.
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("test.hold", vec!["false"]);
            set_default_transport("test.hold");
            cook_terminal(&report("succeeded", None), None, 0);
            assert!(
                !crate::agent_task_lifecycle::claim_cook_terminal_notification("cook-abc", "test")
                    .unwrap(),
                "a queued terminal event must not stay re-claimable",
            );
        });
    }

    #[test]
    fn resumed_terminal_delivery_uses_the_durable_route_and_stays_deduplicated() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("extension", vec!["true"]);
            seed_run_with_route("cook-resumed-attempt-1-aaaa", "secret-destination");
            let mut resumed = report("succeeded", None);
            resumed.cook_id = "cook-resumed".to_string();
            resumed.latest_run_id = Some("cook-resumed-attempt-1-aaaa".to_string());

            cook_terminal(&resumed, None, 0);
            cook_terminal(&resumed, None, 0);

            let delivery = latest_delivery("cook-resumed");
            assert_eq!(delivery["status"], "delivered");
            assert_eq!(delivery["route_classification"], "explicit");
            assert!(!delivery.to_string().contains("secret-destination"));
        });
    }

    #[test]
    fn a_cook_resumed_in_a_new_process_recovers_its_route_from_the_durable_record() {
        // #11115: `notification_route::current()` is a thread-local, so a
        // process that did not launch the cook (`cook --continue`, controller
        // adoption, claimed continuation) has none. Without the durable
        // fallback the whole arc degrades: Started/Progress are dropped and the
        // terminal event lands on the ambient default transport instead of the
        // thread that asked for this work. No route is bound here — that is the
        // resumed process.
        homeboy_core::test_support::with_isolated_home(|_| {
            seed_run_with_route("cook-resumed-attempt-1-aaaa", "thread-42");
            assert!(notification_route::current().is_none());

            let resolved = effective_route("cook-resumed-attempt-1-aaaa");

            assert_eq!(resolved, Some(route("thread-42")));
        });
    }

    #[test]
    fn an_explicitly_bound_route_still_wins_over_durable_metadata() {
        // The thread-local is the caller's live intent; durable metadata is
        // only the fallback for a process that has none. In-process thread hops
        // keep using capture/bind and must not be overridden.
        homeboy_core::test_support::with_isolated_home(|_| {
            seed_run_with_route("cook-bound-attempt-1-aaaa", "durable-thread");

            let resolved = notification_route::with_current(Some(route("live-thread")), || {
                effective_route("cook-bound-attempt-1-aaaa")
            });

            assert_eq!(resolved, Some(route("live-thread")));
        });
    }

    #[test]
    fn an_unroutable_run_is_silent_rather_than_a_cook_failure() {
        homeboy_core::test_support::with_isolated_home(|_| {
            assert!(effective_route("no-such-run").is_none());
            assert!(effective_route("").is_none());
        });
    }

    #[test]
    fn the_terminal_event_is_claimed_once_per_cook_not_once_per_attempt() {
        // The pre-existing exactly-once marker is a column on one `runs` row,
        // so it cannot dedupe an event whose subject is the cook. Durable route
        // rehydration lets a second process reach the same terminal boundary,
        // so the claim has to be keyed the way the event is.
        homeboy_core::test_support::with_isolated_home(|_| {
            let claim = |cook_id: &str| {
                crate::agent_task_lifecycle::claim_cook_terminal_notification(cook_id, "test")
                    .expect("claim")
            };

            assert!(claim("cook-once"));
            assert!(!claim("cook-once"));
            // Every other cook still gets its one delivery.
            assert!(claim("cook-other"));
            // An empty id is never claimable, so it can never suppress a real one.
            assert!(!claim(""));
        });
    }

    // -----------------------------------------------------------------------
    // W3-5 emitters
    // -----------------------------------------------------------------------

    fn batch_cell(
        cook_id: &str,
        exit_code: i32,
    ) -> crate::agent_task_service::AgentTaskCookBatchCellReport {
        crate::agent_task_service::AgentTaskCookBatchCellReport {
            cook_id: cook_id.to_string(),
            initial_run_id: format!("{cook_id}-attempt-1-aaaa"),
            status: if exit_code == 0 {
                "succeeded".to_string()
            } else {
                "durable_failure".to_string()
            },
            exit_code,
            result: None,
            error: None,
        }
    }

    fn batch_report(succeeded: usize, failed: usize) -> AgentTaskCookBatchReport {
        let mut cooks = Vec::new();
        for index in 0..succeeded {
            cooks.push(batch_cell(&format!("cook-green-{index}"), 0));
        }
        for index in 0..failed {
            cooks.push(batch_cell(&format!("cook-red-{index}"), 1));
        }
        AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id: "batch-1".to_string(),
            status: if failed == 0 {
                "succeeded".to_string()
            } else {
                "partial_failure".to_string()
            },
            total: succeeded + failed,
            queued: 0,
            running: 0,
            succeeded,
            failed,
            cancelled: 0,
            timed_out: 0,
            cooks,
        }
    }

    #[test]
    fn a_wave_summary_answers_how_many_are_green_broken_and_blocked() {
        // The gap W3-5 names: ten children produced ten unrelated cook
        // messages and never a wave summary, even though every total below was
        // already computed.
        let report = batch_report(7, 2);
        let payload = batch_terminal_payload(
            &report,
            "fanout-wave",
            Some("homeboy"),
            Some(BatchPortfolioCounts {
                ready: 7,
                blocked: 1,
                merged: 4,
            }),
            1,
        );
        assert_eq!(payload.kind, NotifyEventKind::NeedsAttention);
        let body = payload.render_body();
        assert!(body.contains("Children: 9"), "{body}");
        assert!(body.contains("Succeeded: 7"), "{body}");
        assert!(body.contains("Failed: 2"), "{body}");
        assert!(body.contains("Blocked: 1"), "{body}");
        assert!(body.contains("Merged: 4"), "{body}");
        // The failures are named, so the summary does not cost a second command.
        assert!(body.contains("cook-red-0"), "{body}");
        assert!(
            body.contains("homeboy agent-task fanout status fanout-wave"),
            "{body}"
        );
        let subject = payload.subject.clone().expect("subject");
        assert_eq!(subject.id, "fanout-wave");
        assert_eq!(subject.parent_id.as_deref(), Some("batch-1"));
    }

    #[test]
    fn a_clean_wave_is_completed_not_needs_attention() {
        let payload = batch_terminal_payload(&batch_report(3, 0), "batch-1", None, None, 0);
        assert_eq!(payload.kind, NotifyEventKind::Completed);
        // Addressed by its own batch id: no parent to point at.
        assert!(payload.subject.clone().unwrap().parent_id.is_none());
    }

    #[test]
    fn a_wave_that_exits_zero_with_failed_children_still_needs_attention() {
        // Exit code alone is not the signal. A wave can succeed as an
        // operation while leaving children that a human has to look at.
        let payload = batch_terminal_payload(&batch_report(8, 2), "fanout-wave", None, None, 0);
        assert_eq!(payload.kind, NotifyEventKind::NeedsAttention);
    }

    #[test]
    fn the_wave_terminal_event_is_claimed_once_per_wave() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_transport("test.wave", vec!["true"]);
            set_default_transport("test.wave");
            let report = batch_report(2, 0);

            batch_terminal(&report, "fanout-once", None, None, 0);
            batch_terminal(&report, "fanout-once", None, None, 0);

            assert_eq!(latest_delivery("fanout-once")["status"], "delivered");
            assert!(
                !crate::agent_task_lifecycle::claim_cook_terminal_notification(
                    "fanout-once",
                    "test"
                )
                .unwrap()
            );
        });
    }

    #[test]
    fn a_wave_recovers_its_route_from_a_child_when_it_has_none_of_its_own() {
        // `fanout resume` and a supervisor advance both run in a process that
        // never accepted `--notification-route`. Without the child fallback the
        // wave summary would land on the ambient default transport instead of
        // the thread that asked for the work.
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut report = batch_report(1, 0);
            report.cooks[0].initial_run_id = "cook-wave-attempt-1-aaaa".to_string();
            seed_run_with_route("cook-wave-attempt-1-aaaa", "wave-thread");

            assert!(notification_route::current().is_none());
            assert_eq!(
                batch_route("fanout-routeless", &report),
                Some(route("wave-thread"))
            );
        });
    }

    #[test]
    fn a_waiting_controller_says_what_it_is_waiting_on() {
        // `Waiting` emitted nothing at all, so an orchestrator had no signal
        // its controller had stalled.
        let payload = controller_waiting_payload(
            "loop-1",
            Some("review"),
            &[ControllerWaitSummary {
                wait_key: "controller:child:terminal".to_string(),
                event_type: "controller.terminal".to_string(),
                external_ref: Some("child-loop".to_string()),
            }],
        );
        assert_eq!(payload.kind, NotifyEventKind::Progress);
        let body = payload.render_body();
        assert!(body.contains("controller.terminal (child-loop)"), "{body}");
        assert!(
            body.contains("homeboy agent-task controller resume loop-1"),
            "{body}"
        );
    }

    #[test]
    fn a_controller_reaching_a_state_it_cannot_leave_needs_attention() {
        let escalated = controller_state_payload(
            "loop-1",
            None,
            AgentTaskLoopControllerState::Waiting,
            AgentTaskLoopControllerState::Escalated,
            Some("wait timed out"),
            0,
        );
        assert_eq!(escalated.kind, NotifyEventKind::NeedsAttention);
        assert!(escalated.render_body().contains("wait timed out"));

        let resumed = controller_state_payload(
            "loop-1",
            None,
            AgentTaskLoopControllerState::Waiting,
            AgentTaskLoopControllerState::Running,
            None,
            0,
        );
        assert_eq!(resumed.kind, NotifyEventKind::Progress);

        let done = controller_state_payload(
            "loop-1",
            None,
            AgentTaskLoopControllerState::Running,
            AgentTaskLoopControllerState::Completed,
            None,
            0,
        );
        assert_eq!(done.kind, NotifyEventKind::Completed);
    }

    #[test]
    fn a_controller_state_change_to_the_same_state_emits_nothing() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // A transport that always fails, so a delivered event is visible as
            // a queued outbox entry and a suppressed one is visible as silence.
            install_transport("test.noop", vec!["false"]);
            set_default_transport("test.noop");

            controller_state_changed(
                "loop-noop",
                None,
                AgentTaskLoopControllerState::Waiting,
                AgentTaskLoopControllerState::Waiting,
                None,
                1,
            );
            assert!(
                homeboy_core::notify_outbox::pending_entries().is_empty(),
                "a no-op transition must not emit",
            );

            // A real transition to a state the controller cannot leave does.
            controller_state_changed(
                "loop-noop",
                None,
                AgentTaskLoopControllerState::Waiting,
                AgentTaskLoopControllerState::Escalated,
                Some("wait timed out"),
                0,
            );
            assert_eq!(homeboy_core::notify_outbox::pending_entries().len(), 1);
        });
    }

    #[test]
    fn controller_progress_does_not_reach_an_ambient_default_transport() {
        // Same noise contract as cook: an operator who configured a default
        // transport asked for outcomes, not per-wait progress.
        assert!(!should_deliver(NotifyEventKind::Progress, false));
        assert!(should_deliver(NotifyEventKind::NeedsAttention, false));
    }

    #[test]
    fn controller_emission_is_inert_without_a_configured_transport() {
        homeboy_core::test_support::with_isolated_home(|_| {
            controller_state_changed(
                "loop-1",
                Some("review"),
                AgentTaskLoopControllerState::Running,
                AgentTaskLoopControllerState::Escalated,
                Some("stalled"),
                2,
            );
            controller_waiting(
                "loop-1",
                None,
                &[ControllerWaitSummary {
                    wait_key: "w".to_string(),
                    event_type: "controller.terminal".to_string(),
                    external_ref: None,
                }],
            );
            controller_action_failed("loop-1", None, "action-1", "run_gates", "gate failed");
            batch_terminal(&batch_report(1, 1), "fanout-inert", None, None, 1);
        });
    }
}
