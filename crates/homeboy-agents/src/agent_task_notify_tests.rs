//! Tests for the agent-task notification emitters.
//!
//! Mounted with `#[path]` from `agent_task_notify.rs` rather than living inside
//! it, following `cook.rs` -> `cook_tests.rs`. Keeping them inline pushed the
//! module past the audit's 1500-line `god_file` threshold, which would have been
//! a brand-new finding with no baseline entry.

use super::*;
use crate::agent_task_service::{AgentTaskCookFailureContext, AgentTaskCookRecoveryAction};

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> crate::agent_task_lifecycle::AgentTaskLifecycleStore {
    crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        .expect("lifecycle store")
}

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
        primary_failure: None,
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
            command: "homeboy agent-task cook-continue cook-abc".to_string(),
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
fn intentional_no_change_terminal_notifications_match_policy_outcome() {
    let accepted = terminal_payload(&report("intentional_no_change", None), None, 0);
    assert_eq!(accepted.kind, NotifyEventKind::Completed);
    assert!(accepted
        .render_body()
        .contains("Status: intentional_no_change"));

    let refused = terminal_payload(&report("no_candidate", None), None, 1);
    assert_eq!(refused.kind, NotifyEventKind::NeedsAttention);
    assert!(refused.render_body().contains("Status: no_candidate"));
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
        commands.contains(&"homeboy agent-task cook-continue cook-abc"),
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
            .filter(|action| action.label == recovery.action && action.command == recovery.command)
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
    homeboy_core::extension::catalog::save_manifest(&manifest).expect("install transport");
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
            !crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-abc",
                "test"
            )
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
            crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-abc",
                "test"
            )
            .unwrap()
        );
        crate::agent_task_lifecycle::release_cook_terminal_notification_claim_in_store(
            &test_lifecycle_store(),
            "cook-abc",
        )
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
fn rejected_before_attempt_records_safe_context_and_remains_resendable() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport(
            "test.reject-before-attempt",
            vec!["sh", "-c", "printf '%s\\n' '{\"schema\":\"homeboy/notification-transport-result/v1\",\"status\":\"rejected\",\"attempts\":0,\"terminal_rejection\":{\"schema\":\"homeboy/notification-transport-rejection/v1\",\"reason_code\":\"invalid_route\",\"validation_field\":\"route\"}}'; exit 1"],
        );
        set_default_transport("test.reject-before-attempt");

        cook_terminal(&report("durable_failure", None), None, 1);
        let rejected = latest_delivery("cook-abc");
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["error_class"], "transport_rejected");
        assert_eq!(rejected["rejection_reason"], "invalid_route");
        assert_eq!(rejected["validation_context"]["validation_field"], "route");
        assert!(homeboy_core::notify_outbox::pending_entries().is_empty());
        assert_eq!(homeboy_core::notify_outbox::dead_letter_entries().len(), 1);
        assert!(
            crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-abc",
                "test"
            )
            .unwrap()
        );
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
        let drained =
            homeboy_core::notify_outbox::drain(chrono::Utc::now() + chrono::Duration::seconds(60));
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
            !crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-abc",
                "test"
            )
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
    // process that did not launch the cook (`cook-continue`, controller
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
            crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                cook_id,
                "test",
            )
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
            !crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
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
