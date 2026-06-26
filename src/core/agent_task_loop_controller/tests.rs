use super::*;
use crate::test_support::with_isolated_home;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

#[test]
fn controller_persists_and_resumes_loop_state() {
    with_isolated_home(|_| {
        let mut record = create_controller("loop/demo", "generate", "v1").expect("created");
        let entity_id = record.upsert_entity("idea", "site-1", Vec::new(), Value::Null);
        write_controller(&record).expect("written");

        let loaded = load_controller("loop/demo").expect("loaded");

        assert_eq!(loaded.loop_id, "loop_demo");
        assert_eq!(loaded.phase, "generate");
        assert!(loaded.entities.contains_key(&entity_id));
    });
}

#[test]
fn dedupe_keys_prevent_duplicate_spawn_actions() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    let first = record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({ "task": "repair" }),
        },
        "finding emitted",
    );
    let second = record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({ "task": "repair" }),
        },
        "resume replay",
    );

    assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
    assert_eq!(record.dedupe_keys.len(), 1);
}

#[test]
fn status_diagnostics_flag_old_pending_actions() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.metadata = json!({ "runner_id": "lab-runner" });
    record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({ "task": "repair" }),
        },
        "finding emitted",
    );
    record.next_actions[0].created_at = "2026-06-09T00:00:00Z".to_string();

    let diagnostics = controller_status_diagnostics_with(
        &record,
        DateTime::parse_from_rfc3339("2026-06-11T00:00:01Z")
            .expect("now")
            .with_timezone(&Utc),
        |_| Ok(true),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.summary.pending_action_count, 1);
    assert_eq!(diagnostics.summary.stale_pending_action_count, 1);
    assert_eq!(diagnostics.summary.orphaned_pending_action_count, 0);
    let action = &diagnostics.pending_actions[0];
    assert_eq!(action.action_id, "action-1");
    assert_eq!(action.dedupe_key.as_deref(), Some("finding:abc:repair"));
    assert_eq!(action.runner_id.as_deref(), Some("lab-runner"));
    assert_eq!(action.age_seconds, Some(172801));
    assert!(action.stale);
    assert!(!action.orphaned);
    assert!(action
        .problems
        .contains(&"pending action is older than stale threshold".to_string()));
    assert!(action
        .recovery_commands
        .contains(&"homeboy agent-task controller run loop".to_string()));
    assert!(action
        .recovery_commands
        .contains(&"homeboy agent-task controller resume loop".to_string()));
}

#[test]
fn status_diagnostics_resume_commands_preserve_generic_dispatch_flags() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.metadata = json!({
        "dispatch_backend": "remote runner",
        "dispatch_selector": "provider'one",
        "dispatch_model": "gpt 5",
        "secret_env": ["SHOULD_NOT_APPEAR"]
    });
    record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({ "task": "repair" }),
        },
        "finding emitted",
    );
    record.next_actions[0].created_at = "2026-06-09T00:00:00Z".to_string();

    let diagnostics = controller_status_diagnostics_with(
        &record,
        DateTime::parse_from_rfc3339("2026-06-11T00:00:01Z")
            .expect("now")
            .with_timezone(&Utc),
        |_| Ok(true),
    )
    .expect("diagnostics");

    let commands = &diagnostics.pending_actions[0].recovery_commands;
    assert_eq!(
            commands[0],
            "homeboy agent-task controller run loop --dispatch-backend 'remote runner' --dispatch-selector 'provider'\\''one' --dispatch-model 'gpt 5'"
        );
    assert_eq!(
            commands[1],
            "homeboy agent-task controller resume loop --dispatch-backend 'remote runner' --dispatch-selector 'provider'\\''one' --dispatch-model 'gpt 5'"
        );
    assert!(!commands.join("\n").contains("SHOULD_NOT_APPEAR"));
}

#[test]
fn status_diagnostics_resume_commands_use_action_executor_selector() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({
                "mode": "run_plan",
                "plan": {
                    "tasks": [{
                        "executor": {
                            "backend": "generic-backend",
                            "selector": "generic-provider",
                            "model": "generic-model"
                        }
                    }]
                },
                "payload": { "selector": ".not-a-provider-selector" }
            }),
        },
        "finding emitted",
    );
    record.next_actions[0].created_at = "2026-06-09T00:00:00Z".to_string();

    let diagnostics = controller_status_diagnostics_with(
        &record,
        DateTime::parse_from_rfc3339("2026-06-11T00:00:01Z")
            .expect("now")
            .with_timezone(&Utc),
        |_| Ok(true),
    )
    .expect("diagnostics");

    assert_eq!(
            diagnostics.pending_actions[0].recovery_commands[1],
            "homeboy agent-task controller resume loop --dispatch-backend generic-backend --dispatch-selector generic-provider --dispatch-model generic-model"
        );
}

