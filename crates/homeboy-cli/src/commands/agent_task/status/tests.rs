use super::*;
use homeboy::core::Error;

#[test]
fn compact_status_deduplicates_retry_rotation_artifacts_by_identity() {
    // A retry can rotate providers while replaying the same durable refs.
    let repeated_retry_fixture = json!([
        {"id":"transcript-1","kind":"transcript","path":"retry-1/transcript"},
        {"id":"patch-1","kind":"patch","path":"retry-1/patch"},
        {"id":"transcript-1","kind":"transcript","path":"retry-2/transcript"},
        {"id":"patch-1","kind":"patch","path":"retry-2/patch"},
        {"id":"progress-1","kind":"progress","path":"retry-2/progress"},
    ]);

    let compact = compact_items(Some(&repeated_retry_fixture), &["id", "kind", "path"]);
    assert_eq!(compact.as_array().unwrap().len(), 3);
    assert_eq!(compact_duplicate_count(Some(&repeated_retry_fixture)), 2);
    // Full evidence remains available from the unmodified source through
    // `runs artifacts`; compacting is a presentation-only projection.
    assert_eq!(repeated_retry_fixture.as_array().unwrap().len(), 5);
    assert_eq!(compact[0]["id"], "transcript-1");
    assert_eq!(compact[1]["id"], "patch-1");
    assert_eq!(compact[2]["id"], "progress-1");
}

#[test]
fn typed_provider_timeout_precedes_unparsed_provider_stream_error() {
    let diagnostics = ranked_diagnostics(vec![
        CollectedDiagnostic {
            task_id: "task-a".to_string(),
            class: "provider.process_stream".to_string(),
            message: "provider error: stderr was truncated".to_string(),
            source: "hydrated_process_stream".to_string(),
            data: Value::Null,
        },
        CollectedDiagnostic {
            task_id: "task-a".to_string(),
            class: "agent_task.provider_liveness_timeout".to_string(),
            message: "provider timed out after producing a recoverable candidate".to_string(),
            source: "diagnostics".to_string(),
            data: json!({"failure_classification": "timeout"}),
        },
    ]);

    assert_eq!(
        diagnostics
            .first()
            .map(|diagnostic| diagnostic.class.as_str()),
        Some("agent_task.provider_liveness_timeout")
    );
}

/// `cook --wait` on an unmaterialized admission must not end while the
/// admission's replay is materializing the attempt: the recipe-only window
/// is not terminal until the admission itself is.
/// The provider handing back its patch (`candidate_recoverable` /
/// `patch_available`) is not a Cook verdict while the Cook's driver is
/// still alive to apply it and run the gates: a `cook --wait` that
/// stopped there reported a failed step for a Cook still verifying.
#[test]
fn a_live_cook_driver_keeps_an_intermediate_recoverable_state_open() {
    crate::test_support::with_isolated_home(|_| {
        use fs4::fs_std::FileExt;
        let handoff = (
            json!({
                "state": "candidate_recoverable",
                "phase": "provider_start",
                "mission": "cook-live",
                "run": "cook-live-attempt-1",
            }),
            1,
        );
        let poller = StatusPoller {
            args: StatusArgs {
                run_id: "cook-live".to_string(),
                ..Default::default()
            },
            await_materialization: true,
            recovery_step: false,
        };
        assert!(
            poller.is_terminal(&handoff),
            "no driver: the state is the answer"
        );

        let driver_dir = homeboy::core::paths::homeboy_data()
            .expect("data root")
            .join("agent-task-cooks")
            .join("cook-live");
        std::fs::create_dir_all(&driver_dir).expect("driver lock directory");
        let driver = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(driver_dir.join("driver.lock"))
            .expect("driver lock");
        assert!(driver.try_lock_exclusive().expect("hold driver lock"));
        assert!(
            !poller.is_terminal(&handoff),
            "a live driver still owns the Cook"
        );

        drop(driver);
        assert!(
            poller.is_terminal(&handoff),
            "the driver exited on this state"
        );
    });
}

