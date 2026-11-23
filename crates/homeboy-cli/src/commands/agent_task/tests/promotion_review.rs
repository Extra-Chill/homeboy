//! Agent-task command promotion source resolution and review/loop reporting tests.

use super::support::*;

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn record_candidate(run_id: &str) {
    let store = test_lifecycle_store();
    let plan = agent_task_lifecycle::load_plan_in_store(&store, run_id).unwrap();
    let mut aggregate = store.read_aggregate(run_id).unwrap();
    aggregate.outcomes[0].candidate = Some(homeboy::agents::agent_task::AgentTaskCandidate {
        base_sha: "base".into(),
        candidate_sha: "patch-a".into(),
        tree_sha: "tree".into(),
    });
    agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate).unwrap();
}

fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
        .expect("lifecycle store")
}

#[test]
fn applied_promotion_resume_requires_explicit_gate_rerun() {
    let applied = json!({ "status": "applied" });
    let failed = json!({ "status": "gate_failed" });

    assert!(!homeboy::agents::agent_task_service::promotion_is_resumable(&applied, false));
    assert!(homeboy::agents::agent_task_service::promotion_is_resumable(
        &applied, true
    ));
    assert!(homeboy::agents::agent_task_service::promotion_is_resumable(
        &failed, false
    ));
}

#[test]
fn promotion_recipe_reference_keeps_private_gates_out_of_review_commands() {
    with_temp_home(|| {
        let run_id = "cook-retained-gates-attempt-1";
        let private_program = "printf 'private $TOKEN'\n";
        let gates = homeboy::agents::agent_task_gate::VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            private_verify: vec![private_program.to_string()],
            input_sources: vec![serde_json::from_value(json!({
                "visibility": "private",
                "source_kind": "file",
                "sha256": "sha256:private-fixture",
                "size_bytes": private_program.len(),
                "redaction_policy": "summary_only"
            }))
            .expect("private source provenance")],
            ..Default::default()
        };
        let options = homeboy::agents::agent_task_service::CookRequest {
            identity: homeboy::agents::agent_task_service::CookIdentity {
                cook_id: "cook-retained-gates".to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: test_plan(),
            },
            workspace: homeboy::agents::agent_task_service::CookWorkspace {
                to_worktree: "fixture@retained-gates".to_string(),
                source_worktree_path: None,
                task_base_sha: None,
                source_refs: Vec::new(),
            },
            provider_transport: homeboy::agents::agent_task_service::CookProviderTransport {
                provider_invocation: None,
                attempt_dispatcher: None,
            },
            gates: gates.clone(),
            retry_policy: homeboy::agents::agent_task_service::CookRetryPolicy { max_attempts: 1 },
            finalization: homeboy::agents::agent_task_service::CookFinalization {
                publish_incomplete_draft: false,
                tracker_note: true,
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                head: None,
                title: "Retained gates".to_string(),
                commit_message: "Retained gates".to_string(),
                protected_branches: Vec::new(),
            },
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        run_loaded_plan(test_plan(), Some(run_id), Arc::new(ApplyArtifactExecutor))
            .expect("run Cook attempt");
        record_candidate(run_id);
        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: Some("fixture@retained-gates".to_string()),
            provider_argv: Vec::new(),
        })
        .expect("review Cook attempt");
        let promotion_command = review_value["evidence"]["promotion_candidates"][0]["command"]
            .as_array()
            .expect("promotion command");
        assert!(promotion_command
            .iter()
            .any(|argument| argument == "--base"));
        assert!(promotion_command.iter().any(|argument| argument == "main"));
        assert!(promotion_command
            .iter()
            .any(|argument| argument == "--gates-from-cook-recipe"));
        assert!(!promotion_command.iter().any(|argument| {
            argument
                .as_str()
                .is_some_and(|argument| argument.contains(private_program))
        }));
    });
}

