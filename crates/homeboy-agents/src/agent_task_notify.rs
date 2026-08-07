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
}