#[test]
fn status_diagnostics_flag_missing_referenced_run_records() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "finding:abc:repair".to_string(),
            entity_id: Some("finding:abc".to_string()),
            request: json!({
                "task": "repair",
                "lab": { "runner_id": "configured-lab" },
                "metadata": { "remote_run_id": "missing-run-123" }
            }),
        },
        "finding emitted",
    );
    record.next_actions[0].created_at = "2026-06-11T00:00:00Z".to_string();

    let diagnostics = controller_status_diagnostics_with(
        &record,
        DateTime::parse_from_rfc3339("2026-06-11T00:05:00Z")
            .expect("now")
            .with_timezone(&Utc),
        |run_id| Ok(run_id != "missing-run-123"),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.summary.pending_action_count, 1);
    assert_eq!(diagnostics.summary.stale_pending_action_count, 0);
    assert_eq!(diagnostics.summary.orphaned_pending_action_count, 1);
    let action = &diagnostics.pending_actions[0];
    assert_eq!(action.runner_id.as_deref(), Some("configured-lab"));
    assert_eq!(action.referenced_run_id.as_deref(), Some("missing-run-123"));
    assert_eq!(action.age_seconds, Some(300));
    assert!(!action.stale);
    assert!(action.orphaned);
    assert!(action
        .problems
        .contains(&"referenced run record is missing".to_string()));
}

#[test]
fn status_diagnostics_surface_missing_and_failed_acceptance_gates() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "verify", "v1");
    record.gate_bundles.push(AgentTaskGateBundle {
        bundle_id: "required-artifacts".to_string(),
        description: "required artifact contract".to_string(),
        checks: Vec::new(),
    });
    record.gate_bundles.push(AgentTaskGateBundle {
        bundle_id: "quality".to_string(),
        description: "quality contract".to_string(),
        checks: Vec::new(),
    });
    record.record_action(
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id: "quality".to_string(),
            entity_id: Some("artifact:summary".to_string()),
        },
        "run quality gate",
    );
    record.gate_results.push(AgentTaskGateBundleResult {
        result_id: "gate-result-1".to_string(),
        bundle_id: "quality".to_string(),
        entity_id: Some("artifact:summary".to_string()),
        run_id: None,
        status: AgentTaskGateBundleStatus::Failed,
        checks: Vec::new(),
        recorded_at: "2026-06-11T00:00:00Z".to_string(),
    });

    let diagnostics = controller_status_diagnostics_with(
        &record,
        DateTime::parse_from_rfc3339("2026-06-11T00:05:00Z")
            .expect("now")
            .with_timezone(&Utc),
        |_| Ok(true),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.summary.acceptance_gate_count, 2);
    assert_eq!(diagnostics.summary.missing_acceptance_gate_count, 1);
    assert_eq!(diagnostics.summary.failed_acceptance_gate_count, 1);
    assert!(diagnostics.acceptance_gates.iter().any(|gate| {
        gate.bundle_id == "required-artifacts"
            && gate.entity_id.is_none()
            && gate.status == AgentTaskLoopAcceptanceGateStatus::Missing
            && gate
                .problems
                .contains(&"acceptance gate has no recorded result".to_string())
    }));
    assert!(diagnostics.acceptance_gates.iter().any(|gate| {
        gate.bundle_id == "quality"
            && gate.entity_id.as_deref() == Some("artifact:summary")
            && gate.status == AgentTaskLoopAcceptanceGateStatus::Failed
            && gate.result_id.as_deref() == Some("gate-result-1")
            && gate
                .problems
                .contains(&"acceptance gate recorded a failed result".to_string())
    }));
}

