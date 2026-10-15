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
        provider_route_notes: Vec::new(),
        failure_context: None,
        report_stores: None,
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
            "status": "review_ready",
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
fn green_no_finalize_is_completed_without_a_pull_request_link() {
    let payload = terminal_payload(&report("green_no_finalize", None), None, 0);
    assert_eq!(payload.kind, NotifyEventKind::Completed);
    assert!(payload.links.is_empty());
    assert!(payload.render_body().contains("Status: green_no_finalize"));
}

/// #15881: a primary route skipped before dispatch is named in the terminal
/// notification as one bounded line per skipped route.
#[test]
fn terminal_notification_names_skipped_provider_routes() {
    let mut route_note_report = report("no_changes", None);
    route_note_report.provider_route_notes = vec![
        "Ran on openai/gpt-6-luna; anthropic/claude-sonnet-5-5 skipped: credentials_unusable (Refresh the provider-owned OpenCode credentials, then retry.)"
            .to_string(),
        "xai/grok-4.7 skipped: usage_capped (Wait for the provider usage cap to reset.)"
            .to_string(),
    ];
    let payload = terminal_payload(&route_note_report, None, 1);
    let body = payload.render_body();
    assert!(
        body.contains("Skipped route: Ran on openai/gpt-6-luna; anthropic/claude-sonnet-5-5 skipped: credentials_unusable"),
        "{body}"
    );
    assert!(
        body.contains("Skipped route: xai/grok-4.7 skipped: usage_capped"),
        "{body}"
    );

    // Without skips the notification gains no extra fact.
    let plain = terminal_payload(&report("succeeded", None), None, 0);
    assert!(!plain.render_body().contains("Skipped route:"),);
}

#[test]
fn cancelled_cook_cannot_render_as_success_without_failure_recovery_actions() {
    let mut cancelled = report("cancelled", None);
    cancelled.stop_reason = Some("Cook was cancelled: operator replacement".to_string());
    cancelled.terminal_phase = Some("cancellation".to_string());
    cancelled.terminal_failure_classification = Some("cancelled".to_string());

    let payload = terminal_payload(&cancelled, None, 1);

    assert_eq!(payload.kind, NotifyEventKind::NeedsAttention);
    let body = payload.render_body();
    assert!(body.contains("Status: cancelled"), "{body}");
    assert!(body.contains("operator replacement"), "{body}");
    assert!(!body.contains("cook succeeded"), "{body}");
    assert!(!cancelled.completed_successfully());
    assert!(!payload.actions.iter().any(
        |action| action.command.contains("resume") || action.command.contains("cook-continue")
    ));
}

