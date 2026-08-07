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

// ---------------------------------------------------------------------------
// Daemon reconcile (W3-10)
// ---------------------------------------------------------------------------

/// Generic subject class for a durable agent-task run.
const RUN_SUBJECT_KIND: &str = "agent_task_run";

/// Attribution recorded on the reconcile-level exactly-once marker.
const RUN_RECONCILED_DELIVERED_BY: &str = "daemon-reconcile";

fn run_reconciled_payload(
    run_id: &str,
    state: &str,
    liveness: &str,
    reason: Option<&str>,
) -> NotifyPayload {
    NotifyPayload::new(
        NotifyEventKind::NeedsAttention,
        NotifySubject::new(RUN_SUBJECT_KIND, run_id),
    )
    .with_fact("State", state)
    .with_fact("Liveness", liveness)
    .with_optional_fact("Reason", reason.map(str::to_string))
    .with_action(
        NotifyAction::new("status", format!("homeboy agent-task status {run_id}"))
            .with_kind("show"),
    )
    .with_action(
        NotifyAction::new("diagnose", format!("homeboy agent-task diagnose {run_id}"))
            .with_kind("repair"),
    )
}

/// The daemon reconciled an orphaned `running` record.
///
/// This is terminal for the run — its owner is gone and the record has been
/// cancelled — so it reaches a configured default transport. Before the
/// daemon drove reconciliation this outcome had no notification at all,
/// because it had no producer: the record simply stayed `running` until
/// somebody ran the command by hand.
///
/// Claimed once per run through the same marker the cook terminal event uses,
/// so a reconcile that repeats across daemon restarts announces once.
pub fn run_reconciled(run_id: &str, state: &str, liveness: &str, reason: Option<&str>) {
    let claimed = crate::agent_task_lifecycle::claim_cook_terminal_notification(
        run_id,
        RUN_RECONCILED_DELIVERED_BY,
    )
    .unwrap_or(false);
    if !claimed {
        return;
    }
    let payload = run_reconciled_payload(run_id, state, liveness, reason);
    let event = NotifyEvent::lifecycle(NotifyEventKind::NeedsAttention, run_id, state)
        .with_title(format!("run reconciled — {run_id}"))
        .with_payload(payload);
    let route = effective_route(run_id);
    let dispatch = notify_outbox::dispatch_with_outbox(
        &event.with_route(route.as_ref()),
        Some(NotifyOnceMarker::new(
            RUN_SUBJECT_KIND,
            run_id,
            RUN_RECONCILED_DELIVERED_BY,
        )),
    );
    match dispatch.disposition {
        NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
            let _ = crate::agent_task_lifecycle::confirm_cook_terminal_notification(
                run_id,
                RUN_RECONCILED_DELIVERED_BY,
            );
        }
        NotifyOutboxDisposition::Dropped => {
            let _ = crate::agent_task_lifecycle::release_cook_terminal_notification_claim(run_id);
        }
    }
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
#[path = "agent_task_notify_tests.rs"]
mod tests;