#[test]
fn subcontroller_spawn_records_parent_visible_child_once() {
    let mut record = AgentTaskLoopControllerRecord::new("parent", "plan", "v1");
    let first = record.record_action(
        AgentTaskLoopPolicyAction::SpawnController {
            dedupe_key: "controller:child:plan".to_string(),
            loop_id: "child/controller".to_string(),
            entity_id: Some("goal:4216".to_string()),
            phase: "implement".to_string(),
            config_version: "nested-v1".to_string(),
            request: json!({ "issue": 4216 }),
        },
        "spawn child controller",
    );
    let second = record.record_action(
        AgentTaskLoopPolicyAction::SpawnSubloop {
            dedupe_key: "controller:child:plan".to_string(),
            loop_id: "child/controller".to_string(),
            entity_id: Some("goal:4216".to_string()),
            phase: "implement".to_string(),
            config_version: "nested-v1".to_string(),
            request: json!({ "issue": 4216 }),
        },
        "replayed child controller spawn",
    );

    assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
    assert_eq!(record.subcontrollers.len(), 1);
    let child = &record.subcontrollers[0];
    assert_eq!(child.loop_id, "child_controller");
    assert_eq!(child.parent_loop_id.as_deref(), Some("parent"));
    assert_eq!(child.parent_action_id.as_deref(), Some("action-1"));
    assert_eq!(child.entity_id.as_deref(), Some("goal:4216"));
}

#[test]
fn controller_status_satisfies_wait_when_child_reaches_terminal_state() {
    with_isolated_home(|_| {
        let mut parent = create_controller("parent-loop", "delegate", "v1").expect("parent");
        parent.record_action(
            AgentTaskLoopPolicyAction::SpawnController {
                dedupe_key: "controller:child-loop".to_string(),
                loop_id: "child-loop".to_string(),
                entity_id: Some("goal:4216".to_string()),
                phase: "implement".to_string(),
                config_version: "v1".to_string(),
                request: Value::Null,
            },
            "spawn child",
        );
        parent.record_action(
            AgentTaskLoopPolicyAction::WaitForController {
                loop_id: "child-loop".to_string(),
                entity_id: Some("goal:4216".to_string()),
                wait_key: None,
                terminal_states: Vec::new(),
            },
            "wait for child terminal state",
        );
        write_controller(&parent).expect("parent written");

        let mut child = create_controller("child-loop", "implement", "v1").expect("child");
        child.state = AgentTaskLoopControllerState::Completed;
        write_controller(&child).expect("child written");

        let refreshed = controller_status("parent-loop").expect("refreshed");

        assert_eq!(refreshed.state, AgentTaskLoopControllerState::Running);
        assert_eq!(
            refreshed.subcontrollers[0].state,
            Some(AgentTaskLoopControllerState::Completed)
        );
        assert_eq!(
            refreshed.waits[0].status,
            AgentTaskLoopWaitStatus::Satisfied
        );
        assert_eq!(
            refreshed.waits[0].satisfied_by_event_id.as_deref(),
            Some("controller-terminal:child-loop:Completed")
        );
    });
}

#[test]
fn external_events_satisfy_matching_waits_and_resume_controller() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
    record.record_action(
        AgentTaskLoopPolicyAction::WaitForEvent(AgentTaskLoopWait {
            wait_key: "pr-12-merged".to_string(),
            event_type: "github.pr.merged".to_string(),
            entity_id: Some("pr:12".to_string()),
            external_ref: Some("Extra-Chill/homeboy#12".to_string()),
            timeout_at: None,
            escalation_policy: Some("escalate".to_string()),
            status: AgentTaskLoopWaitStatus::Open,
            satisfied_by_event_id: None,
        }),
        "wait for human merge",
    );

    record.apply_event(AgentTaskLoopExternalEvent {
        event_id: "event-1".to_string(),
        event_type: "github.pr.merged".to_string(),
        event_key: Some("Extra-Chill/homeboy#12".to_string()),
        entity_id: Some("pr:12".to_string()),
        payload: Value::Null,
    });

    assert_eq!(record.state, AgentTaskLoopControllerState::Running);
    assert_eq!(record.waits[0].status, AgentTaskLoopWaitStatus::Satisfied);
    assert_eq!(
        record.waits[0].satisfied_by_event_id.as_deref(),
        Some("event-1")
    );
}

#[test]
fn review_feedback_routes_to_originating_attempt() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
    let action = record.record_action(
        AgentTaskLoopPolicyAction::RequestChanges {
            target_run_id: "run-repair-1".to_string(),
            feedback_id: Some("review-1".to_string()),
        },
        "review requested changes",
    );

    assert_eq!(
        action.dedupe_key.as_deref(),
        Some("feedback:run-repair-1:review-1")
    );
    assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
}