#[test]
fn a_recipe_only_attempt_ends_a_cook_wait_only_once_its_admission_is_terminal() {
    crate::test_support::with_isolated_home(|_| {
        let recipe_only = (
            json!({ "status": "recipe_only_recovery_required", "run_id": "cook-a-attempt-1" }),
            0,
        );
        let poller = |run_id: &str, await_materialization: bool| StatusPoller {
            args: StatusArgs {
                run_id: run_id.to_string(),
                ..Default::default()
            },
            await_materialization,
            recovery_step: false,
        };
        agent_task_lifecycle::submit_plan(
            &AgentTaskPlan::new("cook-a", Vec::new()),
            Some("cook-a"),
        )
        .expect("queued admission");
        assert!(!poller("cook-a", true).is_terminal(&recipe_only));
        // A plain status watch reports the recipe-only state as its answer.
        assert!(poller("cook-a", false).is_terminal(&recipe_only));
        // An admission with no durable record cannot be waited on.
        assert!(poller("cook-absent", true).is_terminal(&recipe_only));
        agent_task_lifecycle::cancel_run("cook-a", Some("admission abandoned"))
            .expect("terminal admission");
        assert!(poller("cook-a", true).is_terminal(&recipe_only));
    });
}

#[test]
fn stale_generic_lab_replay_status_has_no_executable_action() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("workspace.txt"), "recorded")
            .expect("write workspace");
        let recorded_identity =
            homeboy::runner::generic_lab_replay_artifact_identity(workspace.path())
                .expect("record replay artifact identity");
        std::fs::write(workspace.path().join("workspace.txt"), "current")
            .expect("change workspace");
        let mut plan = AgentTaskPlan::new("stale-generic-lab-replay", Vec::new());
        plan.metadata["generic_lab_command_replay"] = json!({
            "schema": "homeboy/generic-lab-command-replay/v1",
            "normalized_args": ["homeboy", "bench"],
            "materialization": {
                "canonical_root": workspace.path(),
                "content_identity": recorded_identity,
            },
        });
        agent_task_lifecycle::submit_plan(&plan, Some("stale-generic-lab-replay"))
            .expect("persist generic replay");
        agent_task_lifecycle::record_pre_execution_failure(
            "stale-generic-lab-replay",
            &plan,
            "lab_daemon_admission",
            &Error::internal_unexpected("daemon unavailable").with_retryable(true),
        )
        .expect("record retryable failure");

        let record = agent_task_lifecycle::reconcile_status("stale-generic-lab-replay")
            .expect("load replay record");
        let retry = retry_replay_action(&record);

        assert_eq!(retry.readiness, "ready");
        assert!(retry.action.is_some());
    });
}

#[test]
fn cook_owned_generic_lab_replay_status_has_no_lab_action() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut plan = AgentTaskPlan::new("cook-owned-generic-lab-replay", Vec::new());
        plan.metadata["generic_lab_command_replay"] = json!({
            "schema": "homeboy/generic-lab-command-replay/v1",
            "normalized_args": ["homeboy", "bench"],
            "materialization": {
                "canonical_root": workspace.path(),
                "content_identity": "snapshot:recorded",
            },
        });
        agent_task_lifecycle::submit_plan(&plan, Some("cook-owned-generic-lab-replay"))
            .expect("persist generic replay");
        agent_task_lifecycle::rewrite_record_for_test("cook-owned-generic-lab-replay", |record| {
            record.metadata["cook_id"] = json!("cook-owned-generic-lab-replay");
        })
        .expect("mark replay Cook-owned");
        agent_task_lifecycle::record_pre_execution_failure(
            "cook-owned-generic-lab-replay",
            &plan,
            "lab_daemon_admission",
            &Error::internal_unexpected("daemon unavailable").with_retryable(true),
        )
        .expect("record retryable failure");

        let record = agent_task_lifecycle::reconcile_status("cook-owned-generic-lab-replay")
            .expect("load replay record");
        let retry = retry_replay_action(&record);

        assert_eq!(retry.readiness, "unavailable");
        assert!(retry.action.is_none());
        assert!(retry
            .reason
            .unwrap()
            .contains("Cook-owned generic Lab replay"));
    });
}

#[test]
fn placement_rewrite_threads_one_resolved_prefix_through_nested_commands() {
    let mut value = json!({
        "command": "homeboy agent-task status cook-1",
        "nested": ["homeboy agent-task recover cook-1"],
    });

    preserve_controller_owner_placement_with_prefix(
        &mut value,
        "cook-1",
        "homeboy --placement local",
    );

    assert_eq!(
        value["command"],
        "homeboy --placement local agent-task status cook-1"
    );
    assert_eq!(
        value["nested"][0],
        "homeboy --placement local agent-task recover cook-1"
    );
}

