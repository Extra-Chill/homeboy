//! Agent-task command run submission, status, run-next, cancel, retry, and resume tests.

use super::support::*;

#[test]
fn submit_run_status_reports_terminal_state() {
    with_temp_home(|| {
        let plan = AgentTaskPlan::new(
            "plan-cli-terminal",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-cli-terminal".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "missing-provider-test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "exercise durable terminal status".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                metadata: Value::Null,
            }],
        );
        let plan_file = tempfile::NamedTempFile::new().expect("plan file");
        std::fs::write(
            plan_file.path(),
            serde_json::to_string(&plan).expect("plan json"),
        )
        .expect("write plan");
        let plan_path = format!("@{}", plan_file.path().display());

        submit(SubmitArgs {
            plan: plan_path,
            run_id: Some("run-cli-terminal".to_string()),
        })
        .expect("submitted");
        let (_, run_exit_code) = run_submitted(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            bridge: false,
            since_cursor: None,
            full: false,
        })
        .expect("run completed");
        let (status_json, status_exit_code) = status(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            bridge: false,
            since_cursor: None,
            full: true,
        })
        .expect("status loaded");
        let (bridge_status_json, bridge_status_exit_code) = status(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            bridge: true,
            since_cursor: Some(0),
            full: false,
        })
        .expect("bridge status loaded");
        let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

        assert_eq!(run_exit_code, 1);
        assert_eq!(status_exit_code, 0);
        assert_eq!(bridge_status_exit_code, 0);
        assert_eq!(
            bridge_status_json["schema"],
            "homeboy/agent-task-run-status/v1"
        );
        assert!(bridge_status_json["normalized_events"].is_array());
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(record.totals.expect("totals").failed, 1);
    });
}

#[test]
fn failed_run_status_logs_and_review_include_outcome_diagnostic_summary() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnostic-summary";
        run_loaded_plan(test_plan(), Some(run_id), DiagnosticFailureExecutor)
            .expect("run completed with failed outcome");

        let (status_value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            bridge: false,
            since_cursor: None,
            full: false,
        })
        .expect("status loaded");
        let (logs_value, _) = logs(StatusArgs {
            run_id: run_id.to_string(),
            bridge: false,
            since_cursor: None,
            full: false,
        })
        .expect("logs loaded");
        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            to_worktree: None,
            provider_command: None,
        })
        .expect("review loaded");

        for value in [&status_value, &logs_value, &review_value] {
            assert_eq!(
                value["diagnostic_summary"]["message"],
                "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
            );
            assert_eq!(value["diagnostic_summary"]["class"], "provider_discovery");
            assert_eq!(value["diagnostic_summary"]["task_id"], "task-a");
        }
    });
}

#[test]
fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
    with_temp_home(|| {
        let run_id = "run-plan-durable";
        let observed_status = Arc::new(Mutex::new(None));
        let executor = InspectingExecutor {
            run_id: run_id.to_string(),
            observed_status: Arc::clone(&observed_status),
        };

        let (_value, exit_code) =
            run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed durable status");
        assert_eq!(exit_code, 0);
        assert_eq!(observed.state, AgentTaskRunState::Running);
        assert_eq!(observed.tasks[0].state, AgentTaskState::Running);
        assert_eq!(observed.metadata["runner_pid"], std::process::id());
        assert!(observed.aggregate_path.is_none());

        let completed = lifecycle_status(run_id).expect("completed status loaded");
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert!(completed.aggregate_path.is_some());
    });
}