#[test]
fn own_pr_until_green_persists_generic_pr_lifecycle_state() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
    let request = AgentTaskPrOwnershipRequest {
        ownership_id: "run-123".to_string(),
        component_id: None,
        path: None,
        base: "main".to_string(),
        head: "fix/pr-owner".to_string(),
        pr_number: Some(42),
        pr_url: Some("https://github.com/Extra-Chill/homeboy/pull/42".to_string()),
        max_retries: 2,
        merge_required: true,
    };
    let action = record.record_action(
        AgentTaskLoopPolicyAction::OwnPrUntilGreen {
            ownership: request.clone(),
            entity_id: None,
        },
        "own PR after finalization",
    );

    assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(record.pr_ownerships.len(), 1);
    assert_eq!(
        record.pr_ownerships[0].state,
        AgentTaskPrOwnershipState::WaitingForChecks
    );
    assert_eq!(record.pr_ownerships[0].head, "fix/pr-owner");
    assert_eq!(record.pr_ownerships[0].pr_number, Some(42));
    assert!(record.entities.contains_key("pull_request:fix_pr-owner_42"));

    let serialized = serde_json::to_value(&action.action).expect("serialized action");
    assert_eq!(serialized["action"], "own_pr_until_green");
    assert_eq!(serialized["ownership"]["ownership_id"], "run-123");
}

#[test]
fn deterministic_policy_transition_can_start_pr_ownership_once() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "finalize", "v1");
    let ownership = AgentTaskPrOwnershipRequest {
        ownership_id: "branch:fix-pr-owner".to_string(),
        component_id: Some("homeboy".to_string()),
        path: None,
        base: "main".to_string(),
        head: "fix/pr-owner".to_string(),
        pr_number: Some(42),
        pr_url: None,
        max_retries: 3,
        merge_required: false,
    };
    let policy = AgentTaskLoopPolicy {
        policy_id: "own-pr-after-finalization".to_string(),
        transitions: vec![AgentTaskLoopTransition {
            transition_id: "finalized-branch".to_string(),
            from_phase: Some("finalize".to_string()),
            on_event_type: Some("agent_task.finalized".to_string()),
            when_json_path: Some("$.event.payload.branch".to_string()),
            actions: vec![AgentTaskLoopPolicyAction::OwnPrUntilGreen {
                ownership: ownership.clone(),
                entity_id: Some("pr:42".to_string()),
            }],
        }],
    };
    let event = AgentTaskLoopExternalEvent {
        event_id: "event-1".to_string(),
        event_type: "agent_task.finalized".to_string(),
        event_key: None,
        entity_id: None,
        payload: json!({ "branch": "fix/pr-owner" }),
    };

    let first = record.evaluate_policy(&policy, Some(&event));
    let second = record.evaluate_policy(&policy, Some(&event));

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(
        first[0].dedupe_key.as_deref(),
        Some("pr-ownership:branch:fix-pr-owner")
    );
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].status,
        AgentTaskLoopActionStatus::AlreadySatisfied
    );
    assert_eq!(record.pr_ownerships.len(), 1);
    assert_eq!(record.pr_ownerships[0].entity_id.as_deref(), Some("pr:42"));
}

#[test]
fn pr_ownership_red_checks_increment_until_retry_limit() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "review", "v1");
    let request = AgentTaskPrOwnershipRequest {
        ownership_id: "run-123".to_string(),
        component_id: None,
        path: None,
        base: "main".to_string(),
        head: "fix/pr-owner".to_string(),
        pr_number: Some(42),
        pr_url: None,
        max_retries: 2,
        merge_required: false,
    };

    let first = record.record_pr_ownership_status(
        &request,
        Some("pr:42".to_string()),
        AgentTaskPrOwnershipStatusUpdate {
            pr_number: Some(42),
            ci_state: Some("terminal_failed".to_string()),
            retry_count: 1,
            ..AgentTaskPrOwnershipStatusUpdate::default()
        },
    );
    let second = record.record_pr_ownership_status(
        &request,
        Some("pr:42".to_string()),
        AgentTaskPrOwnershipStatusUpdate {
            pr_number: Some(42),
            ci_state: Some("terminal_failed".to_string()),
            retry_count: 2,
            ..AgentTaskPrOwnershipStatusUpdate::default()
        },
    );

    assert_eq!(first.state, AgentTaskPrOwnershipState::ChangesRequested);
    assert_eq!(second.state, AgentTaskPrOwnershipState::RetryLimitReached);
    assert_eq!(record.pr_ownerships.len(), 1);
    assert_eq!(record.pr_ownerships[0].retry_count, 2);
}

