use super::*;

#[test]
fn recovery_prefix_preserves_only_explicit_local_authority() {
    let identity = homeboy_lab_contract::ExecutionPlacementIdentity {
        repository: "fixture".to_string(),
        workspace: "fixture".to_string(),
        task: "task".to_string(),
        candidate: None,
        base: None,
    };
    let local = homeboy_lab_contract::ExecutionPlacementDecision::controller_local(
        "fixture",
        "v1",
        identity.clone(),
        homeboy_lab_contract::Placement::Local,
    );
    let automatic = homeboy_lab_contract::ExecutionPlacementDecision::controller_local(
        "fixture",
        "v1",
        identity,
        homeboy_lab_contract::Placement::Auto,
    );

    assert_eq!(
        cook_recovery_command_prefix_for_decision(Some(&local)),
        "homeboy --placement local"
    );
    assert_eq!(
        cook_recovery_command_prefix_for_decision(Some(&automatic)),
        "homeboy"
    );
    let lab = homeboy_lab_contract::ExecutionPlacementDecision::new(
        "fixture",
        "v1",
        homeboy_lab_contract::ExecutionPlacementIdentity {
            repository: "fixture".to_string(),
            workspace: "fixture".to_string(),
            task: "task".to_string(),
            candidate: None,
            base: None,
        },
        homeboy_lab_contract::Placement::Lab,
        homeboy_lab_contract::ExecutionPlacementRequirement::Lab,
        homeboy_lab_contract::EffectiveExecutionPlacement::Lab,
        Some(homeboy_lab_contract::ExecutionPlacementRunnerSelection {
            runner_id: "lab-fixture".to_string(),
            source: homeboy_lab_contract::RunnerSelectionSource::Explicit,
        }),
        homeboy_lab_contract::ExecutionPlacementFallback {
            local_allowed: false,
            reason: None,
        },
        homeboy_lab_contract::ExecutionPlacementOverrideAuthorization {
            authorized: false,
            authority: None,
        },
    );
    assert_eq!(
        cook_recovery_command_prefix_for_decision(Some(&lab)),
        "homeboy"
    );
    assert_eq!(
        cook_recover_command_with_prefix(
            &cook_recovery_command_prefix_for_decision(Some(&lab)),
            "cook-attempt-1",
        ),
        "homeboy agent-task recover cook-attempt-1"
    );
    assert_eq!(
        lab.runner
            .as_ref()
            .expect("provider runner remains recorded")
            .runner_id,
        "lab-fixture"
    );
}

fn report(status: &str, disposition: CookDisposition) -> AgentTaskCookReport {
    AgentTaskCookReport {
        schema: "homeboy/agent-task-cook/v1",
        cook_id: "cook-projection".to_string(),
        latest_run_id: None,
        history_run_ids: Vec::new(),
        invocation_run_ids: Vec::new(),
        status: homeboy_core::cook_status::CookStatus::from_status(status).unwrap(),
        disposition,
        attempts: Vec::new(),
        finalization: None,
        intentional_no_change: None,
        selected_candidate: None,
        stop_reason: None,
        terminal_phase: None,
        terminal_failure_classification: None,
        primary_failure: None,
        provider_route_notes: Vec::new(),
        failure_context: None,
        report_stores: None,
    }
}

#[test]
fn resource_guard_stop_becomes_the_cook_result_blocker() {
    let mut failed = AgentTaskRunResult {
        value: report("failed", CookDisposition::Terminal),
        exit_code: 1,
    };
    failed.value.failure_context = Some(AgentTaskCookFailureContext {
        recovery_step: None,
        cook_id: "cook".to_string(),
        latest_run_id: "run".to_string(),
        selected_run_id: None,
        selected_task_id: None,
        selected_candidate_sha: None,
        promotion_provenance: None,
        durable_recipe_ref: "homeboy://agent-task/cooks/cook/recipe".to_string(),
        lifecycle_state: "failed".to_string(),
        phase: "provider".to_string(),
        reason_code: "failed".to_string(),
        diagnostic: None,
        continuation_admission: None,
        blocking_claim: None,
        provider_budget_consumed: false,
        provider_executions_consumed: 0,
        recovery_legal: true,
        recovery_reason: "resume".to_string(),
        legal_actions: vec![AgentTaskCookRecoveryAction {
            action: "resume".to_string(),
            command: "homeboy agent-task recover run".to_string(),
        }],
        next_actions: vec![AgentTaskCookRecoveryAction {
            action: "resume".to_string(),
            command: "homeboy agent-task recover run".to_string(),
        }],
    });
    let record: agent_task_lifecycle::AgentTaskRunRecord =
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": "run",
            "plan_id": "plan",
            "state": "failed",
            "submitted_at": "2026-01-01T00:00:00Z",
            "plan_path": "/plan",
            "metadata": {
                "runner_id": "homeboy-lab",
                "runner_job_events": [{
                    "kind": "result",
                    "data": {
                        "exit_code": 1,
                        "metrics": {
                            "guard_violation": {
                                "reason": "process_count_limit_exceeded",
                                "message": "stopped",
                                "rss_bytes": 8,
                                "rss_limit_bytes": 16,
                                "process_count": 141,
                                "process_count_limit": 128
                            }
                        }
                    }
                }]
            }
        }))
        .expect("record");

    apply_resource_guard_stop(
        &mut failed,
        &record.metadata,
        &record.run_id,
        record.runner_id(),
    );

    assert_eq!(
        failed.value.terminal_failure_classification.as_deref(),
        Some("resource_guard.process_count_limit_exceeded")
    );
    let value = serde_json::to_value(&failed.value).expect("serialize");
    let context = failed.value.failure_context.expect("failure context");
    assert_eq!(
        context.reason_code,
        "resource_guard.process_count_limit_exceeded"
    );
    assert!(context.next_actions[0]
        .command
        .contains("homeboy runner set <runner> --json"));
    assert!(context.next_actions[0]
        .command
        .contains("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT"));
    assert!(!context.next_actions[0]
        .command
        .contains("agent-task recover"));
    assert_eq!(
        value["primary_failure"]["operation"],
        "resource_guard.process_count_limit_exceeded"
    );
    assert!(value["stop_reason"]
        .as_str()
        .unwrap_or_default()
        .contains("141"));
}