#[test]
fn actionable_diagnostics_outrank_a_successful_provider_process_exit() {
    let diagnostics = ranked_diagnostics(vec![
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "provider.process_exit".to_string(),
            message: "Pi CLI exited with status 0".to_string(),
            source: "diagnostics".to_string(),
            data: Value::Null,
        },
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_task.provider_outcome_contract_violation".to_string(),
            message: "Provider result violates the required output contract.".to_string(),
            source: "diagnostics".to_string(),
            data: Value::Null,
        },
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_task.required_output_missing".to_string(),
            message: "Required output review_form was not produced.".to_string(),
            source: "diagnostics".to_string(),
            data: Value::Null,
        },
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "Tool 'grep' was denied by the external-directory permission policy."
                .to_string(),
            source: "diagnostics".to_string(),
            data: json!({
                "tool": "grep",
                "permission": "external_directory_read",
                "requested_path": "/Users/chubes/Developer/homeboy",
                "canonical_path": "/Users/chubes/Developer/homeboy@fix-11827"
            }),
        },
    ]);

    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.class.as_str())
            .collect::<Vec<_>>(),
        vec![
            "agent_tool.command_denied",
            "agent_task.required_output_missing",
            "agent_task.provider_outcome_contract_violation",
            "provider.process_exit",
        ]
    );
    assert_eq!(
        collected_diagnostic_value(diagnostics[0].clone())["details"]["canonical_path"],
        "/Users/chubes/Developer/homeboy@fix-11827"
    );
}

#[test]
fn recorded_gate_failure_outranks_process_stream_text() {
    let diagnostics = ranked_diagnostics(vec![
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "provider.process_stream".to_string(),
            message: "Verifying lock file contents can be installed on current platform; validating schema".to_string(),
            source: "hydrated_process_stream".to_string(),
            data: Value::Null,
        },
        CollectedDiagnostic {
            task_id: "promotion".to_string(),
            class: "agent_task.promotion_gate_failed".to_string(),
            message: "deterministic gate `gate-2` failed".to_string(),
            source: "current_lifecycle".to_string(),
            data: Value::Null,
        },
    ]);

    assert_eq!(diagnostics[0].class, "agent_task.promotion_gate_failed");
}

/// A recorded controller or gate verdict is the root cause, never the
/// provider's echoed prompt, whatever words that prompt happens to contain.
#[test]
fn recorded_controller_and_gate_verdicts_outrank_an_echoed_prompt() {
    let prompt = CollectedDiagnostic {
        task_id: "cook".to_string(),
        class: "provider.process_stream".to_string(),
        message: "...the gate installs WordPress; a valid fix makes no error reappear".to_string(),
        source: "hydrated_process_stream".to_string(),
        data: Value::Null,
    };
    let controller = CollectedDiagnostic {
        task_id: "controller".to_string(),
        class: "internal.unexpected".to_string(),
        message: "gh pr failed: GraphQL: Something went wrong".to_string(),
        source: "controller_failure".to_string(),
        data: Value::Null,
    };
    let gate = gate_failure_diagnostic(&json!({
        "gate": "gate-1#1",
        "reason": "gate_failed",
        "exit_code": 1,
        "output_tail": "{\"pass\":false,\"blockers_added\":[\"2f15c116f68e:2\"]}\nacceptance_exit=1",
    }));

    for cause in [controller, gate] {
        let ranked = ranked_diagnostics(vec![prompt.clone(), cause.clone()]);
        assert_eq!(ranked[0].message, cause.message);
    }
    let ranked = ranked_diagnostics(vec![
        prompt.clone(),
        gate_failure_diagnostic(&json!({
            "reason": "gate_failed", "exit_code": 1, "output_tail": "{\"pass\":false}",
        })),
    ]);
    assert_eq!(ranked[0].class, "gate.gate_failed");
    assert_eq!(ranked[0].message, "gate_failed (exit 1)\n{\"pass\":false}");
}

#[test]
fn duplicate_diagnostics_merge_richer_denial_data() {
    let diagnostics = ranked_diagnostics(vec![
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "grep was denied".to_string(),
            source: "diagnostics".to_string(),
            data: json!({ "tool": "grep" }),
        },
        CollectedDiagnostic {
            task_id: "cook".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "grep was denied".to_string(),
            source: "diagnostics".to_string(),
            data: json!({
                "permission": "external_directory_read",
                "canonical_path": "/worktree/homeboy@fix-11827"
            }),
        },
    ]);

    assert_eq!(diagnostics.len(), 1);
    let value = collected_diagnostic_value(diagnostics[0].clone());
    assert_eq!(value["details"]["tool"], "grep");
    assert_eq!(value["details"]["permission"], "external_directory_read");
    assert_eq!(
        value["details"]["canonical_path"],
        "/worktree/homeboy@fix-11827"
    );
}