#[test]
fn policy_transitions_can_match_structured_event_jsonpath() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "validate", "v1");
    let policy = AgentTaskLoopPolicy {
        policy_id: "validation-policy".to_string(),
        transitions: vec![AgentTaskLoopTransition {
            transition_id: "actionable-findings".to_string(),
            from_phase: Some("validate".to_string()),
            on_event_type: Some("validation.completed".to_string()),
            when_json_path: Some("$.event.payload.findings[?(@.actionable == true)]".to_string()),
            actions: vec![AgentTaskLoopPolicyAction::FanOut {
                dedupe_key: "validation:run-1:actionable-findings".to_string(),
                entity_ids: vec!["finding:a".to_string()],
                dynamic_artifact: None,
                group_by: Vec::new(),
                requires_non_empty: false,
                max_items: DEFAULT_FAN_OUT_MAX_ITEMS,
                fail_fast: true,
                request_template: json!({ "kind": "repair" }),
            }],
        }],
    };
    let actions = record.evaluate_policy(
        &policy,
        Some(&AgentTaskLoopExternalEvent {
            event_id: "event-1".to_string(),
            event_type: "validation.completed".to_string(),
            event_key: None,
            entity_id: None,
            payload: json!({
                "findings": [
                    { "id": "a", "actionable": true },
                    { "id": "b", "actionable": false }
                ]
            }),
        }),
    );

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].status, AgentTaskLoopActionStatus::Pending);
}

#[test]
fn runner_policy_prefers_declared_runner_when_available() {
    let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = AgentTaskLoopPolicyAction::SpawnTask {
        dedupe_key: "task:lab".to_string(),
        entity_id: None,
        request: json!({ "task": "repair", "runner": "homeboy-lab" }),
    };

    let decision = record.resolve_action_runner_policy(&action, |runner| {
        assert_eq!(runner, "homeboy-lab");
        AgentTaskLoopRunnerAvailability::Available
    });

    assert_eq!(
        decision.target,
        Some(AgentTaskLoopRunnerExecutionTarget::Runner(
            "homeboy-lab".to_string()
        ))
    );
    assert_eq!(decision.blocked_status, None);
    assert_eq!(decision.diagnostic, None);
}

#[test]
fn runner_policy_allows_explicit_local_fallback_when_runner_is_unavailable() {
    let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = AgentTaskLoopPolicyAction::SpawnTask {
        dedupe_key: "task:lab".to_string(),
        entity_id: None,
        request: json!({
            "task": "repair",
            "runner": "homeboy-lab",
            "local_fallback": "allowed"
        }),
    };

    let decision = record.resolve_action_runner_policy(&action, |_| {
        AgentTaskLoopRunnerAvailability::Unavailable {
            reason: "runner heartbeat is stale".to_string(),
        }
    });

    assert_eq!(
        decision.target,
        Some(AgentTaskLoopRunnerExecutionTarget::Local)
    );
    assert_eq!(decision.blocked_status, None);
    assert_eq!(
        decision
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("runner_unavailable_local_fallback_allowed")
    );
}

#[test]
fn runner_policy_denies_local_fallback_for_unavailable_required_runner() {
    let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = AgentTaskLoopPolicyAction::SpawnTask {
        dedupe_key: "task:lab".to_string(),
        entity_id: None,
        request: json!({
            "task": "repair",
            "runner": "homeboy-lab",
            "local_fallback": "denied"
        }),
    };

    let decision = record.resolve_action_runner_policy(&action, |_| {
        AgentTaskLoopRunnerAvailability::Unavailable {
            reason: "runner is not registered".to_string(),
        }
    });

    assert_eq!(decision.target, None);
    assert_eq!(
        decision.blocked_status,
        Some(AgentTaskLoopActionStatus::BlockedRunnerUnavailable)
    );
    let diagnostic = decision.diagnostic.expect("blocked diagnostic");
    assert_eq!(diagnostic.code, "blocked_runner_unavailable");
    assert_eq!(diagnostic.runner.as_deref(), Some("homeboy-lab"));
}