#[test]
fn terminality_and_retryability_follow_the_declared_disposition() {
    // `disposition` is the authority on terminality: a Cook a durable owner
    // still carries is neither finished nor retryable, whatever its status.
    for (disposition, terminal, retryable) in [
        (CookDisposition::Terminal, true, true),
        (CookDisposition::InFlight, false, false),
    ] {
        let report = report("durable_failure", disposition);
        let lifecycle = report.lifecycle();
        assert_eq!(lifecycle.lifecycle_status, ControlPlaneState::Failed);
        assert_eq!(lifecycle.terminal, terminal, "{disposition:?}");
        assert_eq!(lifecycle.retryable, retryable, "{disposition:?}");
    }

    let value = serde_json::to_value(report("durable_failure", CookDisposition::Terminal))
        .expect("serialize");
    assert_eq!(value["schema"], "homeboy/agent-task-cook/v1");
    assert_eq!(value["status"], "durable_failure");
    assert_eq!(value["disposition"], "terminal");
    assert!(value.get("latest_run_id").is_none());
    assert!(value.get("history_run_ids").is_none());
    assert!(value.get("failure_context").is_none());
}

#[test]
fn candidate_without_canonical_finalization_evidence_is_not_recoverable() {
    let mut candidate = report("candidate_recoverable", CookDisposition::Terminal);
    candidate.selected_candidate = Some(serde_json::json!({
        "run_id": "cook-projection-attempt-1",
        "incomplete": false,
        "selected_task_id": "task-1",
        "selected_candidate_sha": "patch-1",
    }));

    let serialized = serde_json::to_value(&candidate).expect("serialize");
    assert_eq!(
        serialized["status"], "candidate_recoverable",
        "closed status is preserved"
    );
    assert_eq!(serialized["completion"]["state"], "candidate_produced");
    assert_eq!(serialized["completion"]["pr_finalized"], false);
    assert!(serialized["completion"].get("next_action").is_none());
}

#[test]
fn only_a_finalization_receipt_with_a_pr_identity_finalizes_the_cook() {
    // Legacy receipts can establish that finalization was attempted, but
    // cannot establish publication without the PR identity.
    for (finalization, state, pr_finalized) in [
        (
            serde_json::json!({"status": "review_ready", "pr_number": 12162}),
            "pr_finalized",
            true,
        ),
        (
            serde_json::json!({"status": "review_ready"}),
            "candidate_produced",
            false,
        ),
    ] {
        let mut candidate = report("review_ready", CookDisposition::Terminal);
        candidate.selected_candidate = Some(serde_json::json!({
            "run_id": "cook-projection-attempt-1",
            "incomplete": false,
            "selected_task_id": "task-1",
            "selected_candidate_sha": "patch-1",
        }));
        candidate.finalization = Some(finalization);

        let completion = candidate.completion().expect("candidate completion");
        assert_eq!(completion.state, state);
        assert_eq!(completion.pr_finalized, pr_finalized);
        assert!(completion.next_action.is_none());
    }
}

#[test]
fn empty_unknown_and_conflicting_selections_never_produce_candidates() {
    for candidate in [
        serde_json::json!({"run_id": "run", "incomplete": false}),
        serde_json::json!({"run_id": "", "incomplete": false, "selected_task_id": "task", "selected_candidate_sha": "patch"}),
        serde_json::json!({"run_id": "run", "incomplete": true, "selected_task_id": "task", "selected_candidate_sha": "patch"}),
    ] {
        assert!(
            cook_completion(Some(&candidate), true, None, None).is_none(),
            "non-canonical selection cannot advertise a candidate or recovery"
        );
    }
}

#[test]
fn canonical_selection_only_uses_the_receipt_of_its_own_attempt() {
    let candidate = serde_json::json!({
        "cook_id": "cook",
        "run_id": "canonical-run",
        "incomplete": false,
        "selected_task_id": "task",
        "selected_candidate_sha": "patch",
    });
    let receipt = serde_json::json!({"status": "review_ready", "pr_number": 12291});

    assert_eq!(
        canonical_candidate_finalization(Some(&candidate), Some(&receipt), Some("canonical-run")),
        Some(receipt.clone())
    );
    assert!(
        canonical_candidate_finalization(
            Some(&candidate),
            Some(&receipt),
            Some("newer-non-substantive-run"),
        )
        .is_none(),
        "a receipt on an exact newer non-substantive attempt cannot finalize the older candidate"
    );
}

#[test]
fn a_pr_receipt_completes_the_cook_without_reopening_finalization_recovery() {
    let completion = cook_completion(
        None,
        true,
        Some(&serde_json::json!({"status": "review_ready", "pr_number": 12291})),
        None,
    )
    .expect("durable PR receipt completes Cook publication");

    assert_eq!(completion.state, "pr_finalized");
    assert!(completion.pr_finalized);
    assert!(completion.next_action.is_none());
}