#[test]
fn duplicate_diagnostics_from_different_tasks_remain_distinct() {
    let diagnostics = ranked_diagnostics(vec![
        CollectedDiagnostic {
            task_id: "task-a".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "grep was denied".to_string(),
            source: "diagnostics".to_string(),
            data: json!({ "tool": "grep" }),
        },
        CollectedDiagnostic {
            task_id: "task-b".to_string(),
            class: "agent_tool.command_denied".to_string(),
            message: "grep was denied".to_string(),
            source: "diagnostics".to_string(),
            data: json!({ "canonical_path": "/other-worktree" }),
        },
    ]);

    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].task_id, "task-a");
    assert_eq!(diagnostics[1].task_id, "task-b");
}

#[test]
fn evidence_diagnostic_collection_stops_at_the_global_ref_budget() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let mut diagnostics = Vec::new();
    let mut budget = DiagnosticCollectionBudget::default();
    let mut truncations = Vec::new();

    for index in 0..=DIAGNOSTIC_EVIDENCE_REF_LIMIT {
        let path = directory.path().join(format!("evidence-{index}.json"));
        std::fs::write(
            &path,
            json!({ "diagnostics": [{ "class": "provider", "message": format!("failure-{index}") }] }).to_string(),
        )
        .expect("write evidence");
        let evidence = AgentTaskEvidenceRef {
            kind: "executor-result".to_string(),
            uri: format!("file://{}", path.display()),
            label: None,
        };
        if let Some(truncation) =
            collect_hydrated_evidence_diagnostics("cook", &evidence, &mut diagnostics, &mut budget)
        {
            truncations.push(truncation);
        }
    }

    assert_eq!(diagnostics.len(), DIAGNOSTIC_EVIDENCE_REF_LIMIT);
    assert_eq!(truncations.len(), 1);
    assert_eq!(truncations[0]["truncation"]["reason"], "evidence_refs");
}

#[test]
fn policy_denial_details_are_bounded_for_default_status() {
    let value = collected_diagnostic_value(CollectedDiagnostic {
        task_id: "cook".to_string(),
        class: "agent_tool.command_denied".to_string(),
        message: "denied".to_string(),
        source: "diagnostics".to_string(),
        data: json!({ "canonical_path": "x".repeat(COMPACT_TEXT_LIMIT + 1) }),
    });

    assert!(
        value["details"].get("canonical_path").is_none(),
        "oversized diagnostic text is omitted atomically"
    );
}

/// The `message` field is the causal answer `diagnose` exists to produce
/// (#14679): unlike secondary `details` payloads, an oversized message
/// must be truncated, not dropped to `null`.
#[test]
fn oversized_diagnostic_messages_are_truncated_not_dropped() {
    let oversized = "x".repeat(COMPACT_TEXT_LIMIT + 100);
    let value = collected_diagnostic_value(CollectedDiagnostic {
        task_id: "cook".to_string(),
        class: "agent_task.committed_harvest_git_failed".to_string(),
        message: oversized.clone(),
        source: "diagnostics".to_string(),
        data: Value::Null,
    });

    let message = value["message"]
        .as_str()
        .expect("oversized message must remain a string, not null");
    assert!(message.chars().count() < oversized.chars().count());
    assert!(message.starts_with('x'));
    assert!(message.ends_with('…'));
}

#[test]
fn retry_action_is_typed_and_bound_to_the_controller() {
    let action = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));

    assert_eq!(
        action.command,
        "homeboy --placement local agent-task retry run-1 --run"
    );
    assert_eq!(
        action.action.as_ref().unwrap().id,
        "agent-task.retry.owner-bound.v1"
    );
    assert_eq!(
        action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["placement"],
        "local"
    );
}

#[test]
fn generic_lab_replay_status_advertises_only_the_lab_retry_action() {
    let action = lab_replay_retry_action("run-1", json!({ "placement": "lab" }));

    assert_eq!(
        action.command,
        "homeboy --placement lab agent-task retry run-1 --run"
    );
    assert_eq!(
        action.action.as_ref().unwrap().id,
        "agent-task.retry.lab-replay.v1"
    );
    assert_eq!(
        action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["placement"],
        "lab"
    );
}

#[test]
fn diagnose_commands_preserve_the_controller_owner_placement() {
    let record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1",
        "run_id": "controller-run",
        "plan_id": "plan",
        "state": "failed",
        "submitted_at": "2026-08-05T00:00:00Z",
        "plan_path": "plan.json",
        "metadata": {}
    }))
    .expect("minimal durable record");

    let commands = diagnose_next_commands(&record, None, None, false, false);

    assert!(commands
        .iter()
        .all(|command| command.starts_with("homeboy --placement local ")));
}