#[test]
fn rotation_exhaustion_notification_preserves_the_terminal_cause() {
    let mut report = report("provider_failure", None);
    report.terminal_phase = Some("provider".to_string());
    report.terminal_failure_classification = Some("provider_rotation_exhausted".to_string());
    report.stop_reason = Some(
        "provider rotation exhausted without a candidate: xai/grok-4.6: provider_account_blocked; zai-coding-plan/glm-5.3: rate_limited; opencode-go/kimi-k3: stalled".to_string(),
    );

    let payload = terminal_payload(&report, None, 1);
    let body = payload.render_body();
    assert!(body.contains("Failure classification: provider_rotation_exhausted"));
    assert!(body.contains("xai/grok-4.6: provider_account_blocked"));
    assert!(body.contains("zai-coding-plan/glm-5.3: rate_limited"));
    assert!(body.contains("opencode-go/kimi-k3: stalled"));
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
fn reserve_pressure_notification_names_the_filesystem_and_scoped_inventory() {
    let mut failed = report("pre_execution_failure", None);
    failed.terminal_failure_classification = Some("capacity".to_string());
    failed.stop_reason = Some(
        "Filesystem reserve shortfall at /worktrees/new-task: 90 bytes available, 100 bytes reserved, 10 bytes short".to_string(),
    );
    let mut context = failure_context();
    context.phase = "worktree_capacity_admission".to_string();
    context.reason_code = "resource.capacity_reserve".to_string();
    context.legal_actions = vec![AgentTaskCookRecoveryAction {
        action: "inspect reclaimable artifacts across repository worktrees".to_string(),
        command:
            "homeboy cleanup artifacts --path /worktrees/repository --all-worktrees --merged-only"
                .to_string(),
    }];
    failed.failure_context = Some(context);

    let payload = terminal_payload(&failed, None, 1);
    let body = payload.render_body();

    assert!(body.contains("Filesystem reserve shortfall at /worktrees/new-task"));
    assert!(body.contains("Failure classification: capacity"));
    assert!(body.contains("Reason code: resource.capacity_reserve"));
    assert!(payload.actions.iter().any(|action| {
        action.command
            == "homeboy cleanup artifacts --path /worktrees/repository --all-worktrees --merged-only"
    }));
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
fn terminal_kind_requires_publication_evidence_even_with_zero_exit() {
    // A provider result cannot advertise publication without a PR receipt.
    let unknown = report("moving_base", None);
    assert_eq!(
        terminal_payload(&unknown, None, 1).kind,
        NotifyEventKind::NeedsAttention
    );
    assert_eq!(
        terminal_payload(&unknown, None, 0).kind,
        NotifyEventKind::NeedsAttention
    );
    let published = report(
        "succeeded",
        Some(serde_json::json!({
            "status": "review_ready",
            "pr_url": "https://example.test/pull/43",
        })),
    );
    assert_eq!(
        terminal_payload(&published, None, 0).kind,
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
        cook_started(
            "cook-abc", "run-1", "task", None, "main", 3, "claude", false,
        );
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

#[test]
fn started_announcement_is_durable_once_per_cook_across_recipe_reentry() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport("extension", vec!["false"]);
        seed_run_with_route("cook-once-attempt-1-aaaa", "original-thread");
        seed_run_with_route("cook-once-attempt-2-bbbb", "later-thread");
        assert!(notification_route::current().is_none());

        let start = |run_id| {
            cook_started(
                "cook-once",
                run_id,
                "task",
                None,
                "main",
                3,
                "claude",
                false,
            );
        };
        start("cook-once-attempt-1-aaaa");
        let pending = homeboy_core::notify_outbox::pending_entries();
        assert_eq!(pending.len(), 1, "first admission must announce");
        assert_eq!(pending[0].event.kind, NotifyEventKind::Started);
        assert_eq!(pending[0].event.route.as_deref(), Some("original-thread"));
        assert_eq!(pending[0].event.transport.as_deref(), Some("extension"));
        assert_eq!(pending[0].event.run_id, "cook-once");
        assert_eq!(
            pending[0].once_marker.as_ref().unwrap().subject_id,
            "cook-once"
        );

        // Re-entering the existing recipe, even with a different attempt and
        // route, cannot enqueue another start. The original route survives in
        // the pending event for the eventual drain.
        start("cook-once-attempt-1-aaaa");
        start("cook-once-attempt-2-bbbb");
        let pending = homeboy_core::notify_outbox::pending_entries();
        assert_eq!(pending.len(), 1, "a queued start consumes the claim");
        assert_eq!(pending[0].event.route.as_deref(), Some("original-thread"));

        // Started and terminal have independent Cook-scoped eligibility.
        assert!(
            crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-once",
                "test"
            )
            .unwrap()
        );
    });
}

#[test]
fn unrouted_admission_does_not_consume_the_started_announcement() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport("extension", vec!["false"]);
        set_default_transport("extension");
        cook_started(
            "cook-late-route",
            "missing",
            "task",
            None,
            "main",
            3,
            "claude",
            false,
        );
        assert!(homeboy_core::notify_outbox::pending_entries().is_empty());

        seed_run_with_route("cook-late-route-attempt-1-aaaa", "thread-42");
        cook_started(
            "cook-late-route",
            "cook-late-route-attempt-1-aaaa",
            "task",
            None,
            "main",
            3,
            "claude",
            false,
        );
        assert_eq!(homeboy_core::notify_outbox::pending_entries().len(), 1);
    });
}