#[test]
fn runner_policy_blocks_remote_materialization_failures() {
    let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = AgentTaskLoopPolicyAction::FanOut {
        dedupe_key: "fanout:lab".to_string(),
        entity_ids: vec!["finding:1".to_string()],
        dynamic_artifact: None,
        group_by: Vec::new(),
        requires_non_empty: false,
        max_items: DEFAULT_FAN_OUT_MAX_ITEMS,
        fail_fast: true,
        request_template: json!({
            "task": "repair",
            "runner": "homeboy-lab",
            "local_fallback": false
        }),
    };

    let decision = record.resolve_action_runner_policy(&action, |_| {
        AgentTaskLoopRunnerAvailability::MaterializationBlocked {
            reason: "workspace snapshot could not be materialized remotely".to_string(),
        }
    });

    assert_eq!(decision.target, None);
    assert_eq!(
        decision.blocked_status,
        Some(AgentTaskLoopActionStatus::BlockedRemoteMaterialization)
    );
    assert_eq!(
        decision
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("blocked_remote_materialization")
    );
}

#[test]
fn runner_policy_blocks_local_execution_when_fallback_is_denied_without_runner() {
    let record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = AgentTaskLoopPolicyAction::RouteFinding {
        finding: AgentTaskLoopFindingPacket {
            finding_id: "finding-1".to_string(),
            severity: "high".to_string(),
            summary: "drift".to_string(),
            owner: None,
            source_transformer: None,
            reproduction_key: None,
            lineage: Vec::new(),
            payload: Value::Null,
        },
        dedupe_key: "finding:1".to_string(),
        entity_id: Some("finding:1".to_string()),
        request_template: json!({
            "task": "repair",
            "local_fallback": "denied"
        }),
    };

    let decision = record.resolve_action_runner_policy(&action, |_| {
        unreachable!("no runner should not probe runner availability")
    });

    assert_eq!(decision.target, None);
    assert_eq!(
        decision.blocked_status,
        Some(AgentTaskLoopActionStatus::BlockedLocalFallbackDenied)
    );
    assert_eq!(
        decision
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("blocked_local_fallback_denied")
    );
}

#[test]
fn runner_policy_block_persists_status_and_diagnostic() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "dispatch", "v1");
    let action = record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "task:lab".to_string(),
            entity_id: Some("finding:1".to_string()),
            request: json!({ "task": "repair", "runner": "homeboy-lab" }),
        },
        "policy matched",
    );

    let decision = record.resolve_action_runner_policy(&action.action, |_| {
        AgentTaskLoopRunnerAvailability::Unavailable {
            reason: "runner heartbeat is stale".to_string(),
        }
    });
    record
        .block_action_for_runner_policy(
            &action.action_id,
            decision.blocked_status.expect("blocked status"),
            decision.diagnostic.expect("blocked diagnostic"),
        )
        .expect("blocked action recorded");

    let persisted_action = record
        .next_actions
        .iter()
        .find(|candidate| candidate.action_id == action.action_id)
        .expect("action present");
    assert_eq!(
        persisted_action.status,
        AgentTaskLoopActionStatus::BlockedRunnerUnavailable
    );
    assert_eq!(persisted_action.diagnostics.len(), 1);
    assert_eq!(
        persisted_action.diagnostics[0].code,
        "blocked_runner_unavailable"
    );
    assert_eq!(record.history[0].event_type, "runner_policy.blocked");
}

#[test]
fn finding_packets_route_once_with_lineage() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "validate", "v1");
    let finding = AgentTaskLoopFindingPacket {
        finding_id: "finding-1".to_string(),
        severity: "high".to_string(),
        summary: "layout drift".to_string(),
        owner: Some("transformer".to_string()),
        source_transformer: Some("hero".to_string()),
        reproduction_key: Some("page:/#hero".to_string()),
        lineage: vec![AgentTaskLoopArtifactRef {
            uri: "artifact://candidate/site".to_string(),
            kind: Some("static_site_candidate".to_string()),
            role: None,
            label: Some("candidate".to_string()),
            semantic_key: None,
        }],
        payload: json!({ "selector": ".hero" }),
    };

    let first = record.route_finding_packet(
        finding.clone(),
        json!({ "task": "iterate-transformer", "finding": finding }),
    );
    let second = record.route_finding_packet(
        AgentTaskLoopFindingPacket {
            finding_id: "finding-1b".to_string(),
            reproduction_key: Some("page:/#hero".to_string()),
            ..match first.action.clone() {
                AgentTaskLoopPolicyAction::RouteFinding { finding, .. } => finding,
                _ => unreachable!("route finding action"),
            }
        },
        json!({ "task": "iterate-transformer" }),
    );

    assert_eq!(first.status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(second.status, AgentTaskLoopActionStatus::AlreadySatisfied);
    assert_eq!(first.dedupe_key.as_deref(), Some("finding:page:/#hero"));
    let entity = record.entities.get("finding:page___hero").expect("entity");
    assert_eq!(entity.state.as_deref(), Some("routed"));
    assert_eq!(entity.artifact_refs.len(), 1);
    assert_eq!(entity.provenance[0].uri, "artifact://candidate/site");
}