#[test]
fn retry_action_is_typed_and_bound_to_its_runner() {
    let action = owner_bound_retry_action(
        "run-1",
        Some("lab-a"),
        json!({ "placement": "runner", "runner_id": "lab-a" }),
    );

    assert_eq!(
        action.command,
        "homeboy --runner lab-a agent-task retry run-1 --run"
    );
    assert_eq!(
        action.action.as_ref().unwrap().evidence.as_ref().unwrap()["owner"]["runner_id"],
        "lab-a"
    );
}

#[test]
fn unavailable_retry_has_a_reason_and_no_executable_action() {
    let retry = RetryReplayAction::unavailable(
        json!({ "placement": "runner", "runner_id": "lab-a" }),
        "source workspace no longer matches the persisted Lab replay identity",
    );

    assert_eq!(retry.projection()["readiness"], "unavailable");
    assert!(retry.projection()["action"].is_null());
    assert!(retry.projection()["reason"]
        .as_str()
        .unwrap()
        .contains("no longer matches"));
    assert_eq!(retry.projection()["admission"]["admitted"], false);
}

#[test]
fn cook_continuation_replaces_generic_retry_in_diagnose_commands() {
    let record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1",
        "run_id": "cook-attempt-timeout",
        "plan_id": "plan",
        "state": "partial_failure",
        "submitted_at": "2026-08-08T00:00:00Z",
        "plan_path": "plan.json",
        "metadata": {}
    }))
    .expect("minimal durable record");
    let retry = RetryReplayAction {
        owner: json!({ "placement": "local" }),
        readiness: "unavailable",
        reason: Some(
            "the authenticated review-form continuation must resume through Cook".to_string(),
        ),
        admission: json!({ "admitted": false }),
        action: None,
        continuation: Some(
            CommandNextAction::new(
                "recover the authenticated Cook",
                "homeboy agent-task recover cook-attempt-timeout",
            )
            .with_kind(CommandNextActionKind::Repair),
        ),
    };

    let commands = diagnose_next_commands(
        &record,
        retry.action.as_ref(),
        retry.continuation.as_ref(),
        false,
        false,
    );

    assert_eq!(retry.projection()["readiness"], "unavailable");
    assert!(retry.projection()["action"].is_null());
    assert!(commands.contains(&"homeboy agent-task recover cook-attempt-timeout".to_string()));
    assert!(commands
        .iter()
        .all(|command| !command.contains("agent-task retry")));
}

#[test]
fn cook_diagnose_uses_shared_runner_prefixed_command() {
    let step = CookRecoveryStep::Adopt {
        candidate_ref: "sha".to_string(),
    };
    let action = cook_diagnose_next_action(&step, "homeboy --runner lab-a", "cook-1");
    assert_eq!(
        action.command,
        step.next_action("homeboy --runner lab-a", "cook-1").command
    );
    assert!(action
        .command
        .starts_with("homeboy --runner lab-a agent-task recover "));
}

