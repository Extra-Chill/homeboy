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
fn evidence_command_hydrates_homeboy_and_file_refs_with_filters_and_redaction() {
    with_isolated_home(|home| {
        let file_path = home.path().join("executor-result.json");
        std::fs::write(
            &file_path,
            r#"{"message":"failed","api_key":"super-secret","details":"useful"}"#,
        )
        .expect("write evidence file");
        let run_id = "run-cli-evidence";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            EvidenceFixtureExecutor {
                run_id: run_id.to_string(),
                file_uri: format!("file://{}", file_path.display()),
            },
        )
        .expect("run completed");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
        })
        .expect("evidence loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-evidence/v1");
        assert_eq!(value["count"], 4);
        let entries = value["evidence"].as_array().expect("evidence array");
        let file_entry = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-result")
            .expect("file evidence");
        assert_eq!(file_entry["source"], "file");
        assert_eq!(file_entry["content"]["format"], "json");
        assert_eq!(file_entry["content"]["value"]["api_key"], "[REDACTED]");
        assert_eq!(file_entry["content"]["value"]["details"], "useful");
        let aggregate_entry = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-normalized-output")
            .expect("homeboy evidence");
        assert_eq!(aggregate_entry["source"], "homeboy");
        assert_eq!(aggregate_entry["content"]["status"], "failed");
    });
}

#[test]
fn diagnose_hydrates_executor_result_evidence_root_cause() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "status": "provider_error",
                "diagnostics": [
                    {
                        "class": "runtime.required_typed_artifacts_missing",
                        "message": "Agent runtime did not produce required typed artifacts: concept_packet, design_packet."
                    },
                    {
                        "class": "agent_runtime.task_run_failed",
                        "message": "RecipeValidationError: configured provider runtime path does not exist"
                    }
                ],
                "command": "agent-runtime task run",
                "exit_code": 1,
                "stderr": "ability unavailable\nsecret=raw-secret"
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        run_loaded_plan(
            test_plan(),
            Some("run-cli-diagnose-evidence"),
            ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            },
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: "run-cli-diagnose-evidence".to_string(),
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-diagnose/v1");
        assert_eq!(
            value["root_cause"]["class"],
            "agent_runtime.task_run_failed"
        );
        assert_eq!(
            value["root_cause"]["message"],
            "RecipeValidationError: configured provider runtime path does not exist"
        );
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["command"],
            "agent-runtime task run"
        );
        assert_eq!(value["hydrated_evidence"][0]["summary"]["exit_code"], 1);
        assert!(value["hydrated_evidence"][0]["summary"]["stderr_excerpt"]
            .as_str()
            .expect("stderr excerpt")
            .contains("[REDACTED]"));
        assert_eq!(
            value["next_commands"][0],
            "homeboy agent-task status run-cli-diagnose-evidence --full"
        );

        let (status_value, status_exit_code) = status(StatusArgs {
            run_id: "run-cli-diagnose-evidence".to_string(),
            bridge: false,
            since_cursor: None,
            full: true,
        })
        .expect("status loaded");
        assert_eq!(status_exit_code, 0);
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "agent_runtime.task_run_failed"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["message"],
            "RecipeValidationError: configured provider runtime path does not exist"
        );
    });
}

#[test]
fn replay_provider_boundary_projects_latest_executor_input() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-input.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "task_id": "task-a",
                "executor": {
                    "backend": "codebox",
                    "config": {
                        "runtime_component_paths": {
                            "agent_runtime": "/runner/data-machine-patched"
                        },
                        "runtime_env": {
                            "WP_CODEBOX_DATA_MACHINE_PATH": "/runner/data-machine-patched"
                        }
                    }
                },
                "inputs": {
                    "runtime_task": {
                        "ability": "runtime-package/run",
                        "input": {
                            "package": {
                                "source": "data-machine"
                            }
                        }
                    }
                },
                "artifact_declarations": [
                    { "name": "runtime-package", "required": true }
                ]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        run_loaded_plan(
            test_plan(),
            Some("run-cli-provider-boundary-replay"),
            ExecutorInputEvidenceExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            },
        )
        .expect("run completed");

        let (value, exit_code) = replay_provider_boundary(ReplayProviderBoundaryArgs {
            run_id: "run-cli-provider-boundary-replay".to_string(),
            task: Some("task-a".to_string()),
        })
        .expect("replay report");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["schema"],
            "homeboy/agent-task-provider-boundary-replay/v1"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_task"]["ability"],
            "runtime-package/run"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_component_paths"]["agent_runtime"],
            "/runner/data-machine-patched"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_env"]["WP_CODEBOX_DATA_MACHINE_PATH"],
            "/runner/data-machine-patched"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["package_descriptor"]["source"],
            "data-machine"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["artifact_declarations"][0]["name"],
            "runtime-package"
        );
        assert_eq!(value["typed_evidence"]["kind"], "provider-boundary-replay");
    });
}

