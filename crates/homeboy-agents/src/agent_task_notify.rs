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
use homeboy_core::notify::{terminal_rejection_result, NotifyDelivery, NotifyEvent, NotifyOutcome};
use homeboy_core::notify_outbox::{self, NotifyOnceMarker, NotifyOutboxDisposition};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

use crate::agent_task_service::AgentTaskCookReport;

/// Generic subject class for every cook lifecycle notification.
const COOK_SUBJECT_KIND: &str = "agent_task_cook";

/// Attribution recorded on the cook-level exactly-once marker.
const COOK_TERMINAL_DELIVERED_BY: &str = "cook-controller";

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
/// resumed elsewhere — `cook-continue`, controller adoption, a claimed
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
/// launching process — `cook-continue`, controller adoption, a claimed
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
    } else if matches!(disposition, NotifyOutboxDisposition::Rejected { .. }) {
        "rejected"
    } else if matches!(outcome.delivery, NotifyDelivery::NotConfigured) {
        "not_configured"
    } else {
        "failed"
    };
    let outbox_entry_id = match disposition {
        NotifyOutboxDisposition::Queued { entry_id }
        | NotifyOutboxDisposition::Rejected { entry_id } => Some(entry_id.clone()),
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
        "rejection_reason": terminal_rejection_result(outcome.result.as_ref()).map(|rejection| rejection.reason_code),
        "validation_context": terminal_rejection_context(outcome.result.as_ref()),
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
        "reason_code",
        "validation_code",
        "validation_field",
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

fn terminal_rejection_context(result: Option<&Value>) -> Option<Value> {
    let rejection = terminal_rejection_result(result)?;
    let mut context = Map::new();
    if let Some(value) = rejection.validation_code {
        context.insert("validation_code".to_string(), Value::String(value));
    }
    if let Some(value) = rejection.validation_field {
        context.insert("validation_field".to_string(), Value::String(value));
    }
    (!context.is_empty()).then_some(Value::Object(context))
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
    previously_started: bool,
) {
    // Re-entry through an existing recipe reaches this boundary too. Claim by
    // Cook identity, not attempt or recipe creation: a crash before delivery
    // may retry, but a delivered (or queued) start must not be announced again.
    let Some(route) = effective_route(run_id) else {
        return;
    };
    let Ok(lifecycle_store) =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
    else {
        return;
    };
    if !lifecycle_store
        .claim_cook_started_notification(cook_id)
        .unwrap_or(false)
    {
        return;
    }
    // Cooks admitted before the started marker existed may already have sent
    // this announcement. A later durable progress phase proves the Cook passed
    // the start boundary; seed its marker instead of waking its old thread.
    if previously_started {
        let _ = lifecycle_store.confirm_cook_started_notification(cook_id);
        return;
    }
    let payload = started_payload(
        cook_id,
        run_id,
        title,
        component,
        base,
        max_attempts,
        provider,
    );
    let event = NotifyEvent::lifecycle(NotifyEventKind::Started, cook_id, "started")
        .with_title(format!("cook started — {title}"))
        .with_payload(payload);
    let dispatch = notify_outbox::dispatch_with_outbox(
        &event.with_route(Some(&route)),
        Some(NotifyOnceMarker::new(
            COOK_SUBJECT_KIND,
            cook_id,
            "cook-started",
        )),
    );
    match dispatch.disposition {
        NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
            let _ = lifecycle_store.confirm_cook_started_notification(cook_id);
        }
        NotifyOutboxDisposition::Dropped | NotifyOutboxDisposition::Rejected { .. } => {
            let _ = lifecycle_store.release_cook_started_notification_claim(cook_id);
        }
    }
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
/// The exit code is the process result; a requested PR also needs a durable
/// completion receipt before the notification may announce success.
fn terminal_payload(
    report: &AgentTaskCookReport,
    component: Option<&str>,
    exit_code: i32,
) -> NotifyPayload {
    let kind = if exit_code == 0 && report.completed_successfully() {
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
    // A primary route skipped before dispatch is an operator-visible condition,
    // not just an event-log line (#15881): one bounded line per skipped route.
    for note in &report.provider_route_notes {
        payload = payload.with_fact("Skipped route", note.clone());
    }

    if let Some(completion) = report.completion() {
        payload = payload.with_fact("Completion", completion.state);
    }
    if let Some(promotion) = report
        .attempts
        .iter()
        .rev()
        .find_map(|attempt| attempt.promotion.as_ref())
    {
        let gate_status = if promotion.deterministic_gates.is_empty() {
            "not_run"
        } else if promotion.finalization_eligible(false) {
            "passed"
        } else if promotion
            .deterministic_gates
            .iter()
            .any(|gate| gate.status == crate::agent_task_gate::AgentTaskGateStatus::Failed)
        {
            "failed"
        } else {
            "unverified"
        };
        payload = payload.with_fact("Gates", gate_status);
    } else if matches!(report.status.as_str(), "gate_failed" | "no_op_gate_failed") {
        payload = payload.with_fact("Gates", "failed");
    } else if report.status == "gates_deferred" {
        payload = payload.with_fact("Gates", "deferred");
    }

    // The pull request link is the single most useful thing a completed cook
    // can hand a reviewer, and it was previously discarded along with the rest
    // of the finalization report.
    if let Some(finalization) = &report.finalization {
        if crate::agent_task_service::cook_finalization_is_pr_receipt(finalization) {
            if let Some(url) = finalization.get("pr_url").and_then(|url| url.as_str()) {
                payload = payload.with_link(NotifyLink::new("pull request", url));
            }
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
    // Resolve one lifecycle store for the whole claim/record/confirm/release
    // protocol. These four steps are a single exactly-once transaction, and a
    // step that lands in a different home than the claim cannot fail while
    // getting it wrong: a confirm against the wrong root consumes an
    // eligibility nobody holds, and a release against the wrong root deletes
    // nothing and returns `Ok(())`, leaving the real claim to expire and be
    // re-delivered. Resolving once is what makes them agree (#7505).
    //
    // An unresolvable store is treated exactly as a lost claim, which is the
    // rule this function already applied to a failed claim.
    let Ok(lifecycle_store) =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
    else {
        return;
    };
    // Claim before building the payload, and treat a failed claim the same as a
    // lost one: the runner-direct and daemon-backstop paths also decline to
    // dispatch when their marker cannot be established, because a duplicated
    // terminal notification is worse than a missing one.
    let claimed = crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
        &lifecycle_store,
        &report.cook_id,
        COOK_TERMINAL_DELIVERED_BY,
    )
    .unwrap_or(false);
    if !claimed {
        return;
    }
    let succeeded = exit_code == 0 && report.completed_successfully();
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
            } else if report.status == "cancelled" {
                "cancelled"
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
    let _ = crate::agent_task_lifecycle::record_cook_terminal_notification_outcome_in_store(
        &lifecycle_store,
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
            let _ = crate::agent_task_lifecycle::confirm_cook_terminal_notification_in_store(
                &lifecycle_store,
                &report.cook_id,
                COOK_TERMINAL_DELIVERED_BY,
            );
        }
        // Nothing durable exists — no transport is configured, or the queue
        // could not be written. Release, exactly as before, so a later
        // terminal observer is eligible to try again.
        NotifyOutboxDisposition::Dropped | NotifyOutboxDisposition::Rejected { .. } => {
            let _ = crate::agent_task_lifecycle::release_cook_terminal_notification_claim_in_store(
                &lifecycle_store,
                &report.cook_id,
            );
        }
    }
}

pub(crate) fn cook_terminal_for_attempt(
    cook_id: &str,
    attempt_run_id: &str,
    status: &str,
    exit_code: i32,
) {
    let report = AgentTaskCookReport {
        schema: "homeboy/agent-task-cook/v1",
        cook_id: cook_id.to_string(),
        latest_run_id: Some(attempt_run_id.to_string()),
        history_run_ids: Vec::new(),
        invocation_run_ids: Vec::new(),
        status: status.to_string(),
        disposition: homeboy_core::cook_status::CookDisposition::Terminal,
        attempts: Vec::new(),
        finalization: None,
        intentional_no_change: None,
        selected_candidate: None,
        stop_reason: None,
        terminal_phase: None,
        terminal_failure_classification: None,
        primary_failure: None,
        moving_base_recovery: None,
        provider_route_notes: Vec::new(),
        failure_context: None,
        report_stores: None,
    };
    cook_terminal(&report, None, exit_code);
}

/// Build the continuation-failed payload. Separated from delivery so its
/// content is assertable without spawning a transport.
fn continuation_failed_payload(cook_id: &str, run_id: &str, diagnostic: &str) -> NotifyPayload {
    let mut subject = cook_subject(cook_id, run_id, None);
    subject.phase = Some("terminal_continuation".to_string());
    let payload = NotifyPayload::new(NotifyEventKind::NeedsAttention, subject)
        .with_fact("Status", "continuation_failed")
        .with_fact("Attempt", run_id.to_string())
        .with_fact("Reason", diagnostic.to_string())
        .with_action(
            NotifyAction::new(
                "continue",
                crate::agent_task_service::cook_recovery_command(
                    run_id,
                    &["cook-continue", run_id, "--rearm"],
                ),
            )
            .with_kind("repair"),
        );
    cook_actions(payload, cook_id)
}

/// The daemon's automatic terminal continuation for a Cook attempt stopped
/// for good (non-retryable failure or retry budget exhausted).
///
/// Before #15831 this stop was silent: the provider produced a candidate, the
/// continuation that would gate, finalize, and open the PR failed inside a
/// daemon work job, and the Cook sat at `candidate_recoverable` until someone
/// happened to run `cook-continue`. Delivered at most once per attempt through
/// its own marker, so it never consumes the Cook's terminal announcement.
pub fn cook_continuation_failed(cook_id: &str, run_id: &str, diagnostic: &str) {
    let Ok(lifecycle_store) =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
    else {
        return;
    };
    let claimed = lifecycle_store
        .claim_cook_continuation_failed_notification(cook_id, run_id)
        .unwrap_or(false);
    if !claimed {
        return;
    }
    let payload = continuation_failed_payload(cook_id, run_id, diagnostic);
    let event = NotifyEvent::lifecycle(payload.kind, cook_id, "continuation_failed")
        .with_title(format!("cook needs attention — {cook_id}"))
        .with_payload(payload);
    let route = effective_route(run_id);
    let dispatch = notify_outbox::dispatch_with_outbox(&event.with_route(route.as_ref()), None);
    match dispatch.disposition {
        NotifyOutboxDisposition::Delivered | NotifyOutboxDisposition::Queued { .. } => {
            let _ = lifecycle_store.confirm_cook_continuation_failed_notification(cook_id, run_id);
        }
        NotifyOutboxDisposition::Dropped | NotifyOutboxDisposition::Rejected { .. } => {
            let _ = lifecycle_store
                .release_cook_continuation_failed_notification_claim(cook_id, run_id);
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
    // One store for claim, confirm, and release — the same exactly-once
    // protocol `cook_terminal` runs, through the same marker (#7505).
    let Ok(lifecycle_store) =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
    else {
        return;
    };
    let claimed = crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
        &lifecycle_store,
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
            let _ = crate::agent_task_lifecycle::confirm_cook_terminal_notification_in_store(
                &lifecycle_store,
                run_id,
                RUN_RECONCILED_DELIVERED_BY,
            );
        }
        NotifyOutboxDisposition::Dropped | NotifyOutboxDisposition::Rejected { .. } => {
            let _ = crate::agent_task_lifecycle::release_cook_terminal_notification_claim_in_store(
                &lifecycle_store,
                run_id,
            );
        }
    }
}

#[cfg(test)]
#[path = "agent_task_notify_tests.rs"]
mod tests;