#[test]
fn compact_cook_report_is_bounded_and_full_remains_retrievable() {
    let attempts = (0..(COMPACT_TASK_LIMIT + 3))
        .map(|attempt| {
            json!({
                "attempt": attempt,
                "run_id": format!("run-{attempt}"),
                "run_state": "failed",
                "promotion": { "nested_evidence": "x".repeat(COMPACT_TEXT_LIMIT + 1) }
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-1",
        "latest_run_id": "run-14",
        "status": "failed",
        "stop_reason": "x".repeat(COMPACT_TEXT_LIMIT + 1),
        "attempts": attempts,
        "finalization": { "status": "blocked", "nested_evidence": { "large": "x".repeat(COMPACT_TEXT_LIMIT + 1) } },
        "selected_candidate": { "latest_attempt_run_id": "run-14", "run_id": "run-12", "selected_task_id": "task-12", "selected_candidate_sha": "patch-12", "reason": "canonical", "skipped_newer_attempts": [{"run_id":"run-14","reason":"malformed"}] }
    });

    let compact = compact_cook_report(report.clone(), false);
    assert_eq!(compact["schema"], report["schema"]);
    assert_eq!(compact["cook_id"], "cook-1");
    assert_eq!(
        compact["attempts"].as_array().unwrap().len(),
        COMPACT_TASK_LIMIT
    );
    assert_eq!(compact["attempts_omitted"], 3);
    assert!(compact["attempts"][0].get("promotion").is_none());
    assert_eq!(compact["selected_candidate"]["run_id"], "run-12");
    assert_eq!(
        compact["selected_candidate"]["selected_candidate_sha"],
        "patch-12"
    );
    assert_eq!(compact["full_command"], "homeboy agent-task status run-14");
    assert!(
        compact.get("failure_context").is_none(),
        "a report without failure_context must not grow one"
    );
    let mut full = compact_cook_report(report.clone(), true);
    assert_eq!(full["cook_summary"]["status"], "failed");
    full.as_object_mut().unwrap().remove("cook_summary");
    assert_eq!(full, report);
}

#[test]
fn compact_cook_failure_diagnostic_truncates_a_long_cause_instead_of_dropping_it() {
    let cause = format!(
        "Invalid argument 'promotion_provider.patch': could not apply promotion patch: {}",
        "error: patch does not apply\n".repeat(40)
    );
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-1",
        "latest_run_id": "run-1",
        "status": "durable_failure",
        "attempts": [],
        "failure_context": {
            "diagnostic": {
                "message": cause,
                "details": { "field": "promotion_provider.patch" },
                "deepest_cause": {
                    "code": "validation.invalid_argument",
                    "field": "promotion_provider.patch",
                    "message": cause,
                }
            }
        }
    });

    let diagnostic = &compact_cook_report(report, false)["failure_context"]["diagnostic"];

    assert_eq!(diagnostic["field"], "promotion_provider.patch");
    let message = diagnostic["message"]
        .as_str()
        .expect("a rejected promotion must say why");
    assert!(message.starts_with("Invalid argument 'promotion_provider.patch'"));
    assert!(message.chars().count() <= COMPACT_TEXT_LIMIT + 1);
}

#[test]
fn adopted_three_gate_finalization_projection_is_bounded_and_actionable_before_evidence() {
    let gate = json!({
        "name": "cargo test",
        "status": "passed",
        "evidence_refs": [{ "uri": "homeboy://evidence/three-gate-proof" }],
        "environment": { "PATH": "x".repeat(8 * 1024) },
        "proof": { "stdout": "x".repeat(8 * 1024) },
    });
    let report = json!({
        "schema": "homeboy/agent-task-finalization-report/v1",
        "run_id": "adopted-three-gate",
        "status": "review_ready",
        "pr_url": "https://github.com/Extra-Chill/homeboy/pull/422",
        "handoff": { "finalize_command": "homeboy agent-task recover adopted-three-gate" },
        "gates": [gate.clone(), gate.clone(), gate],
        "promotion": { "gates": (0..100).map(|_| json!({ "proof": "x".repeat(1024) })).collect::<Vec<_>>() },
    });

    let projected = bounded_full_operation_report(report, "finalize-pr");
    let serialized = serde_json::to_vec(&projected).expect("projection serializes");
    let early =
        std::str::from_utf8(&serialized[..serialized.len().min(1024)]).expect("json is utf8");

    assert!(serialized.len() <= BOUNDED_FULL_STATUS_BYTE_LIMIT);
    assert_eq!(
        projected["schema"],
        "homeboy/agent-task-finalization-report/v1"
    );
    assert_eq!(projected["actionable"]["terminal_state"], "review_ready");
    assert_eq!(
        projected["actionable"]["pr_url"],
        "https://github.com/Extra-Chill/homeboy/pull/422"
    );
    assert_eq!(
        projected["actionable"]["next_action"]["command"],
        "homeboy agent-task recover adopted-three-gate"
    );
    assert_eq!(projected["evidence_refs"].as_array().unwrap().len(), 1);
    assert_eq!(
        projected["evidence_refs"][0]["ref"],
        "homeboy://evidence/three-gate-proof"
    );
    assert!(early.contains("https://github.com/Extra-Chill/homeboy/pull/422"));
    assert!(early.contains("recover adopted-three-gate"));

    let oversized = bounded_full_operation_report(
        json!({
            "schema": "x".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2),
            "run_id": "r".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2),
            "status": "failed",
            "handoff": { "finalize_command": "x".repeat(BOUNDED_FULL_STATUS_BYTE_LIMIT * 2) },
        }),
        "finalize-pr",
    );
    assert!(serialized_len(&oversized) <= BOUNDED_FULL_STATUS_BYTE_LIMIT);
    assert_eq!(oversized["actionable"]["terminal_state"], "failed");
    assert_eq!(oversized["schema"], Value::Null);
}