#[test]
fn run_plan_fails_fast_when_required_secret_env_is_missing() {
    with_temp_home(|| {
        let missing_secret = "HOMEBOY_AGENT_TASK_MISSING_PROVIDER_SECRET_TEST";
        std::env::remove_var(missing_secret);
        let mut plan = test_plan();
        plan.tasks[0].executor.secret_env = vec![missing_secret.to_string()];
        let executor = CapturingExecutor::default();
        let observed_request = Arc::clone(&executor.observed_request);

        let error = run_loaded_plan(plan, Some("run-plan-missing-secret"), executor)
            .expect_err("missing secret should fail before executor dispatch");

        assert!(observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
        assert_eq!(error.details["field"], "secret_env");
        assert!(error.to_string().contains(missing_secret));
        assert!(error.details["tried"]
            .as_array()
            .expect("remediation hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("runner-required secret env contracts"))));
        assert!(!error.to_string().contains("secret-value"));
        assert!(lifecycle_status("run-plan-missing-secret").is_err());
    });
}

#[test]
fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
            .expect("first submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
            .expect("second submitted");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_next_with_executor(InspectingExecutor {
            run_id: "run-next-a".to_string(),
            observed_status: Arc::clone(&observed_status),
        })
        .expect("claimed run completed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed claimed status");
        let first = lifecycle_status("run-next-a").expect("first status");
        let second = lifecycle_status("run-next-b").expect("second status");

        assert_eq!(exit_code, 0);
        assert_eq!(observed.state, AgentTaskRunState::Running);
        assert_eq!(first.state, AgentTaskRunState::Succeeded);
        assert_eq!(second.state, AgentTaskRunState::Queued);
    });
}

#[test]
fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
    with_temp_home(|| {
        let (value, exit_code) = run_next_with_executor(InspectingExecutor {
            run_id: "unused".to_string(),
            observed_status: Arc::new(Mutex::new(None)),
        })
        .expect("run-next checked queue");

        assert_eq!(exit_code, 0);
        assert_eq!(value["claimed"], false);
    });
}

#[test]
fn cancel_command_marks_queued_run_cancelled() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-cli-cancel")).expect("submitted");

        let (value, exit_code) = cancel(CancelArgs {
            run_id: "run-cli-cancel".to_string(),
            reason: Some("not selected".to_string()),
        })
        .expect("cancelled");
        let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

        assert_eq!(exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(record.metadata["cancel_reason"], json!("not selected"));
    });
}

#[test]
fn retry_command_submits_new_queued_run() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-retry-source"))
            .expect("submitted");

        let (value, exit_code) = retry(RetryArgs {
            run_id: "run-retry-source".to_string(),
            new_run_id: Some("run-retry-cli".to_string()),
            run: false,
        })
        .expect("retry queued");
        let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

        assert_eq!(exit_code, 0);
        assert_eq!(record.run_id, "run-retry-cli");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.metadata["retry_of"], json!("run-retry-source"));
    });
}

#[test]
fn resume_command_executes_existing_run() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-resume-cli")).expect("submitted");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_resume_with_executor(
            "run-resume-cli".to_string(),
            InspectingExecutor {
                run_id: "run-resume-cli".to_string(),
                observed_status: Arc::clone(&observed_status),
            },
        )
        .expect("resumed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed status");
        let completed = lifecycle_status("run-resume-cli").expect("completed status");

        assert_eq!(exit_code, 0);
        assert!(observed.metadata["resume_requested_at"].is_string());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
    });
}

#[test]
fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
    let observed_request = Arc::new(Mutex::new(None));
    let executor = CapturingExecutor {
        observed_request: Arc::clone(&observed_request),
    };
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("sample-agent-runtime".to_string());
    plan.tasks[0].workspace.branch = Some("fix/runtime-guidance".to_string());
    plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
    plan.tasks[0].workspace.task_url =
        Some("https://github.com/example/sample-agent-runtime/issues/179".to_string());
    plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
    plan.tasks[0].workspace.materialization = json!({
        "root": "/tmp/homeboy-worktrees/sample-component@fix-179-runtime-guidance"
    });

    let (_value, exit_code) = run_loaded_plan(plan, None, executor).expect("run-plan completed");
    let observed = observed_request
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("provider saw request");

    assert_eq!(exit_code, 0);
    assert_eq!(
        observed.workspace.mode,
        homeboy::core::agent_tasks::AgentTaskWorkspaceMode::Existing
    );
    assert_eq!(
        observed.workspace.root.as_deref(),
        Some("/tmp/homeboy-worktrees/sample-component@fix-179-runtime-guidance")
    );
    assert_eq!(
        observed.workspace.slug.as_deref(),
        Some("sample-agent-runtime")
    );
    assert!(observed.workspace.kind.is_none());
    assert!(observed.workspace.component_id.is_none());
    assert!(observed.workspace.branch.is_none());
    assert!(observed.workspace.base_ref.is_none());
    assert!(observed.workspace.task_url.is_none());
    assert!(observed.workspace.cleanup.is_none());
    assert!(observed.workspace.materialization.is_null());
}

#[test]
fn run_plan_rejects_component_worktree_without_branch() {
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("sample-agent-runtime".to_string());

    let error = run_loaded_plan(plan, None, CapturingExecutor::default())
        .expect_err("component worktree without branch rejected");
    let message = error.to_string();

    assert!(message.contains("workspace.branch"));
    assert!(message.contains("requires branch"));
}