#[test]
fn promotion_resume_reference_keeps_private_gates_out_of_review_commands() {
    with_temp_home(|| {
        let run_id = "run-private-resume-gates";
        let private_program = "printf 'private token'";
        let gates = homeboy::agents::agent_task_gate::VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            private_verify: vec![private_program.to_string()],
            execution_policy:
                homeboy::agents::agent_task_gate::AgentTaskGateExecutionPolicy::ContinueAll,
            ..Default::default()
        };
        run_loaded_plan(test_plan(), Some(run_id), Arc::new(ApplyArtifactExecutor))
            .expect("run completed");
        record_candidate(run_id);
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            homeboy_agents::agent_task_service::set_cook_delivery_artifact_for_test(
                &mut record.metadata,
                "/promotion",
                json!({
                    "status": "gate_failed",
                    "source": { "task_id": "task-a" },
                    "candidate": { "base_sha": "base", "candidate_sha": "patch-a", "tree_sha": "tree" },
                    "target": { "worktree": "fixture@resume" },
                    "provenance": {
                        "resume_contract": {
                            "inputs": { "base_ref": "main" },
                            "gates": gates,
                        }
                    }
                }),
            );
        })
        .expect("durable resume contract");

        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: None,
            provider_argv: Vec::new(),
        })
        .expect("review");
        let serialized = serde_json::to_string(&review_value).expect("review JSON");
        assert!(!serialized.contains(private_program));
        let command = review_value["evidence"]["promotion_candidates"][0]["command"]
            .as_array()
            .expect("promotion command")
            .iter()
            .map(|argument| argument.as_str().expect("argument"))
            .collect::<Vec<_>>();
        assert!(command.contains(&"--gates-from-resume-contract"));
    });
}

#[test]
fn review_does_not_advertise_a_command_for_an_invalid_resume_gate_contract() {
    with_temp_home(|| {
        let run_id = "run-invalid-resume-gates";
        run_loaded_plan(test_plan(), Some(run_id), Arc::new(ApplyArtifactExecutor))
            .expect("run completed");
        record_candidate(run_id);
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            homeboy_agents::agent_task_service::set_cook_delivery_artifact_for_test(
                &mut record.metadata,
                "/promotion",
                json!({
                    "status": "gate_failed",
                    "source": { "task_id": "task-a" },
                    "candidate": { "base_sha": "base", "candidate_sha": "patch-a", "tree_sha": "tree" },
                    "target": { "worktree": "fixture@resume" },
                    "provenance": { "resume_contract": {
                        "inputs": { "base_ref": "main" },
                        "gates": { "gate_timeout_seconds": "not-a-number" }
                    }}
                }),
            );
        })
        .expect("invalid durable resume contract");

        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: None,
            provider_argv: Vec::new(),
        })
        .expect("review remains readable");
        let candidate = &review_value["evidence"]["promotion_candidates"][0];

        assert_eq!(candidate["ready"], false, "{candidate}");
        assert!(candidate["command"].is_null());
        assert_eq!(
            candidate["unavailable_reason"],
            "durable resume contract has an invalid gate policy"
        );
    });
}

#[test]
fn review_reports_queued_run_without_chat_state() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
            .expect("submitted");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-queued".to_string(),
            full: true,
            to_worktree: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/control-plane-run-review/v1");
        assert_eq!(value["run"], "run-review-queued");
        assert_eq!(value["resource"]["state"], "queued");
        assert!(value["evidence"]["aggregate_review"].is_null());
        assert!(!value["evidence"]["logs"].is_null());
        assert_eq!(value["evidence"]["read"]["mutated"], false);
    });
}

#[test]
fn review_reports_completed_aggregate_and_promotion_hints() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-completed"),
            Arc::new(ApplyArtifactExecutor),
        )
        .expect("run completed");
        record_candidate("run-review-completed");
        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-completed".to_string(),
            full: true,
            to_worktree: Some("homeboy-review-flow".to_string()),
            provider_argv: Vec::new(),
        })
        .expect("review loaded");
        assert_eq!(exit_code, 0);
        assert_eq!(value["resource"]["state"], "succeeded");
        assert_eq!(
            value["evidence"]["aggregate_review"]["summary"]["apply_candidates"],
            1
        );
        assert_eq!(
            value["evidence"]["artifacts"]["artifacts"][0]["id"],
            "patch-a"
        );
    });
}