#[test]
fn candidate_patch_validation_promotes_passes_to_human_ready() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    let action = record.record_candidate_patch_validation(
        candidate_patch(1),
        AgentTaskLoopCandidateValidation {
            validation_id: "validation-1".to_string(),
            status: AgentTaskLoopCandidateValidationStatus::Passed,
            evidence: vec![artifact_ref("artifact://validation/report")],
            details: json!({ "passed": true }),
        },
        AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
    );

    assert_eq!(action.status, AgentTaskLoopActionStatus::Pending);
    assert_eq!(record.state, AgentTaskLoopControllerState::HumanReady);
    let entity = record
        .entities
        .get("candidate_patch:candidate-1")
        .expect("candidate entity");
    assert_eq!(entity.state.as_deref(), Some("validated"));
    assert!(entity.human_ready);
    assert_eq!(entity.artifact_refs.len(), 3);
}

#[test]
fn candidate_patch_validation_marks_retry_limit_stop_condition() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.record_candidate_patch_validation(
        candidate_patch(2),
        AgentTaskLoopCandidateValidation {
            validation_id: "validation-2".to_string(),
            status: AgentTaskLoopCandidateValidationStatus::Failed,
            evidence: vec![artifact_ref("artifact://validation/failure")],
            details: json!({ "passed": false }),
        },
        AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
    );

    assert_eq!(record.state, AgentTaskLoopControllerState::HumanReady);
    let entity = record
        .entities
        .get("candidate_patch:candidate-1")
        .expect("candidate entity");
    assert_eq!(entity.state.as_deref(), Some("retry_limit_reached"));
    assert!(entity.human_ready);
}

#[test]
fn candidate_patch_validation_keeps_failed_candidate_retryable() {
    let mut record = AgentTaskLoopControllerRecord::new("loop", "repair", "v1");
    record.record_candidate_patch_validation(
        candidate_patch(1),
        AgentTaskLoopCandidateValidation {
            validation_id: "validation-1".to_string(),
            status: AgentTaskLoopCandidateValidationStatus::Failed,
            evidence: vec![artifact_ref("artifact://validation/failure")],
            details: json!({ "passed": false }),
        },
        AgentTaskLoopCandidateLoopLimits { max_attempts: 2 },
    );

    assert_eq!(record.state, AgentTaskLoopControllerState::Running);
    let entity = record
        .entities
        .get("candidate_patch:candidate-1")
        .expect("candidate entity");
    assert_eq!(entity.state.as_deref(), Some("needs_retry"));
    assert!(!entity.human_ready);
}

#[test]
fn verify_commands_are_reusable_gate_bundle_checks() {
    let bundle = AgentTaskGateBundle::from_verify_commands(
        "candidate-gates",
        vec!["cargo test --lib".to_string()],
    );

    assert_eq!(bundle.bundle_id, "candidate-gates");
    assert_eq!(bundle.checks[0].kind, AgentTaskGateBundleCheckKind::Command);
    assert_eq!(bundle.checks[0].input["command"], json!("cargo test --lib"));
    assert!(bundle.checks[0].retryable);
}

fn candidate_patch(attempt: u32) -> AgentTaskLoopCandidatePatch {
    AgentTaskLoopCandidatePatch {
        candidate_id: "candidate-1".to_string(),
        patch: artifact_ref("artifact://patch/fix.diff"),
        finding_id: Some("finding-1".to_string()),
        worktree: Some("/tmp/homeboy-candidate".to_string()),
        attempt,
        lineage: vec![artifact_ref("artifact://finding/finding-1")],
    }
}

fn artifact_ref(uri: &str) -> AgentTaskLoopArtifactRef {
    AgentTaskLoopArtifactRef {
        uri: uri.to_string(),
        kind: Some("artifact".to_string()),
        role: None,
        label: None,
        semantic_key: None,
    }
}