#[test]
fn generic_contract_fixtures_surface_runtime_import_before_missing_artifact() {
    with_temp_home(|| {
        let run_id = "run-contract-import-diagnostics";
        let outcome = fixture_outcome(
            "../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json",
        );

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            FixtureOutcomeExecutor { outcome },
        )
        .expect("run completed with fixture outcome");

        let (diagnose_value, diagnose_exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
        })
        .expect("diagnose loaded");
        assert_eq!(diagnose_exit_code, 0);
        assert_eq!(
            diagnose_value["root_cause"]["class"],
            "runtime.import_failed"
        );
        assert_eq!(
            diagnose_value["root_cause"]["message"],
            "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
        );
        assert_eq!(
            diagnose_value["missing_artifacts"][0]["missing"],
            json!(["answer_packet"])
        );

        let (status_value, status_exit_code) = status(StatusArgs {
            run_id: run_id.to_string(),
            bridge: false,
            since_cursor: None,
            full: true,
        })
        .expect("status loaded");
        assert_eq!(status_exit_code, 0);
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "runtime.import_failed"
        );
        assert_eq!(
            status_value["failure_reasons"][0]["message"],
            "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
        );
    });
}

#[test]
fn generic_contract_fixtures_hydrate_local_file_and_path_evidence() {
    with_isolated_home(|home| {
        let structured_path = home.path().join("structured-result.json");
        let log_path = home.path().join("runtime.log");
        std::fs::write(
            &structured_path,
            serde_json::to_string(&json!({
                "status": "provider_error",
                "diagnostics": [{
                    "class": "runtime.import_failed",
                    "message": "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
                }],
                "access_token": "secret-token"
            }))
            .expect("structured evidence json"),
        )
        .expect("write structured evidence");
        std::fs::write(&log_path, "runtime import failed").expect("write log evidence");

        let raw = include_str!(
            "../../../../tests/fixtures/agent_task_contract/local_file_evidence_refs.json"
        )
        .replace(
            "__LOCAL_FILE_URI__",
            &format!("file://{}", structured_path.display()),
        )
        .replace("__LOCAL_PATH__", &log_path.display().to_string());
        let outcome: AgentTaskOutcome = serde_json::from_str(&raw).expect("fixture outcome");
        let run_id = "run-contract-local-evidence";

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            FixtureOutcomeExecutor { outcome },
        )
        .expect("run completed with fixture outcome");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
        })
        .expect("evidence loaded");
        assert_eq!(exit_code, 0);
        let entries = value["evidence"].as_array().expect("evidence entries");
        let structured = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-result")
            .expect("structured evidence");
        assert_eq!(structured["source"], "file");
        assert_eq!(
            structured["content"]["value"]["diagnostics"][0]["class"],
            "runtime.import_failed"
        );
        assert_eq!(structured["content"]["value"]["access_token"], "[REDACTED]");

        let plain = entries
            .iter()
            .find(|entry| entry["kind"] == "runtime-log")
            .expect("plain path evidence");
        assert_eq!(plain["source"], "file");
        assert_eq!(plain["content"]["text"], "runtime import failed");
    });
}

#[test]
fn generic_contract_fixtures_accept_successful_required_artifact_handoff() {
    let outcome = fixture_outcome(
        "../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json",
    );

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome
        .typed_artifacts
        .iter()
        .any(|artifact| artifact.name == "answer_packet"));
    assert!(outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.metadata["handoff_schema"]
            == "homeboy/agent-task-artifact-handoff/v1"));
}