#[test]
fn default_review_is_bounded_and_points_to_full_evidence() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-default-bounded"),
            Arc::new(ApplyArtifactExecutor),
        )
        .expect("run completed");
        record_candidate("run-review-default-bounded");
        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-default-bounded".to_string(),
            full: false,
            to_worktree: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");
        assert_eq!(exit_code, 0);
        assert_eq!(value["view"], "summary");
        assert!(value.get("evidence").is_none());
        assert_eq!(value["canonical_candidate"]["state"], "patch_available");
        assert_eq!(value["selected_candidate"]["size_bytes"], 42);
        assert!(value["promotion_candidates"][0]["command"].is_null());
        assert_eq!(
            value["promotion_candidates"][0]["destination_required"],
            true
        );
        assert!(value["next_actions"][0]
            .as_str()
            .expect("destination guidance")
            .contains("--to-worktree <managed-worktree>"));
        assert_eq!(
            value["full_command"],
            "homeboy agent-task review run-review-default-bounded --full"
        );
    });
}

#[test]
fn cook_readers_keep_the_substantive_candidate_after_a_no_change_retry() {
    struct PatchExecutor {
        run_id: String,
    }

    struct NoChangeReviewExecutor;

    impl AgentTaskExecutorAdapter for NoChangeReviewExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("intentional no-change gate retry".to_string()),
                outputs: json!({
                    "review_form": {
                        "summary": "Retry reviewed the candidate.",
                        "what_changed": ["No additional patch was needed."],
                        "compatibility": "No additional compatibility impact.",
                        "used_for": "Reviewed failed-gate evidence."
                    }
                }),
                ..Default::default()
            }
        }
    }

    impl AgentTaskExecutorAdapter for PatchExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            std::fs::write(
                std::path::Path::new(request.workspace.root.as_deref().unwrap())
                    .join("candidate.txt"),
                "new\n",
            )
            .unwrap();
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("produced candidate before failed gate".into()),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "plan".to_string(),
                    uri: format!("homeboy://agent-task/run/{}/plan#task=task-a", self.run_id),
                    label: None,
                }],
                ..Default::default()
            }
        }
    }

    with_temp_home(|| {
        let cook_id = "cook-reader-substantive-candidate";
        let candidate_run_id = "cook-reader-substantive-candidate-attempt-1";
        let retry_run_id = "cook-reader-substantive-candidate-attempt-2";
        let repo = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.test"],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(repo.path().join("candidate.txt"), "old\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-qm", "base"]] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
        }
        let mut plan = test_plan();
        plan.tasks[0].workspace.root = Some(repo.path().display().to_string());
        run_loaded_plan(
            plan,
            Some(candidate_run_id),
            Arc::new(PatchExecutor {
                run_id: candidate_run_id.into(),
            }),
        )
        .expect("substantive candidate completed");
        run_loaded_plan(
            test_plan(),
            Some(retry_run_id),
            Arc::new(NoChangeReviewExecutor),
        )
        .expect("intentional no-change retry completed");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            1,
            candidate_run_id,
        )
        .expect("record substantive candidate");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            2,
            retry_run_id,
        )
        .expect("record latest no-change retry");
        agent_task_lifecycle::record_promotion(
            retry_run_id,
            json!({
                "status": "gate_failed",
                "gate_results": [{ "name": "cargo test", "exit_code": 1 }],
                "provenance": {
                    "gate_retry": "intentional_no_change",
                    "private_verify": ["printf private-contributing-gate-marker"]
                }
            }),
        )
        .expect("record retry verification provenance");

        let (status_value, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
            ..Default::default()
        })
        .expect("Cook status is bounded");
        let (review_value, _) = review::review(ReviewArgs {
            run_id: cook_id.to_string(),
            full: true,
            to_worktree: None,
            provider_argv: Vec::new(),
        })
        .expect("Cook review is bounded");
        let store = test_lifecycle_store();
        let rows_before = [candidate_run_id, retry_run_id]
            .map(|run| store.read_record(run).expect("row before review"));
        let index_before =
            serde_json::to_value(store.read_cook_index(cook_id).expect("Cook index"))
                .expect("index value");
        let direct = homeboy::agents::orchestration::review_from_current_environment(
            cook_id,
            &homeboy_control_plane_contract::ControlPlaneRunReviewRequest::default(),
        )
        .expect("canonical alias review");
        homeboy::agents::orchestration::register();
        let http = homeboy::core::http_api::handle(homeboy::core::http_api::HttpApiRequest {
            method: homeboy::core::http_api::HttpMethod::Get,
            path: format!("/v1/control-plane/runs/{cook_id}/review"),
            body: None,
        })
        .expect("HTTP alias review");
        assert_eq!(http.status, 200);
        assert_eq!(
            serde_json::to_value(direct).expect("direct review value"),
            review_value
        );
        assert_eq!(http.body["resource"], review_value);
        assert_eq!(
            review_value["evidence"]["contributing_attempt"]["verification"]["provenance"]
                ["private_verify"],
            json!(["[private]"])
        );
        assert!(!serde_json::to_string(&review_value)
            .expect("review JSON")
            .contains("private-contributing-gate-marker"));
        assert_eq!(review_value["run"], candidate_run_id);
        assert_eq!(review_value["resource"]["state"], status_value["state"]);
        assert_eq!(
            review_value["evidence"]["record"]["run_id"],
            candidate_run_id
        );
        assert_eq!(
            review_value["evidence"]["artifacts"]["run_id"],
            candidate_run_id
        );
        assert_eq!(
            review_value["evidence"]["retry_context"]["run_id"],
            candidate_run_id
        );
        assert_eq!(
            [candidate_run_id, retry_run_id]
                .map(|run| store.read_record(run).expect("row after review")),
            rows_before
        );
        assert_eq!(
            serde_json::to_value(store.read_cook_index(cook_id).expect("Cook index after"))
                .expect("index value after"),
            index_before
        );
        let (diagnose_value, _) = diagnose(DiagnoseArgs {
            run_id: cook_id.to_string(),
            full: false,
        })
        .expect("Cook diagnosis is bounded");
        let (evidence_value, _) = evidence(EvidenceArgs {
            run_id: cook_id.to_string(),
            kind: None,
            task: None,
            failure_only: true,
            full: false,
        })
        .expect("Cook failure evidence is bounded");
        let (all_evidence_value, _) = evidence(EvidenceArgs {
            run_id: cook_id.to_string(),
            kind: None,
            task: None,
            failure_only: false,
            full: false,
        })
        .expect("Cook evidence reads the selected candidate plan");

        assert_eq!(status_value["run"], candidate_run_id);
        assert_eq!(review_value["resource"]["run"], candidate_run_id);
        assert_eq!(
            review_value["evidence"]["candidate_selection"]["latest_attempt_run_id"],
            retry_run_id
        );
        assert_eq!(
            review_value["evidence"]["candidate_selection"]["run_id"],
            candidate_run_id
        );
        for value in [&diagnose_value, &evidence_value] {
            assert_eq!(value["run_id"], candidate_run_id);
            assert_eq!(
                value["candidate_selection"]["latest_attempt_run_id"],
                retry_run_id
            );
            assert_eq!(value["candidate_selection"]["run_id"], candidate_run_id);
        }
        assert_eq!(
            review_value["evidence"]["contributing_attempt"]["run_id"],
            retry_run_id
        );
        assert_eq!(
            review_value["evidence"]["contributing_attempt"]["review_form"]["summary"],
            "Retry reviewed the candidate."
        );
        assert_eq!(
            review_value["evidence"]["contributing_attempt"]["verification"]["provenance"]
                ["gate_retry"],
            "intentional_no_change"
        );
        assert_eq!(
            all_evidence_value["evidence"][0]["content"]["value"]["task_id"], "task-a",
            "{all_evidence_value:#}"
        );

        let (canonical_value, _) = status(StatusArgs {
            run_id: cook_id.to_string(),
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
            ..Default::default()
        })
        .expect("Cook status selects the candidate");
        assert_eq!(canonical_value["run"], candidate_run_id);

        let (attempt_status, _) = status(StatusArgs {
            run_id: retry_run_id.to_string(),
            exact: true,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
            ..Default::default()
        })
        .expect("exact attempt remains directly addressable");
        assert_eq!(attempt_status["run"], retry_run_id);
    });
}