#[test]
fn historical_cook_progress_seeds_start_marker_without_reannouncing() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport("extension", vec!["false"]);
        seed_run_with_route("historical-attempt", "old-thread");
        cook_started(
            "historical-cook",
            "historical-attempt",
            "old task",
            None,
            "main",
            3,
            "claude",
            true,
        );
        assert!(homeboy_core::notify_outbox::pending_entries().is_empty());
        // A second process sees the confirmed marker even if progress was
        // rewritten to durable_identity by the first continuation.
        cook_started(
            "historical-cook",
            "historical-attempt",
            "old task",
            None,
            "main",
            3,
            "claude",
            false,
        );
        assert!(homeboy_core::notify_outbox::pending_entries().is_empty());
    });
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
fn detached_attempt_terminal_preserves_cook_notification_for_final_report() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport("test.cook", vec!["true"]);
        set_default_transport("test.cook");
        let store = test_lifecycle_store();
        let cook_id = "cook-detached-notify";
        crate::agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(&store, cook_id)
            .expect("parent");
        let attempt_id = "cook-detached-notify-attempt-1-abcd1234";
        let plan = crate::agent_task_scheduler::AgentTaskPlan::new("notify-attempt", Vec::new());
        crate::agent_task_lifecycle::submit_plan(&plan, Some(attempt_id)).expect("attempt");
        crate::agent_task_lifecycle::record_cook_attempt_in_store(&store, cook_id, 1, attempt_id)
            .expect("index");
        assert!(
            crate::agent_task_lifecycle::cook_terminal_notification_outcome(cook_id)
                .expect("read outcome")
                .is_none(),
            "indexing an attempt is admission bookkeeping, not a terminal cook"
        );
        store
            .mutate_record(attempt_id, |record| {
                record.state = crate::agent_task_lifecycle::AgentTaskRunState::Failed;
                true
            })
            .expect("fail attempt");
        assert!(
            crate::agent_task_lifecycle::cook_terminal_notification_outcome(cook_id)
                .expect("read outcome")
                .is_none(),
            "provider outcome must not consume Cook's terminal notification"
        );
        let attempt = store.read_record(attempt_id).expect("raw provider record");
        assert_eq!(
            attempt.state,
            crate::agent_task_lifecycle::AgentTaskRunState::Failed
        );
        crate::agent_task_service::finalize_detached_cook_attempt(cook_id, attempt_id)
            .expect("detached finalizer");
        let delivery = latest_delivery(cook_id);
        assert_eq!(delivery["status"], "delivered");
        assert_eq!(delivery["transport"], "test.cook");
    });
}

#[test]
fn pending_gate_fix_and_finalization_rejection_do_not_announce_success() {
    let mut pending = report("gate_failed", None);
    pending.stop_reason = Some("gate fix pending".to_string());
    let gate = terminal_payload(&pending, None, 1);
    assert_eq!(gate.kind, NotifyEventKind::NeedsAttention);
    assert!(gate.render_body().contains("Gates: failed"));
    assert!(gate.render_body().contains("gate fix pending"));

    let mut rejected = report(
        "publication_rejected",
        Some(serde_json::json!({
            "status": "publication_rejected", "reason": "component finalization rejected"
        })),
    );
    rejected.stop_reason = Some("component finalization rejected".to_string());
    let finalization = terminal_payload(&rejected, None, 0);
    assert_eq!(finalization.kind, NotifyEventKind::NeedsAttention);
    assert!(finalization
        .render_body()
        .contains("component finalization rejected"));
    assert!(finalization.links.is_empty());
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

#[test]
fn progress_does_not_reach_an_ambient_default_transport() {
    // An operator who configured a default transport asked for outcomes, not
    // per-attempt progress.
    assert!(!should_deliver(NotifyEventKind::Progress, false));
    assert!(should_deliver(NotifyEventKind::NeedsAttention, false));
}

#[test]
fn batch_emission_is_inert_without_a_configured_transport() {
    homeboy_core::test_support::with_isolated_home(|_| {});
}

#[test]
fn continuation_failed_payload_names_the_stop_and_the_rearm_command() {
    let payload = continuation_failed_payload(
        "cook-stalled",
        "cook-stalled-attempt-1-abcd1234",
        "cook continuation retry budget exhausted",
    );
    assert_eq!(payload.kind, NotifyEventKind::NeedsAttention);
    let body = payload.render_body();
    assert!(body.contains("continuation_failed"));
    assert!(body.contains("cook continuation retry budget exhausted"));
    assert!(payload.actions.iter().any(|action| action
        .command
        .contains("cook-continue cook-stalled-attempt-1-abcd1234 --rearm")));
}

/// A failed automatic terminal continuation is announced once per attempt and
/// never consumes the Cook's own terminal announcement (#15831).
#[test]
fn failed_terminal_continuation_is_announced_once_without_consuming_cook_terminal() {
    homeboy_core::test_support::with_isolated_home(|_| {
        install_transport("test.cook", vec!["true"]);
        set_default_transport("test.cook");
        let store = test_lifecycle_store();
        let cook_id = "cook-continuation-failed";
        let run_id = "cook-continuation-failed-attempt-1-abcd1234";

        cook_continuation_failed(cook_id, run_id, "cook continuation retry budget exhausted");
        assert!(
            !store
                .claim_cook_continuation_failed_notification(cook_id, run_id)
                .unwrap(),
            "a delivered announcement consumes its once marker"
        );
        assert!(
            store
                .claim_cook_continuation_failed_notification(cook_id, "another-attempt")
                .unwrap(),
            "the marker is scoped to one attempt"
        );
        assert!(
            crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &store, cook_id, "test"
            )
            .unwrap(),
            "the Cook terminal announcement stays eligible"
        );
    });
}