fn fixture_outcome(relative_path: &str) -> AgentTaskOutcome {
    let raw = match relative_path {
        "../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json" => include_str!("../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json"),
        "../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json" => include_str!("../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json"),
        "../../../../tests/fixtures/agent_task_contract/missing_required_artifact.json" => include_str!("../../../../tests/fixtures/agent_task_contract/missing_required_artifact.json"),
        _ => panic!("unknown fixture {relative_path}"),
    };
    serde_json::from_str(raw).expect("fixture outcome")
}

struct FixtureOutcomeExecutor {
    outcome: AgentTaskOutcome,
}

impl AgentTaskExecutorAdapter for FixtureOutcomeExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = self.outcome.clone();
        outcome.task_id = request.task_id;
        outcome
    }
}

#[test]
fn evidence_command_hydrates_plain_local_path_refs_and_summarizes_unsupported_refs() {
    with_isolated_home(|home| {
        let file_path = home.path().join("plain-evidence.txt");
        std::fs::write(&file_path, "plain local evidence").expect("write evidence file");
        let run_id = "run-cli-evidence-local-path";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            EvidencePathFixtureExecutor {
                local_path: file_path.display().to_string(),
                unsupported_uri: "provider-result://opaque/ref".to_string(),
            },
        )
        .expect("run completed");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
        })
        .expect("evidence loaded");

        assert_eq!(exit_code, 0);
        assert!(value["count"].as_u64().expect("evidence count") >= 2);
        let entries = value["evidence"].as_array().expect("evidence array");
        let path_entry = entries
            .iter()
            .find(|entry| entry["uri"] == file_path.display().to_string())
            .expect("local path evidence");
        assert_eq!(path_entry["source"], "file");
        assert_eq!(path_entry["content"]["text"], "plain local evidence");

        let unsupported = entries
            .iter()
            .find(|entry| entry["source"] == "unsupported")
            .expect("unsupported evidence");
        assert_eq!(unsupported["status"], "ok");
        assert_eq!(
            unsupported["content"]["unsupported_ref"],
            "provider-result://opaque/ref"
        );
        assert!(unsupported["content"]["next_action"].is_string());
    });
}

struct EvidencePathFixtureExecutor {
    local_path: String,
    unsupported_uri: String,
}

impl AgentTaskExecutorAdapter for EvidencePathFixtureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("failed with path evidence".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.local_path.clone(),
                    label: Some("Plain path".to_string()),
                },
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.unsupported_uri.clone(),
                    label: Some("Unsupported ref".to_string()),
                },
            ],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

#[test]
fn evidence_command_truncates_large_file_evidence() {
    with_isolated_home(|home| {
        let file_path = home.path().join("large.log");
        std::fs::write(&file_path, "x".repeat(20 * 1024)).expect("write evidence file");
        let run_id = "run-cli-evidence-truncated";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            EvidenceFixtureExecutor {
                run_id: run_id.to_string(),
                file_uri: format!("file://{}", file_path.display()),
            },
        )
        .expect("run completed");

        let (value, _) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: Some("executor-result".to_string()),
            task: Some("task-a".to_string()),
            failure_only: false,
        })
        .expect("evidence loaded");

        assert_eq!(value["count"], 1);
        assert_eq!(value["evidence"][0]["truncated"], true);
        assert_eq!(value["evidence"][0]["bytes_read"], 16 * 1024);
        assert_eq!(value["evidence"][0]["omitted_bytes"], 4 * 1024);
    });
}

struct EvidenceFixtureExecutor {
    run_id: String,
    file_uri: String,
}

impl AgentTaskExecutorAdapter for EvidenceFixtureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("failed with evidence".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.file_uri.clone(),
                    label: Some("Executor result".to_string()),
                },
                AgentTaskEvidenceRef {
                    kind: "executor-normalized-output".to_string(),
                    uri: format!(
                        "homeboy://agent-task/run/{}/aggregate#outcome={}",
                        self.run_id, request.task_id
                    ),
                    label: Some("Normalized output".to_string()),
                },
            ],
            diagnostics: Vec::new(),
            outputs: json!({ "api_key": "super-secret", "result": "failed" }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
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