/// #11113: `cook_failure_context` already computes the runnable recovery
/// commands, and the notification path forwards them. The default (compact)
/// view discarded the whole thing, so the pull channel was strictly poorer
/// than the push channel from the same computation.
#[test]
fn compact_cook_report_keeps_failure_context_actionable() {
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-11113",
        "latest_run_id": "run-2",
        "history_run_ids": ["run-1", "run-2"],
        "invocation_run_ids": ["run-2"],
        "status": "durable_failure",
        "attempts": [],
        "failure_context": {
            "cook_id": "cook-11113",
            "latest_run_id": "run-2",
            "selected_run_id": "run-1",
            "durable_recipe_ref": "homeboy://agent-task/cooks/cook-11113/recipe",
            "lifecycle_state": "Failed",
            "phase": "promotion",
            "reason_code": "operation_in_progress",
            "provider_budget_consumed": true,
            "provider_executions_consumed": 3,
            "recovery_legal": true,
            "recovery_reason": "y".repeat(COMPACT_TEXT_LIMIT + 1),
            "legal_actions": [
                { "action": "status", "command": "homeboy agent-task status run-2" },
                { "action": "diagnose", "command": "homeboy agent-task diagnose run-2" }
            ],
            "next_actions": [
                { "action": "status", "command": "homeboy agent-task status run-2" }
            ],
            "diagnostic": {
                "code": "promotion_rejected",
                "evidence": "private gate output",
                "deepest_cause": {
                    "code": "validation.invalid_argument",
                    "field": "promotion_provider.response",
                    "message": "provider rejected the request"
                }
            },
            "blocking_claim": { "state": "Running", "evidence": "private claim payload" }
        }
    });

    let compact = compact_cook_report(report.clone(), false);
    let context = &compact["failure_context"];

    assert_eq!(context["phase"], "promotion");
    assert_eq!(context["reason_code"], "operation_in_progress");
    assert_eq!(context["recovery_legal"], true);
    assert_eq!(context["provider_executions_consumed"], 3);
    assert_eq!(
        context["legal_actions"][1]["command"],
        "homeboy agent-task diagnose run-2"
    );
    assert_eq!(
        context["next_actions"][0]["command"],
        "homeboy agent-task status run-2"
    );
    assert_eq!(compact["invocation_run_ids"], json!(["run-2"]));

    // Private evidence stays behind --full; compact retains only the typed cause.
    assert_eq!(
        context["diagnostic"],
        json!({
            "code": "validation.invalid_argument",
            "field": "promotion_provider.response",
            "message": "provider rejected the request"
        })
    );
    assert!(
        context.get("blocking_claim").is_none(),
        "blocking_claim carries claim payload evidence"
    );

    assert!(
        context.get("recovery_reason").is_none(),
        "oversized prose is omitted atomically instead of splitting Unicode text"
    );
    assert_eq!(
        context["omitted_scalars"][0],
        json!({ "field": "recovery_reason", "bytes": COMPACT_TEXT_LIMIT + 1 })
    );

    assert_eq!(
        without_cook_summary(compact_cook_report(report.clone(), true)),
        report
    );
}

#[test]
fn compact_cook_report_bounds_recovery_action_samples_and_keeps_full_report() {
    let mut actions = (0..COMPACT_ACTION_LIMIT)
        .map(|index| {
            json!({
                "action": format!("recover-{index}"),
                "command": format!("homeboy agent-task recover run-1 --step={index}"),
                "private_evidence": "must only appear in --full"
            })
        })
        .collect::<Vec<_>>();
    actions.extend((0..20).map(|index| {
        json!({
            "action": format!("oversized-{index}"),
            "command": format!("homeboy agent-task recover run-1 --note={}", "x".repeat(COMPACT_ACTION_BYTE_LIMIT * 4)),
        })
    }));
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-action-budget",
        "latest_run_id": "run-1",
        "status": "durable_failure",
        "attempts": [],
        "failure_context": {
            "phase": "promotion",
            "reason_code": "operation_in_progress",
            "recovery_legal": true,
            "recovery_reason": "choose a recovery command",
            "legal_actions": actions,
            "next_actions": actions
        }
    });

    let compact = compact_cook_report(report.clone(), false);
    let context = &compact["failure_context"];

    for field in ["legal_actions", "next_actions"] {
        assert_eq!(
            context[field].as_array().unwrap().len(),
            COMPACT_ACTION_LIMIT
        );
        assert_eq!(
            context[format!("{field}_omitted")],
            json!(20),
            "the compact view must disclose every omitted recovery action"
        );
        assert!(context[field][0].get("private_evidence").is_none());
        assert_eq!(
            context[field][0]["command"], "homeboy agent-task recover run-1 --step=0",
            "sampled commands remain exactly runnable"
        );
    }
    assert_eq!(
        context["next_action"]["command"],
        "homeboy agent-task recover run-1 --step=0"
    );
    assert!(
        serde_json::to_vec(&compact).unwrap().len() < 6 * 1024,
        "large recovery lists must stay within the compact terminal-output budget"
    );
    assert_eq!(
        without_cook_summary(compact_cook_report(report.clone(), true)),
        report
    );
}