#[test]
fn undelivered_continuation_failure_stays_eligible() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = test_lifecycle_store();
        cook_continuation_failed("cook-x", "cook-x-attempt-1", "failed");
        assert!(store
            .claim_cook_continuation_failed_notification("cook-x", "cook-x-attempt-1")
            .unwrap());
    });
}

/// A transport that records each delivery, so a test can tell an announcement
/// that was withheld from one that reached the operator.
fn install_counting_transport(home: &std::path::Path) -> std::path::PathBuf {
    let marker = home.join("deliveries");
    let script = format!("echo delivered >> '{}'", marker.display());
    install_transport("test.cook", vec!["sh", "-c", &script]);
    set_default_transport("test.cook");
    marker
}

fn deliveries(marker: &std::path::Path) -> usize {
    std::fs::read_to_string(marker)
        .map(|raw| raw.lines().count())
        .unwrap_or(0)
}

#[test]
fn needs_attention_for_a_resolved_tracker_is_recorded_but_not_delivered() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let marker = install_counting_transport(home.path());
        let tracker = "https://github.com/o/r/issues/628";

        cook_terminal_with(&report("durable_failure", None), None, 1, |cook_id| {
            assert_eq!(cook_id, "cook-abc");
            Some(tracker.to_string())
        });

        assert_eq!(deliveries(&marker), 0, "resolved work must not page anyone");
        let outcome = latest_delivery("cook-abc");
        assert_eq!(outcome["status"], "suppressed");
        assert_eq!(outcome["suppression_reason"], "tracker_resolved");
        assert_eq!(outcome["tracker"], tracker);
        assert_eq!(outcome["event_kind"], "needs_attention");
        assert!(cook_terminal_announced("cook-abc"));
        assert!(
            !crate::agent_task_lifecycle::claim_cook_terminal_notification_in_store(
                &test_lifecycle_store(),
                "cook-abc",
                "test"
            )
            .unwrap(),
            "a withheld announcement still consumes the once marker"
        );
    });
}

#[test]
fn needs_attention_for_an_open_tracker_is_delivered() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let marker = install_counting_transport(home.path());
        cook_terminal_with(&report("durable_failure", None), None, 1, |_| None);
        assert_eq!(deliveries(&marker), 1);
        assert_eq!(latest_delivery("cook-abc")["status"], "delivered");
        assert!(cook_terminal_announced("cook-abc"));
    });
}

#[test]
fn success_is_announced_without_asking_the_tracker() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let marker = install_counting_transport(home.path());
        let finalization = serde_json::json!({
            "status": "review_ready",
            "pr_url": "https://github.com/o/r/pull/9",
            "pr_action": "created",
        });
        cook_terminal_with(&report("succeeded", Some(finalization)), None, 0, |_| {
            panic!("a success never consults the tracker")
        });
        assert_eq!(deliveries(&marker), 1);
    });
}

#[test]
fn continuation_failure_for_a_resolved_tracker_is_not_delivered() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let marker = install_counting_transport(home.path());
        let store = test_lifecycle_store();
        let (cook_id, run_id) = ("cook-resolved", "cook-resolved-attempt-1-abcd1234");

        cook_continuation_failed_with(cook_id, run_id, "internal.io_error", |_| {
            Some("https://github.com/o/r/issues/628".to_string())
        });

        assert_eq!(deliveries(&marker), 0);
        assert!(
            !store
                .claim_cook_continuation_failed_notification(cook_id, run_id)
                .unwrap(),
            "the withheld announcement consumes its once marker"
        );

        cook_continuation_failed_with(cook_id, "cook-resolved-attempt-2", "failed", |_| None);
        assert_eq!(deliveries(&marker), 1, "open work still pages");
    });
}

#[test]
fn terminal_announced_ignores_undelivered_outcomes() {
    homeboy_core::test_support::with_isolated_home(|_| {
        assert!(!cook_terminal_announced("cook-abc"), "no outcome yet");
        cook_terminal(&report("durable_failure", None), None, 1);
        assert_eq!(latest_delivery("cook-abc")["status"], "not_configured");
        assert!(
            !cook_terminal_announced("cook-abc"),
            "an undelivered terminal leaves the continuation alert as the only signal"
        );
    });
}