#[test]
fn compact_cook_report_omits_oversized_unicode_action_without_splitting_it() {
    let command = format!("homeboy agent-task retry run-1 --note={}", "👩‍💻".repeat(100));
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-unicode",
        "latest_run_id": "run-1",
        "status": "durable_failure",
        "attempts": [],
        "failure_context": {
            "legal_actions": [{ "action": "retry", "command": command }],
            "next_actions": [{ "action": "retry", "command": command }]
        }
    });

    let compact = compact_cook_report(report.clone(), false);
    let context = &compact["failure_context"];

    assert!(context["legal_actions"].as_array().unwrap().is_empty());
    assert!(context["next_actions"].as_array().unwrap().is_empty());
    assert_eq!(context["legal_actions_omitted"], 1);
    assert_eq!(context["next_actions_omitted"], 1);
    assert!(context.get("next_action").is_none());
    assert!(serde_json::to_vec(&compact).unwrap().len() < 2 * 1024);
    assert_eq!(
        without_cook_summary(compact_cook_report(report.clone(), true)),
        report
    );
}

/// A failed Cook's compact output keeps its cause: an oversized candidate
/// projection is given up before `failure_context`, so the command result
/// names the controller error rather than "exited 1 without reporting a
/// failure cause".
#[test]
fn compact_status_budget_keeps_failure_cause_over_candidate_projection() {
    let large = "x".repeat(COMPACT_STATUS_BYTE_LIMIT);
    let compact = enforce_compact_status_budget(json!({
        "schema": "homeboy/agent-task-cook/v1",
        "view": "summary",
        "cook_id": "cook-cause",
        "status": "durable_failure",
        "durable_candidate": { "canonical_candidate": large },
        "attempts": [{ "run_id": "x".repeat(COMPACT_STATUS_BYTE_LIMIT / 4) }],
        "failure_context": {
            "phase": "controller",
            "reason_code": "validation.invalid_argument",
            "diagnostic": { "message": "baseline has no immutable candidate" }
        }
    }));

    assert!(serialized_len(&compact) <= COMPACT_STATUS_BYTE_LIMIT);
    assert_eq!(
        compact["failure_context"]["diagnostic"]["message"],
        "baseline has no immutable candidate"
    );
    assert!(compact.get("durable_candidate").is_none());
}

#[test]
fn compact_cook_report_enforces_final_byte_budget_after_all_projections() {
    let large = "x".repeat(COMPACT_STATUS_BYTE_LIMIT);
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-budget",
        "latest_run_id": "run-budget",
        "status": "durable_failure",
        "stop_reason": large,
        "terminal_phase": "promotion",
        "attempts": (0..COMPACT_TASK_LIMIT).map(|index| json!({ "attempt": index, "run_id": format!("run-{index}"), "run_state": large })).collect::<Vec<_>>(),
        "finalization": { "status": "blocked", "evidence": large },
        "selected_candidate": { "run_id": "run-budget", "reason": large },
        "failure_context": {
            "phase": "promotion",
            "reason_code": "gate_failed",
            "recovery_reason": large,
            "legal_actions": [{ "action": "status", "command": "homeboy agent-task status run-budget" }]
        },
        "provider": { "evidence": large },
        "remaining_phases": [large],
        "continuation_command": large
    });

    let compact = compact_cook_report(report, false);
    let omitted = compact["omitted_sections"]
        .as_array()
        .expect("omission metadata");

    assert!(serialized_len(&compact) <= COMPACT_STATUS_BYTE_LIMIT);
    assert_eq!(compact["schema"], "homeboy/agent-task-cook/v1");
    assert_eq!(compact["cook_id"], "cook-budget");
    assert_eq!(compact["status"], "durable_failure");
    assert_eq!(
        compact["full_command"],
        "homeboy agent-task status run-budget"
    );
    assert!(omitted.iter().any(|item| item["section"] == "provider"));
}

/// The launch output adds the shared `cook_summary` (#18817); the rest of a
/// full report is returned unchanged.
fn without_cook_summary(mut value: Value) -> Value {
    assert!(
        value["cook_summary"].is_object(),
        "launch output carries cook_summary"
    );
    value.as_object_mut().unwrap().remove("cook_summary");
    value
}
